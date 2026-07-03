//! OpenAPI 3.1 规范 —— 由 handler 注解（`#[utoipa::path]`）与 wire 类型
//! （`ToSchema`）派生，代码即文档、永不漂移。
//!
//! 两个消费口：
//! - 运行时：`GET /api/v1/openapi.json`（本机 API 服务器，无鉴权）
//! - 构建期：`cargo run -p fluxdown_api --example gen_openapi` 输出到 stdout，
//!   重定向到 `website/public/openapi.json` 供官网 Scalar 文档页渲染

use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// FluxDown 本机 HTTP API 的 OpenAPI 文档聚合。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "FluxDown API",
        description = "FluxDown 桌面应用的本机 HTTP API。仅监听 `127.0.0.1`（默认端口 17800，\
可在 设置 → API 服务 中修改）。\n\n\
- **takeover**（脚本接管）：油猴脚本提交下载，进入快速下载确认流程；需 `X-FluxDown-Client` 头\n\
- **aria2**：aria2 JSON-RPC 兼容垫片，「发送到 aria2」类脚本与 AriaNg 可直接对接\n\
- **management**（管理 API）：任务查询/创建/暂停/恢复/删除与队列查询，供 MCP/自动化客户端使用；\
强制要求 token（`Authorization: Bearer <token>` 或 `X-FluxDown-Token` 头）",
        license(name = "MIT", identifier = "MIT")
    ),
    servers((url = "http://127.0.0.1:17800", description = "本机 API 服务（默认端口）")),
    paths(
        crate::server::ping,
        crate::server::takeover_download,
        crate::server::takeover_download_batch,
        crate::server::jsonrpc,
        crate::server::api_info,
        crate::server::api_list_tasks,
        crate::server::api_create_task,
        crate::server::api_get_task,
        crate::server::api_delete_task,
        crate::server::api_pause_task,
        crate::server::api_continue_task,
        crate::server::api_pause_all,
        crate::server::api_continue_all,
        crate::server::api_list_queues,
    ),
    tags(
        (name = "system", description = "探活与基础信息"),
        (name = "takeover", description = "浏览器脚本接管（Tampermonkey / Violentmonkey）"),
        (name = "aria2", description = "aria2 JSON-RPC 兼容"),
        (name = "management", description = "管理 API（强制 token）"),
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
            routes::API_INFO,
            routes::API_TASKS,
            routes::API_TASK,
            routes::API_TASK_PAUSE,
            routes::API_TASK_CONTINUE,
            routes::API_TASKS_PAUSE,
            routes::API_TASKS_CONTINUE,
            routes::API_QUEUES,
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
