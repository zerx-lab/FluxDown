"""阿里云百炼 qwen-mt 的 OpenAI 兼容接口客户端（仅标准库）。

qwen-mt 只接受一条 user 消息、不支持 system prompt；语种、术语、翻译记忆与领域提示
全部经 `translation_options` 传入。输入上限 8192 token，调用方按条 / 按段拆分。
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
from typing import Dict, List

from common import FatalApiError, ItemError

DEFAULT_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1"
DEFAULT_MODEL = "qwen-mt-plus"
MAX_RETRIES = 3
BACKOFF_BASE_SECONDS = 2.0


class QwenMt:
    def __init__(self, api_key: str, model: str = DEFAULT_MODEL, base_url: str = DEFAULT_BASE_URL, timeout: float = 120.0) -> None:
        self.api_key = api_key
        self.model = model
        self.endpoint = base_url.rstrip("/") + "/chat/completions"
        self.timeout = timeout

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
        for attempt in range(MAX_RETRIES + 1):
            if attempt:
                time.sleep(BACKOFF_BASE_SECONDS * (2 ** (attempt - 1)))
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
                if exc.code == 429 or exc.code >= 500:
                    last = f"HTTP {exc.code}：{detail}"
                    continue
                raise ItemError(f"HTTP {exc.code}：{detail}") from exc
            except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
                last = f"网络错误：{exc}"
                continue
            except json.JSONDecodeError as exc:
                last = f"响应不是 JSON：{exc}"
                continue
            try:
                content = payload["choices"][0]["message"]["content"]
            except (KeyError, IndexError, TypeError) as exc:
                raise ItemError(f"响应缺少 choices[0].message.content：{str(payload)[:300]}") from exc
            if not isinstance(content, str):
                raise ItemError("响应 content 不是字符串")
            return content
        raise ItemError(f"重试 {MAX_RETRIES} 次仍失败：{last}")
