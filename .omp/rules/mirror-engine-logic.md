---
description: RSS 过滤 / 文件名解析 / Webhook 事件与端点 / 线程数风险档 / 代理直连名单归一化 / 站点代理规则 / 防吸血名字规则改动须同步 Web、扩展、agent 与各端
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/native/engine/src/rss/filter.rs'
  - '**/web/src/pages/rss/filter{,.test}.ts'
  - '**/native/engine/src/naming/**'
  - '**/fluxDown/utils/filename{,.test}.ts'
  - '**/native/engine/src/webhook.rs'
  - '**/crates/settings/src/sections/webhook.rs'
  - '**/web/src/pages/webhooks/endpoints.ts'
  - '**/native/protocol/src/daemon_config.rs'
  - '**/native/engine/src/segment_coordinator.rs'
  - '**/web/src/lib/threadsRisk.ts'
  - '**/native/engine/src/proxy_config.rs'
  - '**/native/agent/src/system_proxy.rs'
  - '**/native/engine/src/proxy_rules.rs'
  - '**/native/protocol/src/proxy_rules.rs'
  - '**/web/src/lib/{proxyRules,taskRoute}{,.test}.ts'
  - '**/crates/downloads/src/model/task_route.rs'
  - '**/crates/settings/src/sections/proxy_rule{s,_dialog}.rs'
  - '**/mobile/Android/core/src/main/java/com/fluxdown/core/protocol/ProxyRules.kt'
  - '**/mobile/Android/app/src/main/java/com/fluxdown/app/feature/task/TaskDetailFormat.kt'
  - '**/mobile/FluxDown/FluxKit/Sources/FluxDomain/Protocol/ProxyRules.swift'
  - '**/native/engine/src/bt_guard/cloud.rs'
  - '**/crates/settings/src/model/client_rules.rs'
  - '**/crates/downloads/src/model/bt_guard.rs'
  - '**/web/src/lib/btGuardClientRules{,.test}.ts'
  - '**/mobile/FluxDown/FluxDown/Features/Task/TaskDetailText.swift'
interruptMode: never
---

你正在改动引擎侧纯逻辑（RSS 过滤 / 文件名 / Webhook / 线程数 / 代理直连名单 / 站点代理规则）的一侧。下列条目里，标题是事实源一侧、「→」后是必须同回合同步的镜像端：改任一侧都要把另一侧过一遍；有共享夹具的先改夹具用例，再改两侧实现，两侧测试都要跑（`/verify` 第 3 步）。只改了一侧就交付 = 不完整。

## `engine/src/rss/filter.rs`

→ `web/src/pages/rss/filter.ts`（两份逐条对齐，`filter.test.ts` 复用 Rust 用例；预览与实际下载必须一致）

## `native/engine/src/naming/{disposition,charset}.rs`（Content-Disposition 解析、旧式字节打分解码、TLD / 页面字符集先验）

→ `fluxDown/utils/filename.ts`（`parseContentDispositionFilename` / `decodeLegacyBytes`）逐条对齐；跨实现用例 `native/engine/src/naming/fixtures/content_disposition.json` 由 Rust 测试与 `filename.test.ts`（Chrome / Firefox 两种头值模型）共用，改规则先改用例。扩展发来的非空名按显式名处理（只 sanitize），拿不准（脚本端点名等）必须发空串，交给引擎推断并在完成期按实际 GET 响应定名（`tasks.name_inferred`，见 `.omp/knowledge/engine.md`「naming/」）

## `engine/src/webhook.rs` 的 `WebhookEventKind`

→ protocol 与各客户端事件选择器（含 TS `WEBHOOK_EVENTS`），wire 名逐字一致

## `engine/src/webhook.rs` 的 `EndpointSpec` 宽松解析（`lenient` / `reload_endpoints`）

