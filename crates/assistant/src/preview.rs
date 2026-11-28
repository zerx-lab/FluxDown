//! 确认卡片的可读预览：把 `AssistantToolPreviewDto` 渲染成「动作名 + 任务 / 设置 / 地址 / 补充说明」。
//!
//! 只负责正文；危险样式、按钮与倒计时由调用方（确认卡片）处理。

use fluxdown_protocol::AssistantToolPreviewDto;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Div, FontWeight, IntoElement as _, ParentElement as _, Styled as _, div,
};
use gpui_component::{h_flex, v_flex};

use crate::model::Translate;

/// 预览里没有可显示内容的字段占位（设置旧值缺失）。
const NO_PREVIOUS_VALUE: &str = "—";

/// 协议固定的动作标识 → `assistantPreviewAction*` 文案键；未知动作返回 `None`（调用方回退原文）。
pub(crate) fn action_label_key(action: &str) -> Option<&'static str> {
    Some(match action {
        "create" => "assistantPreviewActionCreate",
        "pause" => "assistantPreviewActionPause",
        "resume" => "assistantPreviewActionResume",
        "delete" => "assistantPreviewActionDelete",
        "moveToQueue" => "assistantPreviewActionMoveToQueue",
        "edit" => "assistantPreviewActionEdit",
        "restart" => "assistantPreviewActionRestart",
        "changeUrl" => "assistantPreviewActionChangeUrl",
        "recheck" => "assistantPreviewActionRecheck",
        "reannounce" => "assistantPreviewActionReannounce",
        "setSeedLimits" => "assistantPreviewActionSetSeedLimits",
        "setFilePriorities" => "assistantPreviewActionSetFilePriorities",
        "updateSettings" => "assistantPreviewActionUpdateSettings",
        "queueStart" => "assistantPreviewActionQueueStart",
        "queueStop" => "assistantPreviewActionQueueStop",
        "queueReorder" => "assistantPreviewActionQueueReorder",
        "installComponent" => "assistantPreviewActionInstallComponent",
        "setPluginEnabled" => "assistantPreviewActionSetPluginEnabled",
        "repair" => "assistantPreviewActionRepair",
        "measureRoutes" => "assistantPreviewActionMeasureRoutes",
        _ => return None,
    })
}

/// 预览是否带有任何可渲染内容（动作名之外）。为 `false` 时调用方可直接回退原始参数。
pub(crate) fn preview_has_content(preview: &AssistantToolPreviewDto) -> bool {
    !preview.action.trim().is_empty()
        || !preview.tasks.is_empty()
        || preview.more_tasks > 0
        || preview.delete_files
        || !preview.settings.is_empty()
        || !preview.urls.is_empty()
        || !preview.details.is_empty()
}

/// 预览正文。
pub(crate) fn preview_body(
    preview: &AssistantToolPreviewDto,
    t: Translate<'_>,
    cx: &App,
) -> AnyElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let ext = theme.extended();
    let gap = tokens.spacing.xs;
    let mono = tokens.typography.mono.clone();

    let title = match action_label_key(&preview.action) {
        Some(key) => t(key, &[]),
        None => preview.action.clone(),
    };
    let mut body = v_flex().min_w_0().gap(gap);
    if !title.trim().is_empty() {
        body = body.child(body_text(cx).font_weight(FontWeight::SEMIBOLD).child(title));
    }

    if !preview.tasks.is_empty() || preview.more_tasks > 0 {
        let mut list = v_flex().min_w_0().gap(tokens.spacing.xxs);
        list = list.child(label_text(cx).child(t("assistantPreviewTasks", &[])));
        for name in &preview.tasks {
            list = list.child(bullet(name.clone(), cx));
        }
        if preview.more_tasks > 0 {
            let n = preview.more_tasks.to_string();
            list = list.child(label_text(cx).child(t("assistantPreviewMoreTasks", &[("n", &n)])));
        }
        body = body.child(list);
    }

    if preview.delete_files {
        body = body.child(
            body_text(cx)
                .text_color(ext.colors.warning)
                .child(t("assistantPreviewDeleteFiles", &[])),
        );
    }

    if !preview.settings.is_empty() {
        let mut list = v_flex().min_w_0().gap(tokens.spacing.xxs);
        list = list.child(label_text(cx).child(t("assistantPreviewSettings", &[])));
        for change in &preview.settings {
            let from = change.from.as_deref().unwrap_or(NO_PREVIOUS_VALUE);
            list = list.child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(
                        body_text(cx)
                            .font_weight(FontWeight::MEDIUM)
                            .child(change.key.clone()),
                    )
                    .child(
                        body_text(cx)
                            .font_family(mono.clone())
                            .text_color(tokens.colors.muted_foreground)
                            .child(from.to_owned()),
                    )
                    .child(label_text(cx).child("→"))
                    .child(
                        body_text(cx)
                            .font_family(mono.clone())
                            .child(change.to.clone()),
                    ),
            );
        }
        body = body.child(list);
    }

    if !preview.urls.is_empty() {
        let mut list = v_flex().min_w_0().gap(tokens.spacing.xxs);
        list = list.child(label_text(cx).child(t("assistantPreviewUrls", &[])));
        for url in &preview.urls {
            list = list.child(
                body_text(cx)
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .child(url.clone()),
            );
        }
        body = body.child(list);
    }

    for detail in &preview.details {
        body = body.child(label_text(cx).child(detail.clone()));
    }

    body.into_any_element()
}

