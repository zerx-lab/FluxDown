//! 可移植插件视图协议：节点树、增量 op、DTO，以及 `normalize_view` / `diff_view` /
//! `apply_view_ops` 三个纯函数。节点表的单一事实源是 `plugin_view_schema.json`。
//!
//! 约定：
//! - 节点 id 由 `normalize_view` 分配：根为 `root`，子节点 = 父 id + `/` + (key 或下标)。
//! - `diff_view` 对每个节点按 `[Error?] [Props?] [Rows?] [Children?]` 的顺序输出，然后按新子节点
//!   顺序递归进入保留下来的子节点；`Replace` 之后不再向下递归。
//! - 根节点 `p["__error"]`（字符串）由 `Error` op 写入；下一帧不再带它时，`diff_view` 会在根的
//!   `Props.unset` 里列出 `__error`，`apply_view_ops` 不做隐式清除，因此两者严格互逆。
//! - `apply_view_ops` 返回 `Err` 后树的状态未定义，调用方必须重新取快照。

mod diff;
mod normalize;
mod schema;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use diff::{apply_view_ops, diff_view};
pub use normalize::normalize_view;
pub use schema::{
    VIEW_SCHEMA_JSON, VIEW_TONES_JSON, ViewEventPayloadType, ViewEventSpec, ViewItemSpec,
    ViewKindSpec, ViewLimits, ViewPropFormat, ViewPropSpec, ViewPropType, ViewSchema, view_schema,
};

/// 根节点 id。
pub const VIEW_ROOT_ID: &str = "root";
/// `Error` op 写入根节点的 prop 名。
pub const VIEW_ERROR_PROP: &str = "__error";

/// 归一化后的视图节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ViewNode {
    pub id: String,
    pub k: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub p: Map<String, Value>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub c: Vec<ViewNode>,
}

/// 增量更新 op。`set` / `unset` / `insert` / `upsert` / `remove` 反序列化时可缺省，
/// 序列化时一律输出（`Rows.order` 为 `None` 时省略）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ViewOp {
    /// 整个子树替换（kind 变化）。`id` 为被替换节点的 id，根节点即 `root`。
    Replace { id: String, node: ViewNode },
    /// prop 增删改。`unset` 按字典序排列。
    Props {
        id: String,
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(value_type = Object))]
        set: Map<String, Value>,
        #[serde(default)]
        unset: Vec<String>,
    },
    /// 子节点序列变化。`order` 是变化后完整的子 id 序列；`insert` 只含序列里新出现的子树。
    Children {
        id: String,
        order: Vec<String>,
        #[serde(default)]
        insert: Vec<ViewNode>,
    },
    /// 键控数组（`table.rows` / `list.items`）按 key 增量更新。
    /// 应用顺序：先 `remove`，再 `upsert`（已有 key 原位替换，新 key 追加到末尾），
    /// 最后若有 `order` 则按其重排（必须恰好是现有 key 的排列）。
    Rows {
        id: String,
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(value_type = Vec<Object>))]
        upsert: Vec<Value>,
        #[serde(default)]
        remove: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<Vec<String>>,
    },
    /// 渲染失败：把 `message` 写到根节点 `p["__error"]`。
    Error { message: String },
}

/// 插件请求宿主执行的界面动作（随 [`PluginViewFrameDto::effects`] 下发）。
///
/// 只发给承载该会话的客户端；未知种类反序列化为 [`ViewEffect::Unknown`] 并忽略，
/// 以后新增动作不需要升协议版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ViewEffect {
    /// 切到主界面某个页面：`target` 为路由 id——内置页 [`NAV_BUILTIN_TARGETS`] 之一，
    /// 或本插件活动栏视图 `plugin:<identity>:<viewId>`。
    Navigate { target: String },
    /// 本端不认识的动作。
    #[serde(other)]
    Unknown,
}

/// 插件可跳转的内置主界面页面（与 GPUI 活动栏路由 id、Web 活动栏条目 id 一致）。
pub const NAV_BUILTIN_TARGETS: [&str; 3] = ["downloads", "rss", "webhooks"];

/// `PluginViewClosed.reason`：插件经 `ctx.close()` 请求关闭承载该会话的容器。
pub const VIEW_CLOSE_REASON_REQUESTED: &str = "requested";

/// 插件活动栏视图的路由 id：`plugin:<identity>:<viewId>`（GPUI 动态路由与 Web 外壳条目共用）。
///
/// ```
/// assert_eq!(fluxdown_protocol::plugin_route_id("a@b", "home"), "plugin:a@b:home");
/// ```
#[must_use]
pub fn plugin_route_id(identity: &str, view: &str) -> String {
    format!("plugin:{identity}:{view}")
}

/// `DaemonEvent::PluginViewFrame` 的载荷。
///
/// `effects` 非空、`ops` 为空且 `seq` 等于会话当前序号的帧只携带动作：客户端先执行
/// `effects`，再按序号规则处理 `ops`（旧客户端按「序号不大于当前」整帧忽略）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewFrameDto {
    pub session: String,
    pub seq: u64,
    pub ops: Vec<ViewOp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ViewEffect>,
}

/// `daemon.plugin.view.open` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewOpenParams {
    pub identity: String,
    pub view: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub props: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// 界面语言（BCP 47，如 `zh-CN`）；缺省为空，插件 `ctx.locale` 随之为空串。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// `daemon.plugin.view.open` 返回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewOpenedDto {
    pub session: String,
    pub seq: u64,
    pub tree: ViewNode,
}

