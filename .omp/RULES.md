# FluxDown 粘性红线

每次请求都携带，只收不可逆操作门槛、分支模型与用户拍板的产品决定；其余规范见项目根 AGENTS.md（会话开头加载）与 `.omp/rules/` 路径规则（编辑命中时注入）：

- UI 维护边界：PC 默认指 GPUI（`crates/*` 包 `fluxdown_ui_*` + `native/{agent,daemon,protocol}`）；headless Web SPA `web/` 同走 agent `/rpc`。移动端为 `mobile/Android` 与 `mobile/FluxDown`，经 `native/mobile` 的 UniFFI 共享核心。旧 Flutter 工程及 hub 已退役，不要求维护或恢复；共享 assets、当前客户端的旧数据/主题/升级兼容与官网旧主题编辑器保留。Android 正式发布沿用 `ANDROID_NATIVE_*` 原生签名，不能覆盖旧 Flutter APK。
- git 写操作门槛 = 用户授权：用户在本会话要求过（含 `/commit`、「提交」「推一下」「发版」）即视为已授权，核对前置条件后直接做完；未要求而想顺手做 → 先问。授权按动作粒度计（提交 ≠ 推送，打 tag ≠ 发布）。推送 v* tag 会立即触发 GitHub Actions 全平台发布流水线，不可逆。
- 分支模型：`main` = 开发分支（超集 / 最新），`stable` = 稳定分支（子集）。日常开发一律在 `main`；禁止直接向 `stable` 提交功能。
- `stable` 只能通过合并 `main`（或从 `main` cherry-pick）前进；hotfix 若直接进 `stable`，必须同回合同步回 `main`。
- 一致性判定：`git log stable --not main --oneline` 必须为空。任何操作后不为空即违规，先修复再继续。
- 稳定 tag `vX.Y.Z` 只从 `stable` 打；预览 tag `vX.Y.Z-rc.N` 只从 `main` 打（CI 有分支守卫，打错分支整条流水线失败）。
- GPUI 下载页侧栏：「状态」文件夹（全部/下载中/已完成/做种中/失败/已暂停；「做种中」对齐 qBittorrent Seeding 过滤器，用户 2026-10-22 决定新增，做种任务同时计入「已完成」）已内嵌分类子项，**不做独立的「分类」分区**（用户 2026-09-09 明确决定）；分类的新建/编辑走状态文件夹内分类子项的右键菜单。
- 123 云盘直链只给 ≥ 0.6.0 的应用内更新（UA `fluxdown-agent|android/<版本>`，空 UA 不给）；官网浏览器下载（含大陆访客）不走 123，大陆走 dl-cn（用户 2026-10-25 决定）。
- BT 防吸血（用户 2026-10-31 决定）：允许把命中规则的 BT 对端违规证据上报 FluxCloud（遥测红线唯一例外，BT 无可避免；独立开关、默认关、UI 显式同意、仅登录用户，不含文件名 / 路径 / 种子名，私有种子不报）；检测规则上线默认「仅观察」，用户逐条开启执行；私有种子默认不检测；云名单 = 现有社区数据（BTN-Collected-Rules，CC BY 4.0 署名，FluxCloud 定时同步）+ 自建证据共识名单（新条目须管理员批准）合成一份 ed25519 签名文档，客户端验签。落点见 `.omp/knowledge/ops.md` 防吸血条。
- 站点代理规则（用户 2026-10-28 决定）：四种代理模式下都生效、首条命中；只有代理「跟随全局」的任务 / 订阅看规则；规则只存设备本地。插件请求（含开发模式插件、插件订阅、登录）同样遵循规则与 Auto 换路，解析与下载同出口（用户 2026-10-29 决定）。细则见 engine.md `proxy_rules.rs` 与 `plugin/call_egress.rs` 条。
- Rust 默认安全：`unsafe`（块 / fn / impl / extern / 属性）仅限经审查、std 与现有直接依赖无**语义等价**安全替代的平台边界；必须 cfg + SAFETY + 最小范围 + 健全的安全接口 + RAII/线程归属，禁止 unsafe Send/Sync。不得为去 unsafe 破坏预分配、并发写、大卷空间或错误语义；`native/{api,protocol,daemon,server,nmh,mobile}` 与 `crates/*`、`scripts/desktop-dev` 的项目源码保持零 unsafe（宏展开/依赖另审：UniFFI 生成胶水为独立审计边界；唯一例外 `crates/app/src/app_icon.rs` 的 macOS `setApplicationIconImage(Some)` 单个调用，用户 2026-10-30 决定）。细则见 `rule://no-unsafe-in-rust`。
