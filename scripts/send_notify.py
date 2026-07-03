#!/usr/bin/env python3
"""
FluxDown 平台发布通知脚本
用法: python3 send_notify.py [--config notify_config.json] [--dry-run] [--resume]

改进：
  - 分批发送，每批重新建立 SMTP 连接，规避 163 单连接限制
  - 遇到 4xx 临时错误自动等待重试
  - 进度记录到 .send_progress 文件，支持 --resume 断点续发
"""

import argparse
import json
import os
import smtplib
import ssl
import sys
import time
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText
from pathlib import Path

# ── SMTP 配置 ────────────────────────────────────────────────────────────────
# 凭据一律来自环境变量，禁止硬编码：
#   FLUXDOWN_SMTP_USER  发件邮箱
#   FLUXDOWN_SMTP_PASS  SMTP 授权码
SMTP_HOST = os.environ.get("FLUXDOWN_SMTP_HOST", "smtp.163.com")
SMTP_PORT = int(os.environ.get("FLUXDOWN_SMTP_PORT", "465"))
SMTP_USER = os.environ.get("FLUXDOWN_SMTP_USER", "")
SMTP_PASS = os.environ.get("FLUXDOWN_SMTP_PASS", "")
SENDER_NAME = "FluxDown"

# ── 发送策略 ─────────────────────────────────────────────────────────────────
BATCH_SIZE = 8  # 每批发送数量（每批结束后重新连接）
INTERVAL_SEC = 3  # 同批内每封间隔（秒）
BATCH_PAUSE_SEC = 15  # 批次间暂停（秒），让服务器冷却
RETRY_LIMIT = 3  # 单封最大重试次数
RETRY_WAIT_SEC = 30  # 遇到临时错误后等待时间（秒）

# ── HTML 邮件模板 ────────────────────────────────────────────────────────────
TEMPLATE_PATH = Path(__file__).parent / "email_template.html"

PLATFORM_ICONS: dict[str, str] = {
    "linux": "🐧",
    "macos": "🍎",
    "windows": "🪟",
    "mobile": "📱",
    "web": "🌐",
}


def build_html(
    platform: str, version: str, download_url: str, changelog: list[str]
) -> str:
    if not TEMPLATE_PATH.exists():
        raise FileNotFoundError(f"HTML 模板文件不存在: {TEMPLATE_PATH}")

    changelog_items = "\n".join(
        f'<li style="margin:8px 0;color:#374151;">{item}</li>' for item in changelog
    )
    icon = PLATFORM_ICONS.get(platform.lower(), "🚀")

    tpl = TEMPLATE_PATH.read_text(encoding="utf-8")
    return (
        tpl.replace("{{platform}}", platform)
        .replace("{{version}}", version)
        .replace("{{download_url}}", download_url)
        .replace("{{changelog_items}}", changelog_items)
        .replace("{{icon}}", icon)
    )


def build_text(
    platform: str, version: str, download_url: str, changelog: list[str]
) -> str:
    """纯文本回退版本"""
    items = "\n".join(f"  • {item}" for item in changelog)
    return f"""FluxDown {platform} v{version} 正式发布！

你好！感谢订阅 FluxDown {platform} 平台发布通知。

本次更新亮点：
{items}

立即下载：{download_url}

---
© 2025 FluxDown · zerx-lab · https://fluxdown.zerx.dev
如有问题或建议：https://fluxdown.zerx.dev/feedback
"""


def build_message(
    to_addr: str,
    platform: str,
    version: str,
    download_url: str,
    changelog: list[str],
) -> MIMEMultipart:
    subject = f"FluxDown {platform} v{version} 正式发布 🎉"
    msg = MIMEMultipart("alternative")
    msg["Subject"] = subject
    msg["From"] = f"{SENDER_NAME} <{SMTP_USER}>"
    msg["To"] = to_addr
    msg.attach(
        MIMEText(
            build_text(platform, version, download_url, changelog), "plain", "utf-8"
        )
    )
    msg.attach(
        MIMEText(
            build_html(platform, version, download_url, changelog), "html", "utf-8"
        )
    )
    return msg