/// 项目符号行（任务名过长时省略）。
fn bullet(text: String, cx: &App) -> Div {
    let gap = active_theme(cx).tokens().spacing.xs;
    h_flex()
        .min_w_0()
        .gap(gap)
        .child(label_text(cx).child("•"))
        .child(body_text(cx).min_w_0().truncate().child(text))
}

/// 小节标题 / 补充说明：xs + 三级文字色。
fn label_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let xs = theme.tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(theme.extended().colors.text_tertiary)
}

/// 正文：sm + 前景色。
fn body_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let sm = tokens.typography.sm;
    div()
        .text_size(sm.size)
        .line_height(sm.line_height)
        .text_color(tokens.colors.foreground)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::AssistantSettingChangeDto;

    use super::*;

    const ACTIONS: [&str; 20] = [
        "create",
        "pause",
        "resume",
        "delete",
        "moveToQueue",
        "edit",
        "restart",
        "changeUrl",
        "recheck",
        "reannounce",
        "setSeedLimits",
        "setFilePriorities",
        "updateSettings",
        "queueStart",
        "queueStop",
        "queueReorder",
        "installComponent",
        "setPluginEnabled",
        "repair",
        "measureRoutes",
    ];

    #[test]
    fn every_protocol_action_maps_to_its_key() {
        for action in ACTIONS {
            let key = action_label_key(action).unwrap_or_default();
            let mut expected = String::from("assistantPreviewAction");
            let mut chars = action.chars();
            if let Some(first) = chars.next() {
                expected.extend(first.to_uppercase());
                expected.push_str(chars.as_str());
            }
            assert_eq!(key, expected, "{action}");
        }
    }

    #[test]
    fn unknown_or_miscased_action_has_no_key() {
        for action in ["", "Delete", "delete ", "format", "update_settings"] {
            assert_eq!(action_label_key(action), None, "{action:?}");
        }
    }

    #[test]
    fn content_detection_covers_every_field() {
        assert!(!preview_has_content(&AssistantToolPreviewDto::default()));
        assert!(!preview_has_content(&AssistantToolPreviewDto {
            action: "  ".to_owned(),
            ..Default::default()
        }));
        let with = |f: fn(&mut AssistantToolPreviewDto)| {
            let mut preview = AssistantToolPreviewDto::default();
            f(&mut preview);
            preview_has_content(&preview)
        };
        assert!(with(|p| p.action = "pause".to_owned()));
        assert!(with(|p| p.tasks.push("a.zip".to_owned())));
        assert!(with(|p| p.more_tasks = 3));
        assert!(with(|p| p.delete_files = true));
        assert!(with(|p| p.settings.push(AssistantSettingChangeDto {
            key: "k".to_owned(),
            from: None,
            to: "v".to_owned(),
        })));
        assert!(with(|p| p.urls.push("https://a.test".to_owned())));
        assert!(with(|p| p.details.push("queue".to_owned())));
    }
}