/// `daemon.plugin.view.event` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewEventParams {
    pub session: String,
    /// 触发事件的节点 id。
    pub node: String,
    pub event: ViewEventDto,
}

/// 视图事件；字段名对应 schema `events.*.payload`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ViewEventDto {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// 仅含会话 id 的参数（`daemon.plugin.view.close` / `daemon.plugin.view.snapshot`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewSessionParams {
    pub session: String,
}

/// `daemon.plugin.view.snapshot` 返回：会话当前的序号与整棵树。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewSnapshotDto {
    pub seq: u64,
    pub tree: ViewNode,
}

/// `daemon.plugin.view.keepalive` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginViewKeepaliveParams {
    pub sessions: Vec<String>,
}

/// `daemon.plugin.command` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginCommandParams {
    pub identity: String,
    /// manifest `contributes.commands[].id`。
    pub command: String,
    #[serde(default)]
    pub task_ids: Vec<String>,
}

/// `daemon.plugin.invoke` 参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginInvokeParams {
    pub identity: String,
    /// `flux.rpc.handle` 注册的处理器名。
    pub name: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub args: Value,
}

/// `daemon.plugin.pageInvoke` 参数：浏览器页面脚本经扩展 / agent 调用本插件处理器。
/// 处理器须以 `flux.rpc.handle(name, fn, { page: true })` 注册，收到的第二个参数为
/// `{ caller: "page", pageUrl }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginPageInvokeParams {
    pub identity: String,
    pub name: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub args: Value,
    /// 发起调用的页面地址（扩展取自消息发送方，不信任脚本自报）。
    pub page_url: String,
}

/// `daemon.plugin.pageInvoke` 结果：处理器返回值，以及本次调用期间插件经 `flux.tasks.create` /
/// `flux.tasks.resume` 开始的任务。agent 据后两者把用户在浏览器里的操作登记为「用户开始」
/// （与免打扰捕获同一套进度 / 完成窗口规则），扩展只收到 `value`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PluginPageInvokeResultDto {
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub value: Value,
    #[serde(default)]
    pub created_task_ids: Vec<String>,
    #[serde(default)]
    pub resumed_task_ids: Vec<String>,
}

/// 视图协议错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    /// `normalize_view` 拒绝插件产出的树；`path` 是节点 id（可定位到节点）。
    Invalid { path: String, message: String },
    /// 内置 schema 无法解析（构建缺陷，测试会覆盖）。
    Schema(String),
    /// `apply_view_ops` 无法应用 op。
    Apply { message: String },
}

impl ViewError {
    pub(crate) fn invalid(path: &str, message: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.to_owned(),
            message: message.into(),
        }
    }

    pub(crate) fn apply(message: impl Into<String>) -> Self {
        Self::Apply {
            message: message.into(),
        }
    }
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { path, message } => write!(f, "invalid view node {path}: {message}"),
            Self::Schema(message) => write!(f, "view schema is unusable: {message}"),
            Self::Apply { message } => write!(f, "cannot apply view ops: {message}"),
        }
    }
}

impl std::error::Error for ViewError {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::{PluginViewFrameDto, PluginViewOpenParams, ViewEffect, ViewEventDto, ViewOp};

    #[test]
    fn plugin_view_op_wire_shape_is_tagged_camel_case() {
        let rows = ViewOp::Rows {
            id: "root/t".into(),
            upsert: vec![json!({"key": "a"})],
            remove: vec![],
            order: None,
        };
        assert_eq!(
            serde_json::to_value(&rows).unwrap(),
            json!({"op": "rows", "id": "root/t", "upsert": [{"key": "a"}], "remove": []})
        );
        let error = ViewOp::Error {
            message: "boom".into(),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"op": "error", "message": "boom"})
        );
        let sparse: ViewOp = serde_json::from_value(json!({"op": "props", "id": "root"})).unwrap();
        assert_eq!(
            sparse,
            ViewOp::Props {
                id: "root".into(),
                set: Default::default(),
                unset: vec![]
            }
        );
    }

    #[test]
    fn plugin_view_dtos_use_camel_case() {
        let params: PluginViewOpenParams =
            serde_json::from_value(json!({"identity": "a.b", "view": "v", "taskId": "t1"}))
                .unwrap();
        assert_eq!(params.task_id.as_deref(), Some("t1"));
        assert!(params.props.is_null());

        let event: ViewEventDto =
            serde_json::from_value(json!({"type": "change", "value": true})).unwrap();
        assert_eq!(event.kind, "change");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"type": "change", "value": true})
        );
    }

    #[test]
    fn view_frame_effects_are_optional_and_tolerate_unknown_kinds() {
        // 旧服务端的帧没有 effects；无动作的帧也不输出该字段（旧客户端看到的形状不变）。
        let legacy: PluginViewFrameDto =
            serde_json::from_value(json!({"session": "s", "seq": 3, "ops": []})).unwrap();
        assert!(legacy.effects.is_empty());
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            json!({"session": "s", "seq": 3, "ops": []})
        );

        let frame: PluginViewFrameDto = serde_json::from_value(json!({
            "session": "s", "seq": 3, "ops": [],
            "effects": [
                {"kind": "navigate", "target": "downloads"},
                {"kind": "openSheet", "id": "x"}
            ]
        }))
        .unwrap();
        assert_eq!(
            frame.effects,
            vec![
                ViewEffect::Navigate {
                    target: "downloads".into()
                },
                ViewEffect::Unknown
            ]
        );
    }
}