def new_smtp_conn() -> smtplib.SMTP_SSL:
    """建立并登录一个新的 SMTP_SSL 连接"""
    if not SMTP_USER or not SMTP_PASS:
        print(
            "[错误] 未设置 FLUXDOWN_SMTP_USER / FLUXDOWN_SMTP_PASS 环境变量",
            file=sys.stderr,
        )
        sys.exit(1)
    ctx = ssl.create_default_context()
    smtp = smtplib.SMTP_SSL(SMTP_HOST, SMTP_PORT, context=ctx)
    smtp.login(SMTP_USER, SMTP_PASS)
    return smtp


def send_one(
    smtp: smtplib.SMTP_SSL,
    to_addr: str,
    platform: str,
    version: str,
    download_url: str,
    changelog: list[str],
) -> tuple[bool, bool]:
    """
    发送单封邮件，返回 (成功, 需要重连)。
    遇到临时错误（4xx）返回 (False, True)，表示应重连后重试。
    """
    msg = build_message(to_addr, platform, version, download_url, changelog)
    try:
        smtp.sendmail(SMTP_USER, to_addr, msg.as_bytes())
        return True, False
    except smtplib.SMTPServerDisconnected:
        return False, True
    except smtplib.SMTPResponseException as e:
        code = e.smtp_code
        print(f"\n    [!] SMTP {code}: {e.smtp_error}", file=sys.stderr)
        # 4xx 临时错误：频率限制、服务器忙等
        if 400 <= code < 500:
            return False, True
        # 5xx 永久错误：地址不存在等，不重试
        return False, False
    except smtplib.SMTPException as e:
        print(f"\n    [!] {e}", file=sys.stderr)
        return False, False


def send_with_retry(
    to_addr: str,
    platform: str,
    version: str,
    download_url: str,
    changelog: list[str],
    smtp_ref: list[smtplib.SMTP_SSL],  # 用列表包装以便原地替换
) -> bool:
    """带重试的发送，smtp_ref[0] 可能被替换为新连接"""
    for attempt in range(1, RETRY_LIMIT + 1):
        ok, need_reconnect = send_one(
            smtp_ref[0], to_addr, platform, version, download_url, changelog
        )
        if ok:
            return True
        if not need_reconnect:
            # 永久失败，不重试
            return False
        if attempt < RETRY_LIMIT:
            print(
                f"\n    [重试 {attempt}/{RETRY_LIMIT - 1}] 等待 {RETRY_WAIT_SEC}s 后重连...",
                file=sys.stderr,
            )
            time.sleep(RETRY_WAIT_SEC)
            try:
                smtp_ref[0].quit()
            except Exception:
                pass
            print(f"    [重连] {SMTP_HOST}:{SMTP_PORT} ...", end=" ", flush=True)
            smtp_ref[0] = new_smtp_conn()
            print("OK")
    return False


# ── 进度文件 ─────────────────────────────────────────────────────────────────
def load_progress(progress_path: Path) -> set[str]:
    if progress_path.exists():
        data = json.loads(progress_path.read_text(encoding="utf-8"))
        return set(data.get("sent", []))
    return set()


def save_progress(progress_path: Path, sent: set[str]) -> None:
    progress_path.write_text(
        json.dumps({"sent": sorted(sent)}, ensure_ascii=False, indent=2),
        encoding="utf-8",
    )


