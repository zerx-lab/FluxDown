---
name: release
description: >-
  发布 FluxDown 新版本或查看已有版本时使用。校验版本 tag（稳定版 vX.Y.Z / 前沿版
  vX.Y.Z-rc.N）在推送前合法可用，并按渠道/组件快速列出已有版本。关键词：发布, 发版,
  release, publish, 版本, version, tag, 打标签, 稳定版, 前沿版, stable, frontier, rc,
  预发布, prerelease, 查看版本, 已有版本, 最新版本, 更新渠道, changelog, git-cliff
---

# FluxDown 发布与版本查看

FluxDown 用 **SemVer 预发布后缀**区分双渠道：**稳定版 `vX.Y.Z`**、**前沿版 `vX.Y.Z-rc.N`**。
推送 `v*` tag 会**立即触发 GitHub Actions 全平台发布流水线（不可逆）**，同一次 push 自动派生
各组件 tag。本 skill 覆盖两件事：**发布前校验版本可用** + **快速查看已有版本**。

> 红线（`.omp/RULES.md`）：**未经用户明确要求，禁止 `git commit` / `push` / tag**。
> 本 skill 的推送命令仅在用户明确要求发布时执行。

## 1. 快速查看已有版本

```bash
# 客户端主线（稳定+前沿）最新在前
git tag -l 'v[0-9]*' --sort=-v:refname | head
# 仅稳定版（排除 -rc 预发布）
git tag -l 'v[0-9]*' --sort=-v:refname | grep -v -- '-rc' | head
# 仅前沿版（预发布 -rc）
git tag -l 'v[0-9]*-rc*' --sort=-v:refname | head
# 各组件线最新稳定一个（组件前缀本身含 '-'，故用 '-rc' 判别预发布）
for p in v server-v cli-v mobile-v extension-v; do \
  echo "$p -> $(git tag -l "${p}[0-9]*" --sort=-v:refname | grep -v -- '-rc' | head -1)"; done
# 已发布 release（含 prerelease 标记，需 gh 已登录该私有仓库）
gh release list --limit 20
```

`--sort=-v:refname` 对纯三段式排序精确；混入 `-rc` 时排名近似，需精确按时间用 `--sort=-creatordate`。

## 2. Tag 约定（发布契约）

| 渠道 | tag | 打自分支 | GitHub prerelease | make_latest | 打包范围 |
|---|---|---|---|---|---|
| 稳定版 | `vX.Y.Z` | `main` | false | true | 客户端 / web / 移动 / NAS / 扩展 全部 |
| 前沿版 | `vX.Y.Z-rc.N` | `develop` | true | false | 客户端 / web / 移动 / NAS，**不含浏览器扩展** |

- 组件线（`server-v*` / `cli-v*` / `mobile-v*` / `extension-v*`）由同一次 `v*` push 按目录 diff 自动派生，`make_latest:false`。前沿 push 同步给组件打 `-rc.N` 并标 prerelease。
- **扩展发布后无法改版本号**，故 `-rc` tag 跳过 `build-extension`/`release-extension`。
- 判据全在 `.github/workflows/release.yml`：`prerelease/make_latest = contains(github.ref_name, '-')`；Flutter `--build-name` 用剥后缀的 `CLEAN_VERSION`，`APP_VERSION` 保留完整版号。
- **分支模型**：`develop` = 开发分支（超集），`main` = 稳定分支（子集）；`main` 只经合并/cherry-pick `develop` 前进。稳定发布前 `git log main --not develop` 必须为空。
- **CI 分支守卫**（`changes` job 首步）：tag 提交必须在对应分支上——`vX.Y.Z` ∈ `origin/main`、`vX.Y.Z-rc.N` ∈ `origin/develop`，否则整条流水线立即失败（`git merge-base --is-ancestor` 判定）。

## 3. 发布前校验（保证版本可用）

```bash
V=v0.3.0            # 稳定版；前沿版示例：V=v0.3.0-rc.1
# 1) 格式合法：稳定 ^v[0-9]+\.[0-9]+\.[0-9]+$ ；前沿 ^v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$
printf '%s\n' "$V" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$' && echo OK || echo "格式非法"
# 2) tag 不重复
git rev-parse -q --verify "refs/tags/$V" >/dev/null && echo "已存在，换号" || echo "可用"
# 3) 单调递增：新版须高于最新同类（对照下面输出）
git tag -l 'v[0-9]*' --sort=-v:refname | head -3
# 4) 工作树干净、停在目标分支的目标提交（稳定=main，前沿=develop）
git status --porcelain    # 须为空
git branch --show-current && git log -1 --oneline
# 4b) 稳定发布额外：main 不得含 develop 没有的提交
git log main --not develop --oneline    # 须为空
# 5) 可选：本地预览 release notes（需装 git-cliff；未装则跳过，CI 仍会生成，无规范 commit 时用默认标题）
command -v git-cliff >/dev/null && git cliff --latest --strip header | head -40
```

版本"可用"的硬条件：格式合法 · tag 不重复 · 高于同渠道最新 · 停在正确分支且工作树干净 · 稳定版 `main --not develop` 为空。构建绿由调用者在发布前自行保证。
前沿版额外确认：后缀是 `-rc.N` 且 N 递增（前沿用户按 SemVer 收 `rc.1 < rc.2 < … < 转正 X.Y.Z`）。

## 4. 发布（不可逆，仅用户明确要求时）

```bash
# ⚠️ 推送后立即触发全平台构建与 GitHub Release，无法撤回。
# 先切到正确分支：稳定版 main，前沿版 develop（CI 守卫会拒绝打错分支的 tag）。
git checkout main        # 前沿版改为: git checkout develop
git tag -a "$V" -m "$V"
git push origin "$V"
# 观察流水线
gh run watch
```

## 5. 各渠道发布后可见性（自检预期）

- **官网下载 / `/releases/latest`**：永远只给稳定版（`/api/release` 缺省 = stable；下载页从不带 channel）。
- **前沿版**：仅 `/api/release?channel=frontier` 与客户端"更新渠道 = 前沿版"可见；前沿资产经 `/api/download/<name>?tag=<rc-tag>` 下载。
- **客户端更新判定**：`native/hub/src/updater.rs` 的 SemVer 比较器（含预发布精度）；渠道存于配置 `update_channel`（桌面/移动）、`web_update_channel`（web SPA）。

细节见根 `AGENTS.md`「发布新版本」与 `.github/workflows/release.yml`。
