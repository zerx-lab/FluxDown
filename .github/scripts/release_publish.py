#!/usr/bin/env python3
"""统一 vX.Y.Z release 的收尾（release.yml 的 publish-release job 调用；tag 推送与手动补发都跑，幂等）。

1. 以 `SHA256SUMS-<组件>.txt` 完成哨兵判定已上传完整的组件（契约见 .github/actions/release-upload）。
2. 把各组件哨兵合并为总校验文件 `SHA256SUMS.txt`。
3. 刷新 release 说明头部：已发布组件清单 + 服务器 / CLI / Android / iOS 安装说明。头部位于第一个
   `<!-- fluxdown:lang:* -->` 标记之前，官网 / App 的双语解析只取标记区块，不会把安装说明
   混进更新日志；组件清单变化会让补发产生 release `edited` 事件，官网据此清缓存。
4. 头部起始标记同时持久化本 tag 的发布状态：`expected=` 为历次运行（tag 推送 + 各次补发）应发布
   组件的并集，`docker=ok|pending` 为 Server Docker 镜像是否已推送成功（仅 expected 含 server 时写）。
   补发只构建单个组件，靠这份累计清单判断整个 release 是否齐全。
5. 草稿 → 发布：expected 中每个组件都有完成哨兵且 Docker 不处于 pending 时才转正（转正才触发
   release `published` webhook、官网 / 客户端可见与 Telegram 公告）；否则保持草稿，补发齐全后
   由那次补发转正。已发布的 release 不会退回草稿。稳定版且桌面端完整、且是最高稳定版本时才标 latest。
6. 未完成的组件写入 job summary 并以非零退出，提示补发。

`--seed-notes <file>`（prepare-release 调用）：把本次应发布组件写进说明文件头部后由调用方建 /
刷新草稿，保证 publish-release 未跑到时累计清单也已落盘，补发不会把缺组件的 release 提前转正。

演练（`--rehearsal-dir <dir>`，release.yml rehearsal 模式）：release 不存在，改从该目录读取各组件
`SHA256SUMS-<组件>.txt` 与 `RELEASE_NOTES.md`，按草稿状态走同一套判定，把 `SHA256SUMS.txt`、
`release-body.md` 写回该目录并打印计划中的 gh 操作；不调用任何 gh 命令。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

COMPONENTS = ("app", "extension", "server", "cli", "mobile", "ios")
LABELS = {
    "app": "桌面客户端",
    "extension": "浏览器扩展",
    "server": "Headless 服务器",
    "cli": "命令行 CLI",
    "mobile": "Android",
    "ios": "iOS 侧载包",
}
HEADER_RE = re.compile(
    r"<!-- fluxdown:release:begin[^>]*-->.*?<!-- fluxdown:release:end -->\n*", re.S
)
BEGIN_RE = re.compile(r"<!-- fluxdown:release:begin([^>]*)-->")
STABLE_TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")


def gh(*args: str) -> str:
    return subprocess.run(
        ["gh", *args], check=True, text=True, capture_output=True
    ).stdout


def merge_checksums(texts: list[str]) -> str:
    """合并多个 `sha256sum` 输出，按文件名去重排序。"""
    entries: dict[str, str] = {}
    for text in texts:
        for line in text.splitlines():
            parts = line.split()
            if len(parts) == 2:
                entries[parts[1].lstrip("*")] = parts[0]
    return "".join(f"{digest}  {name}\n" for name, digest in sorted(entries.items()))


def install_sections(present: list[str], version: str, repo: str, owner: str) -> str:
    sections: list[str] = []
    if "server" in present:
        sections.append(
            f"""## 📦 Headless 服务器：直接下载运行（免 Docker）

