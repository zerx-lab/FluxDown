---
description: 插件 manifest / 视图协议 / 贡献点授权 / 市场排序 / 嗅探规则层 / 页面脚本 / 扩展规则改动须同步 SDK、Web、扩展与外部仓
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/native/engine/src/plugin/**'
  - '**/packages/plugin-sdk/**'
  - '**/native/protocol/src/plugin_view/**'
  - '**/native/protocol/tests/fixtures/plugin_view_apply.json'
  - '**/web/src/lib/rpc/protocol/{pluginView,plugin}.ts'
  - '**/crates/plugin_ui/src/**'
  - '**/web/src/pages/plugins/**'
  - '**/crates/extensions/src/market.rs'
  - '**/crates/extensions/tests/fixtures/**'
  - '**/web/src/pages/settings/sections/extensions/**'
  - '**/native/agent/src/nmh.rs'
  - '**/fluxDown/rules/**'
  - '**/fluxDown/background/{host,rule-sync,rules-store}.ts'
  - '**/fluxDown/background/page-scripts/**'
  - '**/fluxDown/utils/nmh-dedicated.ts'
  - '**/website-v2/src/content/docs/*/plugins/**'
interruptMode: never
---

你正在改动插件系统与扩展规则的一侧。下列条目里，标题是事实源一侧、「→」后是必须同回合同步的镜像端：改任一侧都要把另一侧过一遍；有共享夹具的先改夹具用例，再改两侧实现，两侧测试都要跑（`/verify` 第 3 步）。只改了一侧就交付 = 不完整。

## 插件 manifest 规则（`native/engine/src/plugin/manifest.rs`：`main` / `contributes` / 权限 / `risk()` / `network_hosts()` / sniffRules 内容）与安装限制（`install.rs` / `market.rs`）

→ 先改跨实现夹具 `native/engine/src/plugin/fixtures/manifest_cases.json`；SDK `packages/plugin-sdk/src/manifest.ts` 逐条对齐（`manifest.test.ts` 共用夹具；包内容检查只加在 `validatePackage`、审核摘要只加在 `reviewOf`），发布 SDK（`plugin-sdk-v*` tag）后插件索引仓 `sync-validator` 工作流自动同步 `vendor/fluxdown-plugin-manifest.mjs`（索引仓禁止手改，CI 校验与 npm 发布物逐字节一致）；官网 `website-v2/src/content/docs/{en,zh}/plugins/{manifest,permissions}.md`

## 插件视图协议（`native/protocol/src/plugin_view/`：`normalize_view` / `diff_view` / `apply_view_ops`；节点表 `plugin_view_schema.json`、色调 `plugin_view_tones.json`）

→ Web `web/src/lib/rpc/protocol/pluginView.ts::applyViewOps` 与 Rust 共用 `native/protocol/tests/fixtures/plugin_view_apply.json`；渲染器 GPUI `crates/plugin_ui/src/portable/render.rs` ↔ Web `web/src/pages/plugins/view/nodes/*`（色调表两端测试对照同一 JSON）；SDK `bun run gen` 重新生成 `src/types/view.d.ts`（CI `gen:check` 守漂移）；宿主 JS API（`quickjs_host.rs::HOST_PRELUDE`）↔ SDK `src/types/flux.d.ts`；桌面 `fluxdown` 模块 ↔ `cargo run -p fluxdown_ui_plugin_ui --example gen_plugin_typings` → `packages/plugin-sdk/types/desktop.d.ts`

## 插件贡献点与授权判定（`crates/plugin_ui/src/contrib.rs::Contributions::from_plugins`、`crates/extensions/src/market.rs::{risk_of, consent_matches, consent_flow}`）

→ Web `web/src/pages/plugins/contributions.ts`（同纳入条件、活动栏 id `plugin:<identity>:<view>` 同 GPUI 并进 `chromeCatalog.ts`）+ `web/src/pages/settings/sections/extensions/logic.ts::{riskOf, consentMatches, postInstallConsent}`，Rust 与 TS 共用 `crates/extensions/tests/fixtures/consent_cases.json`；引擎 `PluginManifest::risk()` 是同一规则的服务端事实源

## 插件市场排序 / 收藏筛选（`crates/extensions/src/market.rs::{MarketSort, arrange_market}`；收藏数据经 agent `agent.pluginFavorite.{list,set}` 读写 FluxCloud `/api/v1/plugins/favorites*`）

→ `web/src/pages/settings/sections/extensions/logic.ts::arrangeMarket` 逐条对齐，Rust 与 TS 共用 `crates/extensions/tests/fixtures/market_sort_cases.json`（改规则先改用例）；DTO `PluginFavorites*` ↔ `web/src/lib/rpc/protocol/agent.ts`

## 私有扩展商店（FluxCloud `/api/v1/private-plugins*`；`native/protocol` 的 `agent.privatePlugin.{list,install}` / `daemon.plugin.{installPrivate,revokePrivate}`、`MarketEntryDto.{channel,expiresAt}`、`PluginDto.source`、`PrivatePlugin*Dto`；设计稿 `docs/private-plugin-store-design.md`）

