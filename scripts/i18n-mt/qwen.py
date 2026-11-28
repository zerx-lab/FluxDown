"""阿里云百炼 qwen-mt 的 OpenAI 兼容接口客户端（仅标准库）。

qwen-mt 只接受一条 user 消息、不支持 system prompt；语种、术语、翻译记忆与领域提示
全部经 `translation_options` 传入。输入上限 8192 token，调用方按条 / 按段拆分。
"""

from __future__ import annotations

import email.utils
import json
import math
import random
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from datetime import datetime, timezone
from typing import Deque, Dict, List, Optional

from common import FatalApiError, ItemError, RateLimitError, progress

DEFAULT_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1"
DEFAULT_MODEL = "qwen-mt-plus"
MAX_RETRIES = 3
BACKOFF_BASE_SECONDS = 2.0
# 429 限流:所有并发线程共享一个冷却窗口,命中后全体暂停再试,不计入普通重试次数。
MAX_RATE_LIMIT_WAITS = 12
RATE_LIMIT_BASE_SECONDS = 5.0
RATE_LIMIT_MAX_SECONDS = 60.0
# 服务端 Retry-After 优先于本地退避，但设上限，防止异常值把整轮卡死。
RETRY_AFTER_MAX_SECONDS = 300.0
# 不会随时间自愈的账号状态（欠费、停服、仅免费额度已用尽）：任何 HTTP 状态命中都整轮中止。
# 注意 Throttling.AllocationQuota / insufficient_quota 是 TPM 限流，可恢复，不在此列。
# https://help.aliyun.com/zh/model-studio/error-code
FATAL_ERROR_CODES = frozenset({"Arrearage", "isv.OUT_OF_SERVICE", "AllocationQuota.FreeTierOnly"})
# 百炼对 qwen-mt 静态限流(按主账号,超出任一即 429 limit_requests):RPM 一律 60,
# 且「可能按 RPS = RPM/60 限制」,所以请求必须匀速发出而不是一分钟内突发。TPM 按模型(北京地域):
# https://help.aliyun.com/zh/model-studio/rate-limit
DEFAULT_RPM = 60
DEFAULT_TPM = {"qwen-mt-plus": 25_000, "qwen-mt-flash": 35_000, "qwen-mt-lite": 100_000, "qwen-mt-turbo": 35_000}
FALLBACK_TPM = 25_000
WINDOW_SECONDS = 60.0


def retry_after_seconds(value: Optional[str]) -> float:
    """解析 Retry-After（秒数或 HTTP 日期）；缺失、非法或已过期返回 0。"""
    if not value:
        return 0.0
    value = value.strip()
    try:
        seconds = float(value)
    except ValueError:
        try:
            when = email.utils.parsedate_to_datetime(value)
        except (TypeError, ValueError):
            return 0.0
        if when.tzinfo is None:
            when = when.replace(tzinfo=timezone.utc)
        seconds = (when - datetime.now(timezone.utc)).total_seconds()
    return seconds if math.isfinite(seconds) and seconds > 0 else 0.0


def error_codes(body: str) -> List[str]:
    """取错误响应里的 code / type（兼容模式在 error 对象内，原生接口在顶层）。"""
    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        return []
    if not isinstance(payload, dict):
        return []
    error = payload.get("error")
    source = error if isinstance(error, dict) else payload
    return [value for value in (source.get("code"), source.get("type")) if isinstance(value, str)]


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

    def _rate_limited(self, retry_after: float) -> None:
        """登记一次 429：开冷却窗口（本地指数退避与服务端 Retry-After 取大者）并打一行日志；
        窗口已生效时只在 Retry-After 要求更久时延长。"""
        hinted = min(RETRY_AFTER_MAX_SECONDS, retry_after)
        with self._lock:
            now = time.monotonic()
            if self._cooldown_until > now:
                if now + hinted <= self._cooldown_until:
                    return
                delay = hinted
            else:
                backoff = min(RATE_LIMIT_MAX_SECONDS, RATE_LIMIT_BASE_SECONDS * (2 ** self._cooldown_streak))
                self._cooldown_streak += 1
                delay = max(backoff, hinted)
            self._cooldown_until = now + delay
        source = f"，服务端 Retry-After {retry_after:.0f}s" if retry_after > 0 else ""
        progress(f"qwen-mt 触发限流(HTTP 429{source})，全体暂停 {delay:.0f}s；持续出现请调低 QWEN_MT_RPM / QWEN_MT_TPM")

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
                body = exc.read().decode("utf-8", errors="replace")
                detail = body[:300]
                codes = error_codes(body)
                if exc.code in (401, 403) or any(code in FATAL_ERROR_CODES for code in codes):
                    raise FatalApiError(f"qwen-mt 账号不可用 HTTP {exc.code}：{detail}") from exc
                if exc.code == 429:
                    throttled += 1
                    if throttled > MAX_RATE_LIMIT_WAITS:
                        raise RateLimitError(f"限流重试 {MAX_RATE_LIMIT_WAITS} 次仍失败：HTTP 429") from exc
                    self._rate_limited(retry_after_seconds(exc.headers.get("Retry-After") if exc.headers else None))
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
