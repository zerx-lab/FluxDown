//! 路由路径常量 —— server 与 Rust 客户端（如未来 MCP server）共用同一份，
//! 保证请求地址永不漂移。
//!
//! 参数占位符使用 axum 0.8 语法 `{id}`；客户端侧用
//! [`task_path`] 等辅助函数生成实际路径。
//!
//! # Examples
//!
//! ```
//! use fluxdown_api::routes;
//!
//! assert_eq!(routes::API_TASKS, "/api/v1/tasks");
//! assert_eq!(routes::task_path("abc"), "/api/v1/tasks/abc");
//! ```

/// 探活（无鉴权）。
pub const PING: &str = "/ping";
/// 油猴脚本接管：单任务。
pub const DOWNLOAD: &str = "/download";
/// 油猴脚本接管：批量。
pub const DOWNLOAD_BATCH: &str = "/download/batch";
/// aria2 JSON-RPC 兼容端点。
pub const JSONRPC: &str = "/jsonrpc";
/// MCP（Model Context Protocol）兼容端点。
pub const MCP: &str = "/mcp";

/// 管理 API 版本前缀。
pub const API_PREFIX: &str = "/api/v1";
/// 应用信息。
pub const API_INFO: &str = "/api/v1/info";
/// 任务集合（GET 列表 / POST 创建）。
pub const API_TASKS: &str = "/api/v1/tasks";
/// 单任务（GET / DELETE）。
pub const API_TASK: &str = "/api/v1/tasks/{id}";
/// 暂停单任务（PUT）。
pub const API_TASK_PAUSE: &str = "/api/v1/tasks/{id}/pause";
/// 恢复单任务（PUT）。
pub const API_TASK_CONTINUE: &str = "/api/v1/tasks/{id}/continue";
/// 重命名任务文件（POST，body `{"fileName"}`）。
pub const API_TASK_RENAME: &str = "/api/v1/tasks/{id}/rename";
/// 更换任务下载源地址（PUT，body `{"url"}`）。
pub const API_TASK_URL: &str = "/api/v1/tasks/{id}/url";
/// 暂停全部（PUT）。
pub const API_TASKS_PAUSE: &str = "/api/v1/tasks/pause";
/// 恢复全部（PUT）。
pub const API_TASKS_CONTINUE: &str = "/api/v1/tasks/continue";
/// 队列列表（GET）。
pub const API_QUEUES: &str = "/api/v1/queues";
/// OpenAPI 3.1 规范文档（GET，无鉴权）。
pub const API_OPENAPI: &str = "/api/v1/openapi.json";
/// 已保存站点凭据列表/保存（GET/PUT）。
pub const API_SITE_AUTH: &str = "/api/v1/site-auth";
/// 单站点凭据详情/删除（GET/DELETE）。
pub const API_SITE_AUTH_SITE: &str = "/api/v1/site-auth/{site}";

/// 插件集合（GET 列表）。
pub const API_PLUGINS: &str = "/api/v1/plugins";
/// 安装插件（POST zip bytes，≤4MB，受全局请求体上限约束）。
pub const API_PLUGINS_INSTALL: &str = "/api/v1/plugins/install";
/// 安装 dev 插件（POST {dirPath}）。
pub const API_PLUGINS_INSTALL_DEV: &str = "/api/v1/plugins/install-dev";
/// 单插件启用开关（PUT {enabled}）。
pub const API_PLUGIN_ENABLED: &str = "/api/v1/plugins/{identity}/enabled";
/// 单插件设置（PUT {key:value}）。
pub const API_PLUGIN_SETTINGS: &str = "/api/v1/plugins/{identity}/settings";
/// 驱动插件登录流程（POST begin/poll/cancel/logout/status）。
pub const API_PLUGIN_AUTH: &str = "/api/v1/plugins/{identity}/auth";
/// 卸载单插件（DELETE）。
pub const API_PLUGIN: &str = "/api/v1/plugins/{identity}";
/// 任务级逃生舱：忽略插件重试，按原始链接重跑（POST）。
pub const API_TASK_IGNORE_PLUGIN_RETRY: &str = "/api/v1/tasks/{id}/ignore-plugin-retry";

/// 去中心化插件市场：拉取索引（GET）。
pub const API_MARKET: &str = "/api/v1/market";
/// 从市场安装（POST {pluginId}）。
pub const API_MARKET_INSTALL: &str = "/api/v1/market/install";