→ GPUI `crates/settings/src/sections/webhook.rs::parse_endpoints` + Web `web/src/pages/webhooks/endpoints.ts::parseEndpoint`：非对象元素跳过、字段类型不符回退默认值；端点写入两端都走「取最新值重算、冲突重放」（GPUI `SettingsStore::mutate_daemon` ↔ Web `patchEndpoints`），不得写回整份旧数组

## 线程数上限与风险档（`native/protocol/src/daemon_config.rs`：`MAX_TASK_SEGMENTS` = 512 / `HIGH_SEGMENTS_WARN_ABOVE` = 64 / `SEVERE_SEGMENTS_WARN_ABOVE` = 256）

→ 引擎 `segment_coordinator::MAX_SEGMENTS`（显式线程数硬上限；Auto 仍由 advisor / `HINT_UNCAP_MAX` 封顶 64）+ `web/src/lib/threadsRisk.ts` + Web `config.ts` 与移动端 `SettingsCatalog` 的 `default_segments` 范围；GPUI / Web 在设置、队列、新建下载三处 > 64 动态展示风险提示（`RevealCallout`）

## `native/engine/src/proxy_config.rs::normalize_no_proxy` 与 `parse_scutil_exceptions`（直连名单归一化、macOS `ExceptionsList`）

→ `native/agent/src/system_proxy.rs`（`normalize_no_proxy` / `parse_scutil`；agent 不依赖引擎，故各持一份）：`*.x` → `.x`、`a.b.*` 与缩写网段 `169.254/16` → CIDR、`<local>` → 环回 + 私网段近似，两侧写法与用例逐条对齐；环回恒直连两侧都保留

## `native/engine/src/proxy_rules.rs` 的站点代理规则宽松解析与 wire 码（配置键 `proxy_rules`）

→ `native/protocol/src/proxy_rules.rs::parse_proxy_rules`（GPUI）+ Web `web/src/lib/proxyRules.ts` + Android `core/.../protocol/ProxyRules.kt` + iOS `FluxDomain/Protocol/ProxyRules.swift`：空串 / 非法 JSON / 非数组 → 空列表，非对象元素跳过，字段缺失或类型错误回退默认值（`enabled=true`、`match=domain`、`action=direct`、其余空串），编码写全 7 个字段（字段顺序不作要求，测试只比较解析后的值）。匹配只在引擎；客户端经 `daemon.config.proxyRoute` 询问，issue `code` / `reason` 与展示映射四端逐条一致（单条「试一试」草稿强制 `enabled=true`，保存检测用 `upsert(当前列表, draft)`）。路由标签先整串匹配 `rule:direct|system|manual|custom` 再走 `:system`/`:manual` 后缀拆分：GPUI `crates/downloads/src/model/task_route.rs`、Web `web/src/lib/taskRoute.ts`、Android `TaskDetailFormat.kt::routeLabel`、iOS `TaskDetailText.swift::routeLabel`

## `native/engine/src/bt_guard/cloud.rs` 的名字规则宽松解析（`ClientRule::compile` / `parse_local_client_rules`，配置键 `bt_guard_client_rules`）

→ GPUI `crates/settings/src/model/client_rules.rs` + `crates/downloads/src/model/bt_guard.rs::append_ban_client_rule` + Web `web/src/lib/btGuardClientRules.ts` + FluxCloud `server/crates/server/src/bt_guard/config.rs::validate_rules`（云名单侧的同一组字段）：空串 / 非法 JSON → 空列表；`field` / `op` / 动作 / `when` 非法、`id` 或 `value` 为空 → 丢该条；动作取 `verdict ?? action`，`when` 缺省按 `seedingOnly` 推出；`banMinutes` 缺省 4320、钳到 1..=525600；最多 200 条。客户端写回只写 8 个字段（`id` / `field` / `op` / `value` / `action` / `when` / `banMinutes` / `reason`），不写旧字段；匹配只在引擎（忽略 ASCII 大小写）。Android / iOS 只镜像设置目录键，不解析规则