→ `web/src/lib/rpc/protocol/{agent,plugin,method}.ts` + `methods/agent.ts::privatePlugin` 手写镜像；GPUI `crates/extensions/src/{controller,market}.rs`、`pages/plugins.rs`（「专属插件」分区）↔ Web `web/src/pages/settings/sections/extensions/{PrivateSection,MarketSection,logic}.tsx|ts` 同一行为：未登录 = 登录提示、已登录无条目 = 整区隐藏、加载 / 错误 = 标题 + 条 + 重试；安装 / 更新走与公开市场同一套 consent（`consent_cases.json`），区别只在调用 `agent.privatePlugin.install`；`source === "private"` 的已装插件不得显示 / 使用公开市场的更新、撤回信息（`market_action` / `installed_version_yanked` ↔ `logic.ts` 对应函数，两侧测试同步）。引擎侧来源标记 `plugin.<identity>.source = private`（`PluginManager::{install_private_checked,is_private_source,apply_private_revocations}`、`market::install_private_package`）：`MarketClient::install_entry` 拒绝私有来源、`apply_market_revocations` 跳过私有来源；`daemon.plugin.{installPrivate,revokePrivate}` 为 agent 专用（Gateway `internal_daemon_method` 拒绝透传）；`revokePrivate` 的可选 `scope`（`PluginRevokePrivateParams.scope`，缺省 = 旧云端 = 只增不清）对应云端 `GET /private-plugins` 的 `scope`，仅在 agent ↔ daemon 之间流动，Web TS 镜像无需同步；下载票据路径由 `fluxdown_logfile::SANITIZE_PATTERNS` 脱敏

## 插件嗅探规则层（`native/engine/src/plugin/sniff_pack.rs`：id 改写、sequence 只增、`epoch` 随序号持久化并在数据库重置后改变、聚合体积 ≤ 512 KiB、规则文件按 mtime 缓存；agent `nmh.rs` 的 `sniff_rules.{plugin,pluginSequence,pluginEpoch,deferred}` / `capabilities.{pluginRulesSequence,pluginRulesEpoch}`，`have.pluginEpoch` 与 daemon 不同则无视 `have.plugin`，应答超 900 KiB 时后面的文档置 null 并列入 `deferred`；`PluginSniffRulesDto` 同步 `web/src/lib/rpc/protocol/plugin.ts`）

→ 扩展 `fluxDown/rules/{schema,validate}.ts`（`plugin` 层：`TIER_RANK` local < official < plugin < community < builtin、community 全部限制 + 禁 request headers）与 `background/{host,rule-sync,rules-store}.ts`（同 epoch 内低序号视为回滚拒收、epoch 变化整体替换 / 清空 plugin 层、`deferred` 补取；`ruleSyncEnabled` 关闭时只发 `pluginOnly` 请求）

## 插件页面脚本（`native/engine/src/plugin/page_scripts.rs`：门槛同嗅探规则层、索引含每条 sha256、`plugin.page_scripts.{sequence,hash,epoch}`；agent `nmh.rs` 的 `page_scripts` / `page_script_source` / `plugin_invoke` 与 `capabilities.{pageScriptsSequence,pageScriptsEpoch}`；`PluginPageScript*Dto`）

→ 扩展 `fluxDown/background/page-scripts/*`（注册模型：每插件 `worldId` = `fd.<identity 的 @ 换成 .>`、isolated 为「启动数据 + `page-script-runtime.js` + 源码」三段、main 无桥；令牌认证来源插件）与运行时 `background/page-scripts/runtime-core.ts` 的 `fluxdown` 桥 ↔ SDK `packages/plugin-sdk/types/page.d.ts` ↔ 官网 `plugins/page-scripts-guide.md`（en/zh）逐项一致，桥能力只加不改；`plugin_invoke` 必须走每插件专用 NMH 端口（`utils/nmh-dedicated.ts`；中继与 agent 连接都是一问一答串行）

## 扩展规则校验 / 求值（`fluxDown/rules/{schema,validate,engine,glob,url}.ts` 与 `builtin.json`；社区规则产出地址 / mirror 站点范围约束）

→ 规则仓 `zerx-lab/fluxdown-sniff-rules` 的 `vendor/fluxdown-sniff-rules.mjs`：`bun scripts/sniff-rules/sync-repo.ts vendor --repo <规则仓>` 重新打包并提交到规则仓（`--check` 校验漂移），规则仓 CI、登记仓库收录与作者自检（`author-check` 可复用工作流）才与扩展判定一致。站点专属规则只进规则仓 `rules/official/`；官方包发布后 `sync-repo.ts seed` 刷新扩展快照 `fluxDown/rules/official.json`（勿手改）。社区 id 命名空间（`community.<ns>.*`，扩展「提交到社区」固定 `community.misc.*`）↔ 规则仓 `scripts/{lib,submit}.mjs`