| 平台 | 资产 |
|---|---|
| Windows x64 / ARM64 | `FluxDown-Server-{version}-windows-{{x64,arm64}}.zip` |
| macOS Intel / Apple Silicon | `FluxDown-Server-{version}-macos-{{x64,arm64}}.tar.gz` |
| Linux x64 / ARM64（musl 静态链接，任意发行版可用） | `FluxDown-Server-{version}-linux-{{x64,arm64}}.tar.gz` |
| OpenWrt 软路由 24.10 及更早（opkg，x86_64 / aarch64 各子架构） | `fluxdown-server_{version}_<arch>.ipk` + `luci-app-fluxdown_{version}_all.ipk` |
| OpenWrt 软路由 25.12 起（apk，x86_64 / aarch64 各子架构） | `fluxdown-server_{version}_<arch>.apk` + `luci-app-fluxdown_{version}_all.apk` |
| QNAP NAS（QTS x86_64 / ARM64） | `FluxDown-Server-{version}-qnap-{{x64,arm64}}.qpkg` |
| 群晖 NAS（DSM 7 / DSM 6，x86_64 / ARM64） | `FluxDown-Server-{version}-synology-{{dsm7,dsm6}}-{{x64,arm64}}.spk` |
| 飞牛 fnOS NAS（x86 / ARM） | `FluxDown-Server-{version}-fnos-{{x64,arm64}}.fpk` |

解压后运行 `fluxdown-agent --server`（Windows 为 `fluxdown-agent.exe --server`），浏览器访问 `http://127.0.0.1:17800`。
**包内两个可执行文件必须放在同一目录**：`fluxdown-agent`（含 Web 控制台，已编译进二进制）与 `fluxdownd`（下载 daemon，由 agent 自动拉起、退出时一并关停），无需附带任何静态资源目录。
首次启动请在浏览器里完成「初始化 FluxDown Server」向导。向导需要数据目录 `agent/setup-bootstrap` 里的一次性口令（Docker 为 `/data/agent/setup-bootstrap`）；无人值守部署可用 `FLUXDOWN_TOKEN` 预置并跳过向导。

**OpenWrt**：24.10 及更早用 `opkg install fluxdown-server_*.ipk luci-app-fluxdown_*.ipk`；25.12 起用 `apk add --allow-untrusted fluxdown-server_*.apk luci-app-fluxdown_*.apk`（包未用 OpenWrt 官方密钥签名）。`<arch>` 取路由器 `/etc/apk/arch`（或 `opkg print-architecture`）中的架构名。LuCI「服务 → FluxDown」页管理，Web 界面在 `http://路由器IP:17800`。agent 二进制含完整 Web UI，另附 fluxdownd，需 overlay 空间充足的软路由设备。
**QNAP**：App Center → 手动安装 `.qpkg`（需在设置中允许安装未经验证的应用），装完点应用图标直达 Web 界面。
**群晖**：套件中心 → 手动安装 `.spk`（需在「设置 → 信任来源」允许任何发行者）。DSM 7.x 装 `dsm7` 包（以套件专属用户运行），DSM 6.x 装 `dsm6` 包；Intel/AMD 机型选 x64，ARM 机型（rtd1296/rtd1619b 等）选 arm64。装完在套件中心点「打开」直达 Web 界面（端口 17800）。
**飞牛 fnOS**：应用中心 → 手动安装 `.fpk`；x86 机型选 x64，ARM 机型选 arm64。以专用应用用户 `fluxdown` 运行，默认保存到安装时创建的 `fluxdown` 共享文件夹，其他目录在应用设置「授权目录」中添加。装完点桌面图标直达 Web 界面（端口 17800）。

## 🐳 Headless 服务器：Docker（linux/amd64 · linux/arm64）

```bash
docker pull ghcr.io/{owner}/fluxdown-server:{version}
docker run -d -p 17800:17800 -v fluxdown-data:/data \\
  ghcr.io/{owner}/fluxdown-server:{version}
```

Compose 部署见仓库 `docker/docker-compose.yml`。首次打开 Web 界面时按向导设置访问密钥（须先读取数据卷里的 `agent/setup-bootstrap`），或用 `-e FLUXDOWN_TOKEN=...` 预置。
"""
        )
    if "cli" in present:
        sections.append(
            f"""## 📦 命令行 CLI

| 平台 | 资产 |
|---|---|
| Windows x64 / ARM64 | `FluxDown-CLI-{version}-windows-{{x64,arm64}}.zip` |
| macOS Intel / Apple Silicon | `FluxDown-CLI-{version}-macos-{{x64,arm64}}.tar.gz` |
| Linux x64 / ARM64（musl 静态链接，任意发行版可用） | `FluxDown-CLI-{version}-linux-{{x64,arm64}}.tar.gz` |

