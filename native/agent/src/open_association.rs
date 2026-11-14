//! 系统默认处理程序交来的 `.torrent` / `magnet:` / `ed2k://`：用户已在设置中关闭对应关联时
//! 不建任务，改发系统通知说明。
//!
//! 关闭关联时 agent 会把系统默认处理程序移交给其他已安装的客户端；macOS Launch Services
//! 没有「无默认处理程序」状态，FluxDown 是唯一候选时系统仍会把它们交给 FluxDown，只能
//! 在捕获入口按 opt-out 偏好拦截。请求经 `association` 参数声明来源关联
//! （[`OpenAssociation`] wire 名），未声明的请求（界面拖入、剪贴板、扩展）不受约束。

use std::sync::Arc;

use fluxdown_protocol::capture_link::OpenAssociation;
use fluxdown_protocol::{AgentSnapshot, ApplicationErrorCode, RpcErrorData};

use crate::event_hub::AgentEventHub;
use crate::notification::{NoticeText, Notifier};

/// 捕获请求参数中声明来源关联的字段。
const ASSOCIATION_PARAM: &str = "association";
/// 拦截通知正文的文案键；标题复用设置页对应开关的标题。
const IGNORED_BODY_KEY: &str = "associationOffIgnored";

pub struct OpenAssociationGuard {
    events: AgentEventHub,
    notifier: Arc<Notifier>,
    text: NoticeText,
}

impl OpenAssociationGuard {
    #[must_use]
    pub fn new(events: AgentEventHub, notifier: Arc<Notifier>) -> Self {
        Self {
            events,
            notifier,
            text: NoticeText,
        }
    }

    /// 请求声明的来源关联已被用户关闭 → 发系统通知并返回 `true`，调用方不得建任务。
    pub fn intercept(&self, params: &serde_json::Value) -> Result<bool, RpcErrorData> {
        let Some(association) = requested_association(params)? else {
            return Ok(false);
        };
        let (disabled, locale) = self.events.inspect(|snapshot| {
            (
                opted_out(snapshot, association),
                crate::background_effects::locale_preference(snapshot),
            )
        });
        if !disabled {
            return Ok(false);
        }
        tracing::info!(
            ?association,
            "association turned off by user; ignoring system open"
        );
        let title = self.text.text(title_key(association), locale.as_deref());
        let body = self.text.text(IGNORED_BODY_KEY, locale.as_deref());
        let notifier = Arc::clone(&self.notifier);
        tokio::task::spawn_blocking(move || notifier.show(&title, &body));
        Ok(true)
    }
}

fn requested_association(
    params: &serde_json::Value,
) -> Result<Option<OpenAssociation>, RpcErrorData> {
    match params.get(ASSOCIATION_PARAM) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|_| RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)),
    }
}

fn opted_out(snapshot: &AgentSnapshot, association: OpenAssociation) -> bool {
    snapshot
        .preferences
        .values
        .get(association.opt_out_pref_key())
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// 设置页对应开关的标题键。
const fn title_key(association: OpenAssociation) -> &'static str {
    match association {
        OpenAssociation::Torrent => "torrentFileAssociation",
        OpenAssociation::Magnet => "magnetLinkAssociation",
        OpenAssociation::Ed2k => "ed2kLinkAssociation",
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::AgentSnapshot;
    use serde_json::json;

    use super::*;

    fn snapshot_with(key: &str, value: serde_json::Value) -> AgentSnapshot {
        let mut snapshot = AgentSnapshot::default();
        snapshot.preferences.values.insert(key.to_owned(), value);
        snapshot
    }

    #[test]
    fn only_declared_system_opens_are_subject_to_opt_out() {
        assert_eq!(requested_association(&json!({ "silent": true })), Ok(None));
        assert_eq!(
            requested_association(&json!({ "association": "ed2k" })),
            Ok(Some(OpenAssociation::Ed2k))
        );
        assert!(requested_association(&json!({ "association": "http" })).is_err());
    }

    #[test]
    fn opt_out_is_per_association() {
        let snapshot = snapshot_with("magnet_assoc_user_disabled", json!(true));
        assert!(opted_out(&snapshot, OpenAssociation::Magnet));
        assert!(!opted_out(&snapshot, OpenAssociation::Ed2k));
        assert!(!opted_out(&snapshot, OpenAssociation::Torrent));
        let reenabled = snapshot_with("magnet_assoc_user_disabled", json!(false));
        assert!(!opted_out(&reenabled, OpenAssociation::Magnet));
    }
}
