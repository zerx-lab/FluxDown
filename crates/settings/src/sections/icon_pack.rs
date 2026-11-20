//! 外观 → 文件图标：图标包卡片（预览几种常见文件的图标）。只写偏好
//! `appearance.file_icon_pack`，生效由 app 的偏好投影（`apply_icon_pack_preference`）完成。

use fluxdown_protocol::FILE_ICON_PACK_KEY;
use fluxdown_ui_components::FluxIcon;
use fluxdown_ui_icon_pack::{
    BUILTIN_PACK_IDS, DEFAULT_DESKTOP_PACK, FileIconChoice, FileKind, IconPackState,
    SYSTEM_PACK_ID, active_icon_packs, pack_icon,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{Icon, h_flex, v_flex};

use super::SectionContext;
use crate::ui::{Control, SettingsRow};

const CARD_WIDTH: f32 = 120.;
const PREVIEW_ICON: f32 = 20.;
/// 预览用的样例文件：覆盖视频、压缩包、文档、程序四种最常见的下载。
const PREVIEW_FILES: [&str; 4] = ["video.mp4", "archive.zip", "report.pdf", "setup.exe"];

pub(crate) fn item(ctx: &SectionContext) -> SettingsRow {
    ctx.item("fileIconPack", Some("fileIconPackDesc"), field(ctx))
        .vertical()
        .keywords([ctx.t("searchKeywordsFileIconPack")])
}

fn label_key(id: &str) -> &'static str {
    match id {
        SYSTEM_PACK_ID => "fileIconPackSystem",
        "lucide" => "fileIconPackLucide",
        "material" => "fileIconPackMaterial",
        "thunder" => "fileIconPackThunder",
        _ => "fileIconPackCatppuccin",
    }
}

fn field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let cards: Vec<(SharedString, SharedString, IconPackState)> = BUILTIN_PACK_IDS
        .into_iter()
        .map(|id| {
            let selection = format!("builtin:{id}");
            let state = IconPackState::new(&selection);
            (SharedString::from(selection), ctx.t(label_key(id)), state)
        })
        .collect();
    Control::custom(
        move |disabled: bool, _key: &SharedString, window: &mut Window, cx: &mut App| {
            let current = active_icon_packs(cx).selection().to_owned();
            let current = if cards.iter().any(|(selection, _, _)| *selection == current) {
                current
            } else {
                DEFAULT_DESKTOP_PACK.to_owned()
            };
            let mut row = h_flex()
                .w_full()
                .flex_wrap()
                .gap(active_theme(cx).tokens().spacing.sm);
            for (selection, label, state) in &cards {
                let selected = *selection == current;
                let preview = preview_icons(selection, state, window, cx);
                let store = store.clone();
                let value = selection.clone();
                row = row.child(card(
                    selection,
                    label.clone(),
                    preview,
                    selected,
                    disabled,
                    move |cx| {
                        store.update(cx, |store, cx| {
                            store.set_pref_str(FILE_ICON_PACK_KEY, value.to_string(), cx);
                        });
                    },
                    cx,
                ));
            }
            row.into_any_element()
        },
    )
}

fn preview_icons(
    selection: &str,
    state: &IconPackState,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let theme = active_theme(cx);
    let dark = theme.mode().is_dark();
    let color = theme.tokens().colors.muted_foreground;
    let edge = theme.text_extent(PREVIEW_ICON);
    if selection == DEFAULT_DESKTOP_PACK {
        // 系统图标随文件管理器与平台变化，没有固定样例：用一个硬盘图标示意。
        return vec![
            Icon::new(FluxIcon::HardDrive)
                .size(edge)
                .text_color(color)
                .into_any_element(),
        ];
    }
    PREVIEW_FILES
        .iter()
        .filter_map(|name| match state.resolve(name, FileKind::of_name(name))? {
            FileIconChoice::Pack(icon) | FileIconChoice::System { fallback: icon } => {
                Some(pack_icon(&icon, dark, edge, color, window, cx))
            }
        })
        .collect()
}

fn card(
    selection: &SharedString,
    label: SharedString,
    preview: Vec<AnyElement>,
    selected: bool,
    disabled: bool,
    on_select: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let colors = tokens.colors;
    let row_hover = extended.colors.row_hover;
    v_flex()
        .id(SharedString::from(format!("icon-pack-card-{selection}")))
        .w(theme.text_extent(CARD_WIDTH))
        .p(tokens.spacing.sm)
        .gap(tokens.spacing.xs)
        .rounded(tokens.radius.lg)
        .border_1()
        .border_color(if selected {
            colors.primary
        } else {
            colors.border
        })
        .bg(colors.surface)
        .when(!disabled, |this| {
            this.cursor_pointer()
                .when(!selected, |this| {
                    this.hover(move |style| style.bg(row_hover))
                })
                .on_click(move |_, _, cx| on_select(cx))
        })
        .child(
            h_flex()
                .w_full()
                .h(theme.text_extent(PREVIEW_ICON * 2.))
                .justify_center()
                .items_center()
                .gap(tokens.spacing.xs)
                .rounded(tokens.radius.md)
                .bg(colors.background)
                .children(preview),
        )
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap(tokens.spacing.xs)
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(colors.foreground)
                .child(div().min_w_0().truncate().child(label))
                .when(selected, |this| {
                    this.child(
                        Icon::new(FluxIcon::Check)
                            .size(extended.icon.sm)
                            .text_color(colors.primary),
                    )
                }),
        )
        .into_any_element()
}
