---
description: 访问密钥策略、开机自启、应用更新/更新说明、云端推送、套餐购买判定须与 Web / Android / LuCI 镜像一致
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/native/agent/src/server_mode.rs'
  - '**/web/src/lib/token-policy.ts'
  - '**/packaging/openwrt/luci/**/fluxdown.js'
  - '**/native/agent/src/platform/autostart.rs'
  - '**/crates/settings/src/{update_view,version_button}.rs'
  - '**/web/src/lib/{update,releaseNotes,diagnosticInfo}.ts'
  - '**/mobile/Android/core/src/main/java/com/fluxdown/core/update/**'
  - '**/crates/settings/src/sections/about.rs'
  - '**/crates/settings/src/model/cloud_notify.rs'
  - '**/web/src/pages/webhooks/cloudLogic.ts'
  - '**/crates/account/src/billing.rs'
  - '**/web/src/pages/settings/sections/account/billing.ts'
interruptMode: never
---

你正在改动设置与账户相关判定的一侧。下列条目里，标题是事实源一侧、「→」后是必须同回合同步的镜像端：改任一侧都要把另一侧过一遍；有共享夹具的先改夹具用例，再改两侧实现，两侧测试都要跑（`/verify` 第 3 步）。只改了一侧就交付 = 不完整。

## `agent/src/server_mode.rs::validate_access_key`

→ `web/src/lib/token-policy.ts` + OpenWrt LuCI `packaging/openwrt/luci/www/luci-static/resources/view/fluxdown.js::validateToken`（UCI `token` → `FLUXDOWN_TOKEN` + `FLUXDOWN_TOKEN_FORCE=1`）

## 开机自启与旧版迁移

→ `native/agent/src/platform/autostart.rs`：「已启用」尊重系统级禁用（Windows `StartupApproved`、XDG `Hidden` / `X-GNOME-Autostart-enabled`），启动时自动迁移只改启动目标、**绝不**改系统启用状态；见 `.omp/knowledge/clients.md`「开机自启与旧版迁移」

## 应用内更新界面判定（`crates/settings/src/update_view.rs`：阶段 → 文案键、手动原因 / 失败键、`can_install` / `can_cancel` / 手动下载地址）

→ `web/src/lib/update.ts` + 原生 Android `mobile/Android/core/.../update/UpdateView.kt`：逐条对齐（`update.test.ts` / `UpdateViewTest` 覆盖同一组；Android 仅 Ready / Installing / 用户取消 / 商店安装换用 `mobileUpdate*` 文案键）；状态与流程只归 agent `native/agent/src/update/`（Android 经 `native/mobile` 的 `AppUpdater` 复用同一 `UpdateService`，Kotlin 只做安装 / 调度 / 界面），发布资产 / 校验和哨兵 / 双域名依赖见 `.omp/knowledge/hosts-and-api.md`「应用内更新」

## 状态栏版本条目（`crates/settings/src/version_button.rs`：`update_badge` 更新形态、`diagnostic_text` 诊断信息格式）

→ `web/src/lib/update.ts::versionBadge` + `web/src/lib/diagnosticInfo.ts`：逐条对齐（`update.test.ts` / `diagnosticInfo.test.ts` 与 Rust 测试为同一组用例）；诊断信息只含版本 / 渠道 / 协议 / 平台或安装形态 / 客户端 / 语言，不含路径、账户、设备名、地址。Android / iOS 无此条目

## 更新说明展示规则（`crates/settings/src/sections/about.rs`：`localized_release_body` 按界面语言切段、`strip_version_heading` 去掉正文开头与标题重复的版本标题、`release_date` 取发布日期；标题统一为「v版本 + 发布日期」，关于页与新版本提醒窗口共用）

→ `web/src/lib/releaseNotes.ts` + 原生 Android `mobile/Android/core/.../update/UpdateView.kt`（`localizedReleaseBody` / `stripVersionHeading` / `releaseDate`）逐条对齐，`releaseNotes.test.ts` / `UpdateViewTest` 与 Rust 测试为同一组用例；Web 不引入 Markdown 渲染器，`releaseNoteBlocks` 只拆标题 / 列表 / 段落

## 云端推送界面判定（`crates/settings/src/model/cloud_notify.rs`：catalog 开关与标签可见性、卡片状态优先级、用量 0 = 不限 / 满额与重置时间、隐私摘要、默认标签）

→ `web/src/pages/webhooks/cloudLogic.ts`：逐条对齐（`cloudLogic.test.ts` 与 model 测试共用同组样例）。云端只有 email / telegram（依赖 FD 的 SMTP 与官方机器人）；不依赖 FD 基础设施的推送服务一律做成引擎自托管 Webhook 预设（`engine/src/webhook.rs::Preset`），不进云端

## 套餐购买与推介判定（`crates/account/src/billing.rs`：金额格式化、生效价 / 可购买过滤、卡片状态优先级 Current > TierBlocked > CreditBlocked、应付与划线价、倒计时、订单轮询动作、推荐码规范化与创建校验、validate reason / 购买类 `ErrorReason` → 文案键；当前套餐以 `user.plan` 为准）

→ `web/src/pages/settings/sections/account/billing.ts`：逐条对齐（`billing.test.ts` 与 Rust 测试为同一组用例）。推介页入口 = 设备本地偏好 `ui.show_settings_referral`（不进同步目录，缺省 false）且已登录，GPUI 设置侧栏与 Web `categories.ts` 同位（紧随「账户」）
