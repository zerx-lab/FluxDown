//! OpenAPI 3.1 规范 —— 由 handler 注解（`#[utoipa::path]`）与 wire 类型
//! （`ToSchema`）派生，代码即文档、永不漂移。
//!
//! 两个消费口：
//! - 运行时：`GET /api/v1/openapi.json`（本机 API 服务器，无鉴权）
//! - 构建期：`cargo run -p fluxdown_api --example gen_openapi` 输出到 stdout，
//!   重定向到 `website-v2/public/openapi.json` 供官网 Scalar 文档页渲染

use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// FluxDown 本机 HTTP API 的 OpenAPI 文档聚合。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "FluxDown API",
        description = "FluxDown HTTP API，用于提交下载、管理任务和连接自动化客户端。\
本机默认地址为 `http://127.0.0.1:17800`，端口可在「设置 → API 服务」中修改。\n\n\
## 接入方式\n\n\
- **脚本接管（takeover）**：接收浏览器脚本提交的下载请求，进入下载确认流程；需提供 `X-FluxDown-Client` 请求头。\n\
- **aria2**：提供兼容的 JSON-RPC 接口，可连接 AriaNg 等 aria2 客户端。\n\
- **MCP**：供 AI 客户端查询和调用下载工具。\n\
- **管理 API（management）**：直接创建和管理下载任务，无需界面确认；支持队列查询与站点凭据管理。\n\
- **任务组、插件与 RSS**：管理多文件下载、插件和自动订阅下载。\n\
- **BT 防吸血（bt）**：查看防吸血状态与事件日志，管理封禁、限速与白名单条目。\n\
- **设备互联（link）**：发现、配对设备，并向已配对设备发送下载任务。\n\n\
## 访问认证\n\n\
管理 API、MCP、任务组、插件和 RSS 接口必须提供访问令牌：\
使用 `Authorization: Bearer <token>` 或 `X-FluxDown-Token: <token>` 请求头。\
设备互联的配对和设备通信接口使用独立认证机制，具体要求见各接口说明。",
        license(name = "MIT", identifier = "MIT")
    ),
    servers((url = "http://127.0.0.1:17800", description = "本机 API 服务（默认端口）")),
    paths(
        crate::server::ping,
        crate::server::takeover_download,
        crate::server::takeover_download_batch,
        crate::server::jsonrpc,
        crate::server::mcp,
        crate::server::api_info,
        crate::server::api_list_tasks,
        crate::server::api_create_task,
        crate::server::api_get_task,
        crate::server::api_delete_task,
        crate::server::api_pause_task,
        crate::server::api_continue_task,
        crate::server::api_rename_task,
        crate::server::api_change_task_url,
        crate::server::api_pause_all,
        crate::server::api_continue_all,
        crate::server::api_list_queues,
        crate::server::api_list_site_auth,
        crate::server::api_get_site_auth,
        crate::server::api_save_site_auth,
        crate::server::api_delete_site_auth,
        crate::server::api_resolve_preview,
        crate::server::api_list_groups,
        crate::server::api_create_group,
        crate::server::api_delete_group,
        crate::server::api_group_pause,
        crate::server::api_group_continue,
        crate::server::api_list_rss_sources,
        crate::server::api_create_rss_source,
        crate::server::api_update_rss_source,
        crate::server::api_delete_rss_source,
        crate::server::api_refresh_rss_source,
        crate::server::api_list_rss_items,
        crate::server::api_rss_item_action,
        crate::server::api_validate_rss_feed,
        crate::server::api_bt_guard_state,
        crate::server::api_bt_guard_entries_list,
        crate::server::api_bt_guard_entries_add,
        crate::server::api_bt_guard_entry_remove,
        crate::server::api_bt_guard_events_list,
        crate::server::api_list_plugins,
        crate::server::api_install_plugin,
        crate::server::api_install_plugin_dev,
        crate::server::api_set_plugin_enabled,
        crate::server::api_update_plugin_settings,
        crate::server::api_plugin_auth,
        crate::server::api_uninstall_plugin,
        crate::server::api_ignore_plugin_retry,
        crate::server::api_market_list,
        crate::server::api_market_install,
        crate::server::api_link_pair_hello,
        crate::server::api_link_pair_reveal,
        crate::server::api_link_pair_confirm,
        crate::server::api_link_create_task,
        crate::server::api_link_info,
        crate::server::api_link_unpair,
        crate::server::api_link_generate_code,
        crate::server::api_link_stop_advertising,
        crate::server::api_link_discovery,
        crate::server::api_link_discovered,
        crate::server::api_link_probe,
        crate::server::api_link_pair_begin,
        crate::server::api_link_pair_finish,
        crate::server::api_link_pair_approve,
        crate::server::api_link_devices,
        crate::server::api_link_remove_device,
        crate::server::api_link_device_tasks,
    ),
    tags(
        (name = "system", description = "服务状态"),
        (name = "takeover", description = "浏览器脚本下载接管"),
        (name = "aria2", description = "aria2 兼容接口"),
        (name = "mcp", description = "MCP 工具调用"),
        (name = "management", description = "下载任务与站点凭据管理"),
        (name = "groups", description = "任务组与文件预览"),
        (name = "plugins", description = "插件安装与管理"),
        (name = "rss", description = "RSS 订阅与自动下载"),
        (name = "bt", description = "BT 防吸血：状态、过滤条目与事件日志"),
        (name = "link", description = "设备发现、配对与任务传输"),
    ),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;