解压后得到 `fluxdown`（Windows 为 `fluxdown.exe`），放入 PATH 即可使用；也可从源码安装：
`cargo install --git https://github.com/{repo} fluxdown_cli`。

```bash
fluxdown ping                         # 探活（需 FluxDown App / server 运行中）
fluxdown add https://example.com/f.zip
fluxdown ls
```

服务地址经 `--url` 或 `FLUXDOWN_URL`（默认 `http://127.0.0.1:17800`），管理 token 经 `--token` 或 `FLUXDOWN_TOKEN`。
"""
        )
    if "mobile" in present:
        sections.append(
            f"""## 📱 Android

| 设备 | 资产 |
|---|---|
| 不确定架构（通用包） | `FluxDown-{version}-android-universal.apk` |
| 64 位 ARM（绝大多数手机） | `FluxDown-{version}-android-arm64-v8a.apk` |
| 32 位 ARM | `FluxDown-{version}-android-armeabi-v7a.apk` |
| x86_64（模拟器 / Chromebook） | `FluxDown-{version}-android-x86_64.apk` |

⚠️ **从旧版（Flutter 版）升级**：新版 Android 客户端换用了新的签名证书，不能直接覆盖安装旧版（系统可能提示「应用未安装」「软件包无效」「无法降级」或签名冲突）。
请先在旧版中记下需要保留的下载任务与设置，卸载旧版后再安装新版；卸载会清除旧版的本地数据，已下载到公共目录的文件不受影响。
"""
        )
    if "ios" in present:
        sections.append(
            f"""## 📱 iOS（侧载，未签名）

| 版本 | 资产 |
|---|---|
| 完整版：含 BitTorrent / eD2K，远端主机的插件 / 组件管理可用 | `FluxDown-{version}-ios-sideload-unsigned.ipa` |