# ── 主流程 ───────────────────────────────────────────────────────────────────
def main() -> None:
    parser = argparse.ArgumentParser(description="FluxDown 平台发布通知脚本")
    parser.add_argument(
        "--config",
        default=Path(__file__).parent / "notify_config.json",
        type=Path,
        help="配置文件路径（默认: notify_config.json）",
    )
    parser.add_argument("--dry-run", action="store_true", help="仅预览，不实际发送邮件")
    parser.add_argument(
        "--resume",
        action="store_true",
        help="从上次中断处继续（读取 .send_progress 跳过已发地址）",
    )
    args = parser.parse_args()

    # 读取配置
    config_path: Path = args.config
    if not config_path.exists():
        print(f"[错误] 配置文件不存在: {config_path}", file=sys.stderr)
        sys.exit(1)

    with config_path.open(encoding="utf-8") as f:
        cfg = json.load(f)

    platform: str = cfg["platform"]
    version: str = cfg["version"]
    download_url: str = cfg["download_url"]
    changelog: list[str] = cfg.get("changelog", [])
    recipients: list[str] = cfg["recipients"]

    progress_path = config_path.parent / ".send_progress"

    # 断点续发
    already_sent: set[str] = set()
    if args.resume:
        already_sent = load_progress(progress_path)
        print(f"[续发] 跳过已发送 {len(already_sent)} 位，继续剩余地址")

    pending = [addr for addr in recipients if addr not in already_sent]

    print(f"╔══════════════════════════════════════════╗")
    print(f"  FluxDown 发布通知脚本")
    print(f"  平台: {platform}  版本: v{version}")
    print(f"  收件人: {len(recipients)} 位  待发: {len(pending)} 位")
    print(f"  批大小: {BATCH_SIZE}  间隔: {INTERVAL_SEC}s  批停顿: {BATCH_PAUSE_SEC}s")
    print(
        f"  Dry-run: {'是' if args.dry_run else '否'}  断点续发: {'是' if args.resume else '否'}"
    )
    print(f"╚══════════════════════════════════════════╝\n")

    success_count = 0
    fail_count = 0
    sent_set = set(already_sent)

    if args.dry_run:
        for addr in pending:
            print(f"  [dry-run] 跳过发送 → {addr}")
            success_count += 1
        print(f"\n── 完成 ──────────────────────────────────")
        print(f"  成功: {success_count}  失败: {fail_count}")
        return

    # 分批发送
    batches = [pending[i : i + BATCH_SIZE] for i in range(0, len(pending), BATCH_SIZE)]
    total = len(pending)
    global_idx = len(already_sent)  # 在全部收件人中的序号起点

    for batch_no, batch in enumerate(batches, 1):
        print(
            f"[批次 {batch_no}/{len(batches)}] 连接 {SMTP_HOST}:{SMTP_PORT} ...",
            end=" ",
            flush=True,
        )
        try:
            smtp = new_smtp_conn()
        except smtplib.SMTPAuthenticationError:
            print("\n[错误] SMTP 认证失败，请检查账号/密码/授权码", file=sys.stderr)
            sys.exit(1)
        except OSError as e:
            print(f"\n[错误] 无法连接到 SMTP 服务器: {e}", file=sys.stderr)
            sys.exit(1)
        print("OK 已登录\n")

        smtp_ref = [smtp]

        for batch_local_idx, addr in enumerate(batch):
            global_idx += 1
            print(
                f"  [{global_idx}/{len(recipients)}] 发送 → {addr} ... ",
                end="",
                flush=True,
            )

            ok = send_with_retry(
                addr, platform, version, download_url, changelog, smtp_ref
            )
            if ok:
                print("✓")
                success_count += 1
                sent_set.add(addr)
                save_progress(progress_path, sent_set)
            else:
                print("✗")
                fail_count += 1

            # 同批内等待（最后一封不需要等）
            is_last_in_batch = batch_local_idx == len(batch) - 1
            is_last_overall = global_idx == total + len(already_sent)
            if not is_last_in_batch and not is_last_overall:
                time.sleep(INTERVAL_SEC)

        # 关闭当前连接
        try:
            smtp_ref[0].quit()
        except Exception:
            pass

        # 批次间暂停（最后一批不等）
        if batch_no < len(batches):
            print(f"\n  [批次暂停] 等待 {BATCH_PAUSE_SEC}s，让服务器冷却...\n")
            time.sleep(BATCH_PAUSE_SEC)

    # 全部发完后清理进度文件
    if fail_count == 0 and progress_path.exists():
        progress_path.unlink()

    print(f"\n── 完成 ──────────────────────────────────")
    print(f"  成功: {success_count}  失败: {fail_count}")
    if fail_count > 0:
        print(f"  提示: 下次运行加 --resume 可跳过已成功的地址")


if __name__ == "__main__":
    main()