/// 前置预解析清单（POST，只读、不建任务）。
pub const API_RESOLVE_PREVIEW: &str = "/api/v1/resolve/preview";
/// 任务组集合（GET 列表 / POST 建组+子任务）。
pub const API_GROUPS: &str = "/api/v1/groups";
/// 单任务组（DELETE，query `deleteFiles=true` 可选同时删文件）。
pub const API_GROUP: &str = "/api/v1/groups/{id}";
/// 暂停组内成员（PUT）。
pub const API_GROUP_PAUSE: &str = "/api/v1/groups/{id}/pause";
/// 恢复组内成员（PUT）。
pub const API_GROUP_CONTINUE: &str = "/api/v1/groups/{id}/continue";

/// RSS 订阅集合（GET 列表 / POST 新建）。
pub const API_RSS: &str = "/api/v1/rss";
/// 单个 RSS 订阅（PUT 更新 / DELETE 删除）。
pub const API_RSS_SOURCE: &str = "/api/v1/rss/{id}";
/// 立即抓取一个订阅（POST）。
pub const API_RSS_REFRESH: &str = "/api/v1/rss/{id}/refresh";
/// 一个订阅的条目流（GET）。
pub const API_RSS_ITEMS: &str = "/api/v1/rss/{id}/items";
/// 对条目执行手动操作（POST，guid 在请求体里——真实 guid 常是整条 URL）。
pub const API_RSS_ITEM_ACTION: &str = "/api/v1/rss/{id}/items/action";
/// 只读验证一个 feed 地址（POST，新建订阅向导）。
pub const API_RSS_VALIDATE: &str = "/api/v1/rss/validate";

// -- BT 防吸血（均需 management token）--

/// 防吸血状态总览（GET）：开关 / 今日统计 / 云名单状态 / 过滤订阅 / 各层条目数。
pub const API_BT_GUARD_STATE: &str = "/api/v1/bt/guard/state";
/// 过滤条目（GET 分页列出；POST 手动添加封禁 / 限速 / 阻塞或白名单条目）。
pub const API_BT_GUARD_ENTRIES: &str = "/api/v1/bt/guard/entries";
/// 单个过滤条目（DELETE 移除；云名单层条目不可删，应改为加白）。
pub const API_BT_GUARD_ENTRY: &str = "/api/v1/bt/guard/entries/{id}";
/// 防吸血事件日志（GET 分页，按时间降序）。
pub const API_BT_GUARD_EVENTS: &str = "/api/v1/bt/guard/events";

/// P2P 设备互联：发起配对握手（POST，无 token 鉴权，由一次性配对码守卫）。
pub const API_LINK_PAIR_HELLO: &str = "/api/v1/link/pair/hello";
/// P2P 设备互联：揭示发起方在 hello 里承诺过的临时公钥（POST，无 token 鉴权，由
/// `pair/hello` 建立的会话 id + 公钥承诺守卫）。
pub const API_LINK_PAIR_REVEAL: &str = "/api/v1/link/pair/reveal";
/// P2P 设备互联：确认/拒绝配对（POST，无 token 鉴权，由会话 + SAS 守卫）。
pub const API_LINK_PAIR_CONFIRM: &str = "/api/v1/link/pair/confirm";
/// P2P 设备互联：已配对设备下发下载任务（POST，链路 HMAC 鉴权，非 token；请求与响应体均为
/// AEAD 密文，`X-FluxLink-Enc: v2`）。
pub const API_LINK_TASKS: &str = "/api/v1/link/tasks";
/// P2P 设备互联：已配对设备经已认证链路双向交换设备信息（名称 / 平台 / 版本 / 回连地址 /
/// 云设备 ID / 默认下载目录 / 路径风格）（POST，链路 HMAC 鉴权，请求与响应体均为 AEAD 密文）。
pub const API_LINK_INFO: &str = "/api/v1/link/info";
/// P2P 设备互联：已配对设备通知本机「我已解除与你的配对」（POST，链路 HMAC 鉴权，请求与响应体
/// 均为 AEAD 密文）。收到后本机把对应记录标记为已撤销（拒绝其入站请求、不再向它下发）。
pub const API_LINK_UNPAIR: &str = "/api/v1/link/unpair";
/// P2P 设备互联：生成一次性配对码（POST，**需 management token**，供 web/CLI 让
/// headless 设备出示配对码）。
pub const API_LINK_CODE: &str = "/api/v1/link/code";

// -- 本地互联管理面（web/PC 一致驱动 LinkManager；均需 management token）--

