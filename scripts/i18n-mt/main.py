#!/usr/bin/env python3
"""qwen-mt 自动翻译入口（仅标准库，GitHub Actions `.github/workflows/i18n-mt.yml` 调用）。

    python3 scripts/i18n-mt/main.py all                 # app + extension + docs
    python3 scripts/i18n-mt/main.py app --locales es,ru # 只同步 assets/i18n 的指定语言
    python3 scripts/i18n-mt/main.py all --dry-run       # 只统计待译，不调接口、不写文件

环境变量：DASHSCOPE_API_KEY（必填，演练除外）、QWEN_MT_MODEL（默认 qwen-mt-plus）、
DASHSCOPE_BASE_URL（默认北京地域 OpenAI 兼容地址）。
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from catalogs import app_surface, extension_surface, sync_surface
from common import ConfigError, FatalApiError, Report
from docs import sync_docs
from qwen import DEFAULT_BASE_URL, DEFAULT_MODEL, QwenMt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("target", choices=["all", "app", "extension", "docs"])
    parser.add_argument("--locales", default="", help="逗号分隔，只处理这些语言（docs 只有 zh，不受影响）")
    parser.add_argument("--limit", type=int, default=1500, help="每种语言每轮最多翻译的条数 / 文档篇数")
    parser.add_argument("--workers", type=int, default=6, help="并发请求数")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--summary", type=Path, help="把 Markdown 汇总追加到该文件（$GITHUB_STEP_SUMMARY）")
    args = parser.parse_args()

    translate = None
    if not args.dry_run:
        api_key = os.environ.get("DASHSCOPE_API_KEY", "").strip()
        if not api_key:
            print("缺少环境变量 DASHSCOPE_API_KEY", file=sys.stderr)
            return 2
        client = QwenMt(
            api_key,
            model=os.environ.get("QWEN_MT_MODEL", "").strip() or DEFAULT_MODEL,
            base_url=os.environ.get("DASHSCOPE_BASE_URL", "").strip() or DEFAULT_BASE_URL,
        )
        translate = client.translate

    wanted = [code.strip() for code in args.locales.split(",") if code.strip()] or None
    report = Report()
    status = 0
    try:
        if args.target in ("all", "app"):
            sync_surface(app_surface(), translate, report, wanted, args.limit, args.workers)
        if args.target in ("all", "extension"):
            sync_surface(extension_surface(), translate, report, wanted, args.limit, args.workers)
        if args.target in ("all", "docs"):
            sync_docs(translate, report, args.limit, args.workers)
    except (ConfigError, FatalApiError) as exc:
        report.note(f"中止：{exc}")
        print(f"::error::{exc}", file=sys.stderr)
        status = 1

    summary = report.markdown()
    print(summary)
    if args.summary:
        with args.summary.open("a", encoding="utf-8") as handle:
            handle.write(summary)
    if report.failures:
        print(f"::warning::{len(report.failures)} 条翻译失败，保持原状，下一轮重试（详见汇总）")
    return status


if __name__ == "__main__":
    sys.exit(main())
