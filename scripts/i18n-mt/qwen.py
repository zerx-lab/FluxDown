"""阿里云百炼 qwen-mt 的 OpenAI 兼容接口客户端（仅标准库）。

qwen-mt 只接受一条 user 消息、不支持 system prompt；语种、术语、翻译记忆与领域提示
全部经 `translation_options` 传入。输入上限 8192 token，调用方按条 / 按段拆分。
"""

from __future__ import annotations

import json
import random
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from typing import Deque, Dict, List

from common import FatalApiError, ItemError, progress

DEFAULT_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1"
DEFAULT_MODEL = "qwen-mt-plus"
MAX_RETRIES = 3
BACKOFF_BASE_SECONDS = 2.0
# 429 限流:所有并发线程共享一个冷却窗口,命中后全体暂停再试,不计入普通重试次数。
MAX_RATE_LIMIT_WAITS = 12
RATE_LIMIT_BASE_SECONDS = 5.0
RATE_LIMIT_MAX_SECONDS = 60.0
# 百炼对 qwen-mt 静态限流(按主账号,超出任一即 429 limit_requests):RPM 一律 60,
# 且「可能按 RPS = RPM/60 限制」,所以请求必须匀速发出而不是一分钟内突发。TPM 按模型(北京地域):
# https://help.aliyun.com/zh/model-studio/rate-limit
DEFAULT_RPM = 60
DEFAULT_TPM = {"qwen-mt-plus": 25_000, "qwen-mt-flash": 35_000, "qwen-mt-lite": 100_000, "qwen-mt-turbo": 35_000}
FALLBACK_TPM = 25_000
WINDOW_SECONDS = 60.0


class QwenMt:
    def __init__(
        self,
        api_key: str,
        model: str = DEFAULT_MODEL,
        base_url: str = DEFAULT_BASE_URL,
        timeout: float = 120.0,
        rpm: int = 0,
        tpm: int = 0,
    ) -> None:
        self.api_key = api_key
        self.model = model
        self.endpoint = base_url.rstrip("/") + "/chat/completions"
        self.timeout = timeout
        # 留 10% 余量:服务端按滑动窗口计数,客户端时钟与之并不对齐。
        self.interval = WINDOW_SECONDS / max(1.0, (rpm or DEFAULT_RPM) * 0.9)
        self.tpm = int((tpm or DEFAULT_TPM.get(model, FALLBACK_TPM)) * 0.9)
        self._lock = threading.Lock()
        self._next_slot = 0.0
        self._cooldown_until = 0.0
        self._cooldown_streak = 0
        self._spent: Deque[List[float]] = deque()

    def _acquire(self, estimate: int) -> List[float]:
        """匀速发请求(间隔 60/RPM 秒),并保证最近 60 秒的 token 用量不超 TPM;返回本次用量记录(响应后改为实际值)。"""
        while True:
            with self._lock:
                now = time.monotonic()
                while self._spent and self._spent[0][0] <= now - WINDOW_SECONDS:
                    self._spent.popleft()
                wait = max(self._next_slot, self._cooldown_until) - now
                used = sum(entry[1] for entry in self._spent)
                if self._spent and used + estimate > self.tpm:
                    wait = max(wait, self._spent[0][0] + WINDOW_SECONDS - now)
                if wait <= 0:
                    self._next_slot = now + self.interval
                    entry = [now, float(estimate)]
                    self._spent.append(entry)
                    return entry
            time.sleep(min(wait, 5.0) + random.uniform(0, 0.05))

    def _rate_limited(self) -> None:
        """登记一次 429:冷却窗口未生效时开新窗口(时长按连续命中指数增长)并打一行日志。"""
        with self._lock:
            now = time.monotonic()
            if self._cooldown_until > now:
                return
            delay = min(RATE_LIMIT_MAX_SECONDS, RATE_LIMIT_BASE_SECONDS * (2 ** self._cooldown_streak))
            self._cooldown_streak += 1
            self._cooldown_until = now + delay
        progress(f"qwen-mt 触发限流(HTTP 429),全体暂停 {delay:.0f}s;持续出现请调低 QWEN_MT_RPM / QWEN_MT_TPM")

    def _succeeded(self, entry: List[float], payload: object) -> None:
        usage = payload.get("usage") if isinstance(payload, dict) else None
        total = usage.get("total_tokens") if isinstance(usage, dict) else None
        with self._lock:
            self._cooldown_streak = 0
            if isinstance(total, int):
                entry[1] = float(total)

    def translate(
        self,
        text: str,
        source_lang: str,
        target_lang: str,
        terms: List[Dict[str, str]],
        tm_list: List[Dict[str, str]],
        domains: str,
    ) -> str:
        options: Dict[str, object] = {"source_lang": source_lang, "target_lang": target_lang}
        if terms:
            options["terms"] = terms
        if tm_list:
            options["tm_list"] = tm_list
        if domains:
            options["domains"] = domains
        body = json.dumps(
            {"model": self.model, "messages": [{"role": "user", "content": text}], "translation_options": options},
            ensure_ascii=False,
        ).encode("utf-8")
        last = ""
        retries = 0
        throttled = 0
        # 粗估 token(输入按 UTF-8 字节 / 3,输出按原文字符 / 2),响应后以 usage.total_tokens 校正。
        estimate = len(body) // 3 + len(text) // 2
        while True:
            entry = self._acquire(estimate)
            request = urllib.request.Request(
                self.endpoint,
                data=body,
                method="POST",
                headers={"Authorization": f"Bearer {self.api_key}", "Content-Type": "application/json"},
            )
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    payload = json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as exc:
                detail = exc.read().decode("utf-8", errors="replace")[:300]
                if exc.code in (401, 403):
                    raise FatalApiError(f"qwen-mt 鉴权失败 HTTP {exc.code}：{detail}") from exc
                if exc.code == 429:
                    throttled += 1
                    if throttled > MAX_RATE_LIMIT_WAITS:
                        raise ItemError(f"限流重试 {MAX_RATE_LIMIT_WAITS} 次仍失败：HTTP 429") from exc
                    self._rate_limited()
                    continue
                if exc.code < 500:
                    raise ItemError(f"HTTP {exc.code}：{detail}") from exc
                last = f"HTTP {exc.code}：{detail}"
            except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
                last = f"网络错误：{exc}"
            except json.JSONDecodeError as exc:
                last = f"响应不是 JSON：{exc}"
            else:
                self._succeeded(entry, payload)
                try:
                    content = payload["choices"][0]["message"]["content"]
                except (KeyError, IndexError, TypeError) as exc:
                    raise ItemError(f"响应缺少 choices[0].message.content：{str(payload)[:300]}") from exc
                if not isinstance(content, str):
                    raise ItemError("响应 content 不是字符串")
                return content
            retries += 1
            if retries > MAX_RETRIES:
                raise ItemError(f"重试 {MAX_RETRIES} 次仍失败：{last[:200]}")
            progress(f"qwen-mt 第 {retries} 次重试（{last[:120]}）")
            time.sleep(BACKOFF_BASE_SECONDS * (2 ** (retries - 1)))