IPA 未签名，需用 AltStore / SideStore / Sideloadly 等工具以自己的 Apple ID 签名后安装；免费 Apple ID 的签名 7 天后过期，需重新签名（AltStore / SideStore 可自动续签）。
不想自签可加入 TestFlight 公测（App Store 版，不含 P2P 与插件 / 组件功能），入口见官网下载页。
"""
        )
    return "\n".join(sections)


def header_attrs(body: str) -> dict[str, str]:
    """读取说明头部起始标记上的 `key=value` 属性（无头部返回空）。"""
    match = BEGIN_RE.search(body)
    if not match:
        return {}
    return dict(kv.split("=", 1) for kv in match.group(1).split() if "=" in kv)


def component_list(*groups: list[str]) -> list[str]:
    """多组组件名取并集，按 COMPONENTS 规范顺序输出，丢弃未知名。"""
    wanted = {c for group in groups for c in group}
    return [c for c in COMPONENTS if c in wanted]


def docker_state(
    expected_all: list[str], run_expected: list[str], docker_result: str, previous: str | None
) -> str | None:
    """Server Docker 镜像状态：本次构建了 server 以本次结果为准，否则沿用记录；未知按 pending。"""
    if "server" not in expected_all:
        return None
    if "server" in run_expected:
        return "ok" if docker_result == "success" else "pending"
    return previous if previous in ("ok", "pending") else "pending"


def compose_body(
    body: str,
    present: list[str],
    expected: list[str],
    docker: str | None,
    version: str,
    repo: str,
    owner: str,
) -> str:
    """去掉旧头部，按当前已发布组件与累计发布状态重建头部，其后接原有更新说明。"""
    notes = HEADER_RE.sub("", body).lstrip("\n")
    attrs = f"components={','.join(present)} expected={','.join(expected)}"
    if docker:
        attrs += f" docker={docker}"
    header = f"<!-- fluxdown:release:begin {attrs} -->\n"
    sections = install_sections(present, version, repo, owner)
    if sections:
        header += (
            "<details>\n<summary>📦 下载与安装说明（服务器 / CLI / Android / iOS）· 文件校验见 SHA256SUMS.txt</summary>\n\n"
            f"{sections}\n</details>\n"
        )
    header += "<!-- fluxdown:release:end -->\n\n"
    return header + notes


def is_highest_stable(tag: str, tags: list[str]) -> bool:
    current = STABLE_TAG_RE.match(tag)
    if not current:
        return False
    versions = [tuple(map(int, m.groups())) for t in tags if (m := STABLE_TAG_RE.match(t))]
    return tuple(map(int, current.groups())) == max(versions, default=())


def write_summary(
    tag: str,
    expected: list[str],
    present: list[str],
    failures: list[str],
    published: bool,
    rehearsal: bool,
) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    rows = [
        f"| {LABELS[c]} (`{c}`) | {'✔' if c in expected else ''} | {'✅' if c in present else '—'} |"
        for c in COMPONENTS
    ]
    if rehearsal:
        title = f"## Release {tag}（演练，未发布）"
    else:
        title = f"## Release {tag}（{'已发布' if published else '草稿'}）"
    lines = [
        title,
        "",
        "| 组件 | 应发布（tag + 历次补发累计） | 已上传（完成哨兵） |",
        "|---|---|---|",
        *rows,
    ]
    if failures and rehearsal:
        lines += ["", f"**演练发现未完成：{', '.join(failures)}**。真实发布时 release 会停在草稿，需先修复。"]
    elif failures:
        state = (
            "release 已发布，以下组件仍缺失"
            if published
            else "release 保持草稿（官网与客户端仍提供上一版本），全部组件补发完成后由那次补发自动转正并发布公告"
        )
        components = list(dict.fromkeys(c.split("(")[0] for c in failures))
        lines += [
            "",
            f"**未完成：{', '.join(failures)}**。{state}。",
            "",
            "修复后补发（源码默认取 tag，修了代码时用 `source_ref` 指定包含修复的分支或提交）：",
            "",
            "```bash",
            *(
                f"gh workflow run release.yml --ref main -f tag={tag} -f component={c} [-f source_ref=<branch|sha>]"
                for c in components
            ),
            "```",
            "",
            "构建源码未变的偶发失败也可直接在本次运行页点 “Re-run failed jobs”。",
        ]
    with open(path, "a", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def seed_notes(tag: str, notes_path: Path, expected: list[str], release_exists: bool) -> None:
    """prepare-release：把累计应发布组件写进即将写入草稿的说明文件头部。"""
    repo = os.environ["GITHUB_REPOSITORY"]
    owner = os.environ["GITHUB_REPOSITORY_OWNER"]
    old_body = (
        json.loads(gh("release", "view", tag, "--json", "body"))["body"] or ""
        if release_exists
        else ""
    )
    attrs = header_attrs(old_body)
    expected_all = component_list(attrs.get("expected", "").split(","), expected)
    present = component_list(attrs.get("components", "").split(","))
    docker = docker_state(expected_all, [], "", attrs.get("docker"))
    body = compose_body(
        notes_path.read_text(encoding="utf-8"),
        present,
        expected_all,
        docker,
        tag.removeprefix("v"),
        repo,
        owner,
    )
    notes_path.write_text(body, encoding="utf-8")
    print(f"{tag}: seeded release header; expected={','.join(expected_all)}; docker={docker}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True)
    parser.add_argument("--expected", default="", help="本次应发布的组件，逗号分隔")
    parser.add_argument("--docker-result", default="", help="build-server-docker 的 job result")
    parser.add_argument(
        "--rehearsal-dir", default="", help="演练：从该目录读哨兵与说明，只输出预览，不调用 gh"
    )
    parser.add_argument(
        "--seed-notes", default="", help="prepare-release：把累计应发布组件写进该说明文件头部后退出"
    )
    parser.add_argument(
        "--release-exists", action="store_true", help="--seed-notes 时 release 已存在（合并其头部状态）"
    )
    args = parser.parse_args()

    tag: str = args.tag
    version = tag.removeprefix("v")
    expected = component_list(args.expected.split(","))
    if args.seed_notes:
        seed_notes(tag, Path(args.seed_notes), expected, args.release_exists)
        return 0

    repo = os.environ["GITHUB_REPOSITORY"]
    owner = os.environ["GITHUB_REPOSITORY_OWNER"]
    rehearsal_dir = Path(args.rehearsal_dir) if args.rehearsal_dir else None

    if rehearsal_dir:
        # 演练：prepare-release 未建草稿，按「草稿 + 本次生成的说明 + 本次产出的哨兵」模拟
        rehearsal_dir.mkdir(parents=True, exist_ok=True)
        notes_path = rehearsal_dir / "RELEASE_NOTES.md"
        info = {
            "assets": [{"name": p.name} for p in rehearsal_dir.glob("SHA256SUMS-*.txt")],
            "body": notes_path.read_text(encoding="utf-8") if notes_path.exists() else "",
            "isDraft": True,
        }
    else:
        info = json.loads(gh("release", "view", tag, "--json", "assets,body,isDraft"))
    names = {a["name"] for a in info["assets"]}
    present = [c for c in COMPONENTS if f"SHA256SUMS-{c}.txt" in names]

    old_body = info.get("body") or ""
    attrs = header_attrs(old_body)
    expected_all = component_list(attrs.get("expected", "").split(","), expected)
    docker = docker_state(expected_all, expected, args.docker_result, attrs.get("docker"))
    failures = [c for c in expected_all if c not in present]
    if docker == "pending":
        failures.append("server(docker)")
    complete = bool(expected_all) and not failures

    if present and rehearsal_dir:
        merged = merge_checksums(
            [p.read_text(encoding="utf-8") for p in sorted(rehearsal_dir.glob("SHA256SUMS-*.txt"))]
        )
        (rehearsal_dir / "SHA256SUMS.txt").write_text(merged, encoding="utf-8")
    elif present:
        with tempfile.TemporaryDirectory() as tmp:
            gh("release", "download", tag, "-p", "SHA256SUMS-*.txt", "-D", tmp, "--clobber")
            merged = merge_checksums(
                [p.read_text(encoding="utf-8") for p in sorted(Path(tmp).glob("SHA256SUMS-*.txt"))]
            )
            total = Path(tmp) / "SHA256SUMS.txt"
            total.write_text(merged, encoding="utf-8")
            gh("release", "upload", tag, str(total), "--clobber")

    edit_args: list[str] = []
    new_body = compose_body(old_body, present, expected_all, docker, version, repo, owner)
    notes_dir = rehearsal_dir or Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
    notes_file = notes_dir / "release-body.md"
    if new_body != old_body:
        notes_file.write_text(new_body, encoding="utf-8")
        edit_args += ["--notes-file", str(notes_file)]

    tags = subprocess.run(
        ["git", "tag", "-l", "v*"], check=True, text=True, capture_output=True
    ).stdout.split()
    if tag not in tags:
        tags.append(tag)  # 演练的 tag 尚不存在；真实发布时它已在列表中，此处无变化
    should_latest = "app" in present and is_highest_stable(tag, tags)
    published = not info["isDraft"] or complete
    if info["isDraft"]:
        if complete:
            # 草稿首次发布时 GitHub 默认抢占 latest，必须显式给值
            edit_args += ["--draft=false", f"--latest={'true' if should_latest else 'false'}"]
    elif should_latest:
        edit_args += ["--latest"]

    if edit_args and rehearsal_dir:
        print(f"🧪 rehearsal: would run gh release edit {tag} {' '.join(edit_args)}")
    elif edit_args:
        gh("release", "edit", tag, *edit_args)

    write_summary(tag, expected_all, present, failures, published, rehearsal_dir is not None)

    state = "published" if published else "draft"
    if rehearsal_dir:
        state = f"rehearsal (would be {state})"
    print(
        f"{tag}: {state}; components={','.join(present) or '<none>'}; "
        f"expected={','.join(expected_all) or '<none>'}; docker={docker}; latest={should_latest}"
    )
    if not expected_all:
        print(f"::error::{tag} 没有记录任何应发布组件，release 保持草稿")
        return 1
    if failures:
        if rehearsal_dir:
            hint = "演练结果见 job summary"
        elif published:
            hint = "release 已发布；见 job summary 的补发命令"
        else:
            hint = "release 保持草稿，全部补发完成后自动转正；见 job summary 的补发命令"
        print(f"::error::{tag} 未完成的组件：{', '.join(failures)}（{hint}）")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