/// 本地设备发现开关（POST `{"action":"start"|"stop"}`，**需 management token**）。
/// start 幂等，且会清空发现快照。
pub const API_LINK_DISCOVERY: &str = "/api/v1/link/discovery";
/// 当前发现快照（GET，**需 management token**）。
pub const API_LINK_DISCOVERED: &str = "/api/v1/link/discovered";
/// 手动地址探测（POST `{"host","port"}`，**需 management token**；结果不入
/// 发现快照，直接返回给调用方）。
pub const API_LINK_PROBE: &str = "/api/v1/link/probe";
/// 发起配对（POST `{"host","port","code"}`，**需 management token**）。
pub const API_LINK_PAIR_BEGIN: &str = "/api/v1/link/pair/begin";
/// SAS 核对后确认/拒绝配对（POST `{"token","accept"}`，**需 management token**）。
pub const API_LINK_PAIR_FINISH: &str = "/api/v1/link/pair/finish";
/// 批准/拒绝一次入站配对核验（POST `{"sessionId","accept"}`，**需 management
/// token**；响应本机收到的 `IncomingPairing` 通知，区别于发起方视角的
/// [`API_LINK_PAIR_FINISH`]）。
pub const API_LINK_PAIR_APPROVE: &str = "/api/v1/link/pair/approve";
/// 已配对设备列表（GET，**需 management token**；含并发在线探测）。
pub const API_LINK_DEVICES: &str = "/api/v1/link/devices";
/// 单个已配对设备（DELETE 解除配对，**需 management token**；不存在 404）。
pub const API_LINK_DEVICE: &str = "/api/v1/link/devices/{fingerprint}";
/// 已配对设备下发下载任务（POST，**需 management token**；区别于数据面链路
/// HMAC 鉴权的 [`API_LINK_TASKS`]）。
pub const API_LINK_DEVICE_TASKS: &str = "/api/v1/link/devices/{fingerprint}/tasks";

/// 生成单任务路径（客户端用）。
#[must_use]
pub fn task_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}")
}

/// 生成暂停单任务路径（客户端用）。
#[must_use]
pub fn task_pause_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/pause")
}

/// 生成恢复单任务路径（客户端用）。
#[must_use]
pub fn task_continue_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/continue")
}

/// 生成重命名任务文件路径（客户端用）。
#[must_use]
pub fn task_rename_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/rename")
}

/// 生成更换任务下载源地址路径（客户端用）。
#[must_use]
pub fn task_url_path(task_id: &str) -> String {
    format!("{API_TASKS}/{task_id}/url")
}

/// 生成单任务组路径（客户端用）。
#[must_use]
pub fn group_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}")
}

/// 生成暂停任务组路径（客户端用）。
#[must_use]
pub fn group_pause_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}/pause")
}

/// 生成恢复任务组路径（客户端用）。
#[must_use]
pub fn group_continue_path(group_id: &str) -> String {
    format!("{API_GROUPS}/{group_id}/continue")
}

/// 生成单个 RSS 订阅路径（客户端用）。
#[must_use]
pub fn rss_source_path(source_id: &str) -> String {
    format!("{API_RSS}/{source_id}")
}

/// 生成立即抓取订阅路径（客户端用）。
#[must_use]
pub fn rss_refresh_path(source_id: &str) -> String {
    format!("{API_RSS}/{source_id}/refresh")
}

/// 生成订阅条目流路径（客户端用）。
#[must_use]
pub fn rss_items_path(source_id: &str) -> String {
    format!("{API_RSS}/{source_id}/items")
}

/// 生成条目手动操作路径（客户端用；guid 走请求体，不进路径）。
#[must_use]
pub fn rss_item_action_path(source_id: &str) -> String {
    format!("{API_RSS}/{source_id}/items/action")
}

/// 生成单个防吸血过滤条目路径（客户端用）。
#[must_use]
pub fn bt_guard_entry_path(entry_id: i64) -> String {
    format!("{API_BT_GUARD_ENTRIES}/{entry_id}")
}

/// 生成单个已配对设备路径（客户端用）。
#[must_use]
pub fn link_device_path(fingerprint: &str) -> String {
    format!("/api/v1/link/devices/{fingerprint}")
}

/// 生成已配对设备下发任务路径（客户端用）。
#[must_use]
pub fn link_device_tasks_path(fingerprint: &str) -> String {
    format!("/api/v1/link/devices/{fingerprint}/tasks")
}