/// 注入两种鉴权方案：`Authorization: Bearer` 与 `X-FluxDown-Token` 头。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
        components.add_security_scheme(
            "tokenHeader",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-FluxDown-Token"))),
        );
    }
}

/// 序列化完整规范为 pretty JSON。
///
/// # Examples
///
/// ```
/// let json = fluxdown_api::openapi::openapi_json();
/// assert!(json.contains("\"openapi\""));
/// assert!(json.contains("/api/v1/tasks"));
/// ```
#[must_use]
pub fn openapi_json() -> String {
    ApiDoc::openapi()
        .to_pretty_json()
        .unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use utoipa::OpenApi;

    use super::ApiDoc;
    use crate::routes;

    /// 漂移守卫：routes.rs 里每个对外路径常量都必须出现在规范中。
    /// handler 注解里的 path 是字面量，新增/改名路由而忘记同步注解时此测试失败。
    #[test]
    fn spec_covers_every_route_constant() {
        let spec = ApiDoc::openapi();
        let documented: Vec<&str> = spec.paths.paths.keys().map(String::as_str).collect();
        let expected = [
            routes::PING,
            routes::DOWNLOAD,
            routes::DOWNLOAD_BATCH,
            routes::JSONRPC,
            routes::MCP,
            routes::API_INFO,
            routes::API_TASKS,
            routes::API_TASK,
            routes::API_TASK_PAUSE,
            routes::API_TASK_CONTINUE,
            routes::API_TASK_RENAME,
            routes::API_TASK_URL,
            routes::API_TASKS_PAUSE,
            routes::API_TASKS_CONTINUE,
            routes::API_QUEUES,
            routes::API_SITE_AUTH,
            routes::API_SITE_AUTH_SITE,
            routes::API_RESOLVE_PREVIEW,
            routes::API_GROUPS,
            routes::API_GROUP,
            routes::API_GROUP_PAUSE,
            routes::API_GROUP_CONTINUE,
            routes::API_RSS,
            routes::API_RSS_SOURCE,
            routes::API_RSS_REFRESH,
            routes::API_RSS_ITEMS,
            routes::API_RSS_ITEM_ACTION,
            routes::API_RSS_VALIDATE,
            routes::API_BT_GUARD_STATE,
            routes::API_BT_GUARD_ENTRIES,
            routes::API_BT_GUARD_ENTRY,
            routes::API_BT_GUARD_EVENTS,
            routes::API_PLUGINS,
            routes::API_PLUGINS_INSTALL,
            routes::API_PLUGINS_INSTALL_DEV,
            routes::API_PLUGIN_ENABLED,
            routes::API_PLUGIN_SETTINGS,
            routes::API_PLUGIN_AUTH,
            routes::API_PLUGIN,
            routes::API_TASK_IGNORE_PLUGIN_RETRY,
            routes::API_MARKET,
            routes::API_MARKET_INSTALL,
            routes::API_LINK_PAIR_HELLO,
            routes::API_LINK_PAIR_REVEAL,
            routes::API_LINK_PAIR_CONFIRM,
            routes::API_LINK_TASKS,
            routes::API_LINK_INFO,
            routes::API_LINK_UNPAIR,
            routes::API_LINK_CODE,
            routes::API_LINK_DISCOVERY,
            routes::API_LINK_DISCOVERED,
            routes::API_LINK_PROBE,
            routes::API_LINK_PAIR_BEGIN,
            routes::API_LINK_PAIR_FINISH,
            routes::API_LINK_PAIR_APPROVE,
            routes::API_LINK_DEVICES,
            routes::API_LINK_DEVICE,
            routes::API_LINK_DEVICE_TASKS,
        ];
        for path in expected {
            assert!(
                documented.contains(&path),
                "route constant {path} missing from OpenAPI spec; \
                 update the #[utoipa::path] annotation in server.rs"
            );
        }
        // API_OPENAPI 本身是文档端点，不自我描述。
        assert_eq!(
            documented.len(),
            expected.len(),
            "spec has undocumented extra paths"
        );
    }

    /// 规范可序列化且含鉴权方案。
    #[test]
    fn spec_serializes_with_security_schemes() {
        let json = super::openapi_json();
        assert!(json.contains("bearerAuth"));
        assert!(json.contains("X-FluxDown-Token"));
    }
}
