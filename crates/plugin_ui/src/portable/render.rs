//! 视图节点 → GPUI 元素。节点表见 `native/protocol/src/plugin_view_schema.json`。
//!
//! 渲染层不信任树的内容：协议层已校验过，但这里对缺失 / 类型不符的 prop 一律退回默认值，
//! 不 panic、不丢弃整棵树。

use std::{collections::HashMap, ops::Range, rc::Rc, sync::Arc};

use fluxdown_protocol::{ViewEventDto, ViewNode};
use fluxdown_ui_components::{BusyExt as _, ControlExt as _};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, ElementId, Entity, Focusable as _,
    FontWeight, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window, div, img, prelude::FluentBuilder as _, px, uniform_list,
};
use gpui_component::{
    Disableable as _, Sizable as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    progress::Progress,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    spinner::Spinner,
    switch::Switch,
    tab::{Tab, TabBar},
    text::TextView,
};
use serde_json::Value;

use super::host::{PendingInput, PluginViewHost};

/// 色调 → registry token 路径；与 `native/protocol/src/plugin_view_tones.json` 一致（有测试守护）。
pub const TONES: [(&str, &str); 7] = [
    ("neutral", "colors.foreground"),
    ("accent", "colors.primary"),
    ("success", "colors.success"),
    ("warning", "colors.warning"),
    ("danger", "colors.destructive"),
    ("info", "colors.info"),
    ("muted", "colors.mutedForeground"),
];

/// 色调名对应的 token 路径。
pub fn tone_path(tone: &str) -> Option<&'static str> {
    TONES
        .iter()
        .find(|(name, _)| *name == tone)
        .map(|(_, path)| *path)
}

const TABLE_MAX_VISIBLE_ROWS: usize = 12;
const LIST_MAX_VISIBLE_ROWS: usize = 12;

/// 一次渲染的会话上下文。
pub(super) struct RenderEnv<'a> {
    pub host: &'a WeakEntity<PluginViewHost>,
    pub session: &'a str,
    pub revision: u64,
    pub pending: &'a HashMap<String, PendingInput>,
}

/// 当前主题里渲染要用的值（一次渲染取一次）。
#[derive(Clone)]
struct Look {
    fg: Hsla,
    muted_fg: Hsla,
    border: Hsla,
    surface: Hsla,
    muted: Hsla,
    primary: Hsla,
    destructive: Hsla,
    success: Hsla,
    warning: Hsla,
    info: Hsla,
    row_hover: Hsla,
    gap_xs: Pixels,
    gap_sm: Pixels,
    gap_md: Pixels,
    gap_lg: Pixels,
    gap_xl: Pixels,
    text_xs: Pixels,
    text_sm: Pixels,
    text_md: Pixels,
    text_lg: Pixels,
    text_xl: Pixels,
    mono: SharedString,
    radius: Pixels,
    row_height: Pixels,
}

impl Look {
    fn from_cx(cx: &App) -> Self {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let ext = &theme.extended().colors;
        Self {
            fg: tokens.colors.foreground,
            muted_fg: tokens.colors.muted_foreground,
            border: tokens.colors.border,
            surface: tokens.colors.surface,
            muted: tokens.colors.muted,
            primary: tokens.colors.primary,
            destructive: tokens.colors.destructive,
            success: ext.success,
            warning: ext.warning,
            info: ext.info,
            row_hover: ext.row_hover,
            gap_xs: tokens.spacing.xs,
            gap_sm: tokens.spacing.sm,
            gap_md: tokens.spacing.md,
            gap_lg: tokens.spacing.lg,
            gap_xl: tokens.spacing.xl,
            text_xs: tokens.typography.xs.size,
            text_sm: tokens.typography.sm.size,
            text_md: tokens.typography.md.size,
            text_lg: tokens.typography.lg.size,
            text_xl: tokens.typography.xl.size,
            mono: tokens.typography.mono.clone(),
            radius: tokens.radius.md,
            row_height: theme.text_extent(32.0),
        }
    }

    /// registry token 路径 → 颜色。
    fn resolve(&self, path: &str) -> Option<Hsla> {
        Some(match path {
            "colors.foreground" => self.fg,
            "colors.primary" => self.primary,
            "colors.success" => self.success,
            "colors.warning" => self.warning,
            "colors.destructive" => self.destructive,
            "colors.info" => self.info,
            "colors.mutedForeground" => self.muted_fg,
            _ => return None,
        })
    }

    fn tone(&self, tone: &str) -> Hsla {
        tone_path(tone)
            .and_then(|path| self.resolve(path))
            .unwrap_or(self.fg)
    }

    fn gap(&self, name: &str) -> Pixels {
        match name {
            "xs" => self.gap_xs,
            "sm" => self.gap_sm,
            "md" => self.gap_md,
            "lg" => self.gap_lg,
            "xl" => self.gap_xl,
            _ => px(0.0),
        }
    }

    fn text_size(&self, name: &str) -> Pixels {
        match name {
            "xs" => self.text_xs,
            "md" => self.text_md,
            "lg" => self.text_lg,
            "xl" => self.text_xl,
            _ => self.text_sm,
        }
    }
}

// ───────────────────────── prop 读取 ─────────────────────────

fn str_prop<'a>(node: &'a ViewNode, name: &str) -> &'a str {
    node.p.get(name).and_then(Value::as_str).unwrap_or("")
}

fn opt_str_prop<'a>(node: &'a ViewNode, name: &str) -> Option<&'a str> {
    node.p
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn bool_prop(node: &ViewNode, name: &str) -> bool {
    node.p.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn enum_prop<'a>(node: &'a ViewNode, name: &str, default: &'a str) -> &'a str {
    node.p.get(name).and_then(Value::as_str).unwrap_or(default)
}

fn num_prop(node: &ViewNode, name: &str) -> Option<f64> {
    node.p.get(name).and_then(Value::as_f64)
}

fn array_prop<'a>(node: &'a ViewNode, name: &str) -> &'a [Value] {
    node.p
        .get(name)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn field_str<'a>(item: &'a Value, name: &str) -> &'a str {
    item.get(name).and_then(Value::as_str).unwrap_or("")
}

fn element_id(env: &RenderEnv<'_>, node: &ViewNode) -> ElementId {
    ElementId::Name(SharedString::from(format!("{}/{}", env.session, node.id)))
}

// ───────────────────────── 事件 ─────────────────────────

fn event(kind: &str) -> ViewEventDto {
    ViewEventDto {
        kind: kind.to_owned(),
        value: None,
        key: None,
        action: None,
    }
}

fn value_event(kind: &str, value: Value) -> ViewEventDto {
    ViewEventDto {
        value: Some(value),
        ..event(kind)
    }
}

fn emit(host: &WeakEntity<PluginViewHost>, node: &str, event: ViewEventDto, cx: &mut App) {
    if host
        .update(cx, |host, cx| host.send_event(node, event, cx))
        .is_err()
    {
        log::debug!("plugin view host released, dropping `{}` event", node);
    }
}

// ───────────────────────── 入口 ─────────────────────────

/// 渲染整棵树。
pub(super) fn render_tree(
    root: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut Context<PluginViewHost>,
) -> AnyElement {
    let look = Look::from_cx(cx);
    render_node(root, env, &look, window, cx)
}

fn render_children(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    look: &Look,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut out = Vec::with_capacity(node.c.len());
    for child in &node.c {
        out.push(render_node(child, env, look, window, cx));
    }
    out
}

fn render_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    look: &Look,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match node.k.as_str() {
        "stack" => stack(node, env, look, window, cx),
        "scroll" => {
            let children = render_children(node, env, look, window, cx);
            div()
                .id(element_id(env, node))
                .w_full()
                .min_w_0()
                .overflow_y_scroll()
                .when_some(num_prop(node, "maxHeight"), |this, height| {
                    this.max_h(px(height as f32))
                })
                .child(div().flex().flex_col().gap(look.gap_sm).children(children))
                .into_any_element()
        }
        "card" => {
            let children = render_children(node, env, look, window, cx);
            div()
                .flex()
                .flex_col()
                .gap(look.gap_sm)
                .p(look.gap(enum_prop(node, "padding", "md")))
                .border_1()
                .border_color(look.border)
                .rounded(look.radius)
                .bg(look.surface)
                .when_some(opt_str_prop(node, "title"), |this, title| {
                    this.child(
                        div()
                            .text_size(look.text_md)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(look.fg)
                            .child(title.to_owned()),
                    )
                })
                .children(children)
                .into_any_element()
        }
        "section" => {
            let children = render_children(node, env, look, window, cx);
            div()
                .flex()
                .flex_col()
                .gap(look.gap_sm)
                .child(
                    div()
                        .text_size(look.text_lg)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(look.fg)
                        .child(str_prop(node, "title").to_owned()),
                )
                .when_some(opt_str_prop(node, "description"), |this, text| {
                    this.child(
                        div()
                            .text_size(look.text_sm)
                            .text_color(look.muted_fg)
                            .child(text.to_owned()),
                    )
                })
                .children(children)
                .into_any_element()
        }
        "divider" => div()
            .w_full()
            .h(px(1.0))
            .flex_none()
            .bg(look.border)
            .into_any_element(),
        "text" => text_node(node, look),
        "heading" => {
            let (size, weight) = match node.p.get("level").and_then(Value::as_i64).unwrap_or(2) {
                1 => (look.text_xl, FontWeight::BOLD),
                3 => (look.text_md, FontWeight::SEMIBOLD),
                _ => (look.text_lg, FontWeight::SEMIBOLD),
            };
            div()
                .text_size(size)
                .font_weight(weight)
                .text_color(look.fg)
                .child(str_prop(node, "text").to_owned())
                .into_any_element()
        }
        "markdown" => div()
            .w_full()
            .text_size(look.text_sm)
            .text_color(look.fg)
            .child(
                TextView::markdown(
                    element_id(env, node),
                    SharedString::from(str_prop(node, "text").to_owned()),
                )
                .selectable(true)
                .w_full(),
            )
            .into_any_element(),
        "code" => div()
            .w_full()
            .p(look.gap_sm)
            .rounded(look.radius)
            .bg(look.muted)
            .font_family(look.mono.clone())
            .text_size(look.text_sm)
            .text_color(look.fg)
            .child(str_prop(node, "text").to_owned())
            .into_any_element(),
        "link" => link_node(node, env, look),
        "image" => image_node(node, env, look, window, cx),
        "badge" => badge(
            str_prop(node, "text"),
            look.tone(enum_prop(node, "tone", "neutral")),
            look,
        )
        .into_any_element(),
        "progress" => progress_node(node, env, look),
        "spinner" => div()
            .flex()
            .items_center()
            .gap(look.gap_sm)
            .child(Spinner::new().small())
            .when_some(opt_str_prop(node, "label"), |this, label| {
                this.child(
                    div()
                        .text_size(look.text_sm)
                        .text_color(look.muted_fg)
                        .child(label.to_owned()),
                )
            })
            .into_any_element(),
        "kv" => kv_node(node, look),
        "callout" => callout_node(node, env),
        "empty" => div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(look.gap_xs)
            .p(look.gap_lg)
            .child(
                div()
                    .text_size(look.text_md)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(look.fg)
                    .child(str_prop(node, "title").to_owned()),
            )
            .when_some(opt_str_prop(node, "description"), |this, text| {
                this.child(
                    div()
                        .text_size(look.text_sm)
                        .text_color(look.muted_fg)
                        .child(text.to_owned()),
                )
            })
            .into_any_element(),
        "button" => button_node(node, env),
        "switch" => switch_node(node, env),
        "checkbox" => checkbox_node(node, env),
        "select" => select_node(node, env, window, cx),
        "input" => input_node(node, env, window, cx),
        "textarea" => textarea_node(node, env, window, cx),
        "tabs" => tabs_node(node, env),
        "table" => table_node(node, env, window, cx),
        "list" => list_node(node, env, window, cx),
        other => div()
            .text_size(look.text_xs)
            .text_color(look.muted_fg)
            .child(format!("[unsupported node: {other}]"))
            .into_any_element(),
    }
}

// ───────────────────────── 布局与文本 ─────────────────────────

fn stack(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    look: &Look,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let children = render_children(node, env, look, window, cx);
    let row = enum_prop(node, "direction", "column") == "row";
    let this: Div = div().flex().min_w_0();
    let this = if row {
        this.flex_row()
    } else {
        this.flex_col()
    };
    this.gap(look.gap(enum_prop(node, "gap", "sm")))
        .p(look.gap(enum_prop(node, "padding", "none")))
        .map(|this| match enum_prop(node, "align", "stretch") {
            "start" => this.items_start(),
            "center" => this.items_center(),
            "end" => this.items_end(),
            _ => this,
        })
        .map(|this| match enum_prop(node, "justify", "start") {
            "center" => this.justify_center(),
            "end" => this.justify_end(),
            "between" => this.justify_between(),
            _ => this,
        })
        .when(bool_prop(node, "wrap"), |this| this.flex_wrap())
        .when(bool_prop(node, "grow"), |this| {
            this.flex_grow(1.0).flex_basis(px(0.0))
        })
        .children(children)
        .into_any_element()
}

fn weight(name: &str) -> FontWeight {
    match name {
        "medium" => FontWeight::MEDIUM,
        "semibold" => FontWeight::SEMIBOLD,
        _ => FontWeight::NORMAL,
    }
}

fn text_node(node: &ViewNode, look: &Look) -> AnyElement {
    div()
        .text_size(look.text_size(enum_prop(node, "size", "sm")))
        .text_color(look.tone(enum_prop(node, "tone", "neutral")))
        .font_weight(weight(enum_prop(node, "weight", "normal")))
        .when(bool_prop(node, "mono"), |this| {
            this.font_family(look.mono.clone())
        })
        .when(bool_prop(node, "truncate"), |this| {
            this.min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
        })
        .child(str_prop(node, "text").to_owned())
        .into_any_element()
}

fn badge(text: &str, color: Hsla, look: &Look) -> Div {
    div()
        .px(look.gap_sm)
        .py(px(1.0))
        .rounded_full()
        .bg(color.opacity(0.15))
        .text_size(look.text_xs)
        .text_color(color)
        .child(text.to_owned())
}

fn link_node(node: &ViewNode, env: &RenderEnv<'_>, look: &Look) -> AnyElement {
    let href = str_prop(node, "href").to_owned();
    let safe = href.starts_with("https://") || href.starts_with("http://");
    div()
        .id(element_id(env, node))
        .text_size(look.text_sm)
        .text_color(look.primary)
        .underline()
        .when(safe, |this| {
            this.cursor_pointer()
                .on_click(move |_, _, cx| cx.open_url(&href))
        })
        .child(str_prop(node, "text").to_owned())
        .into_any_element()
}

fn progress_node(node: &ViewNode, env: &RenderEnv<'_>, look: &Look) -> AnyElement {
    let value = node.p.get("value").and_then(Value::as_f64);
    let bar = Progress::new(element_id(env, node)).w_full();
    let bar = match value {
        Some(value) => bar.value((value.clamp(0.0, 1.0) * 100.0) as f32),
        None => bar.loading(true),
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(look.gap_xs)
        .when_some(opt_str_prop(node, "label"), |this, label| {
            this.child(
                div()
                    .text_size(look.text_xs)
                    .text_color(look.muted_fg)
                    .child(label.to_owned()),
            )
        })
        .child(bar)
        .into_any_element()
}

fn kv_node(node: &ViewNode, look: &Look) -> AnyElement {
    let key_width = look.row_height * 4.0;
    let mut column = div().flex().flex_col().gap(look.gap_xs).min_w_0();
    for item in array_prop(node, "items") {
        column = column.child(
            div()
                .flex()
                .gap(look.gap_md)
                .text_size(look.text_sm)
                .child(
                    div()
                        .w(key_width)
                        .flex_none()
                        .text_color(look.muted_fg)
                        .child(field_str(item, "key").to_owned()),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_grow(1.0)
                        .text_color(look.fg)
                        .child(field_str(item, "value").to_owned()),
                ),
        );
    }
    column.into_any_element()
}

fn callout_node(node: &ViewNode, env: &RenderEnv<'_>) -> AnyElement {
    let id = element_id(env, node);
    let text = str_prop(node, "text").to_owned();
    let alert = match enum_prop(node, "tone", "info") {
        "success" => Alert::success(id, text),
        "warning" => Alert::warning(id, text),
        "danger" => Alert::error(id, text),
        _ => Alert::info(id, text),
    };
    alert
        .when_some(opt_str_prop(node, "title"), |this, title| {
            this.title(title.to_owned())
        })
        .w_full()
        .into_any_element()
}

// ───────────────────────── image ─────────────────────────

/// 缓存解码结果，`src` 不变时不重复解码。
struct ImageSlot {
    src: String,
    image: Option<Arc<Image>>,
}

fn image_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    look: &Look,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let src = str_prop(node, "src");
    let slot = window.use_keyed_state(element_id(env, node), cx, |_, _| ImageSlot {
        src: String::new(),
        image: None,
    });
    if slot.read(cx).src != src {
        let image = decode_image(src);
        slot.update(cx, |slot, _| {
            slot.src = src.to_owned();
            slot.image = image;
        });
    }
    let alt = str_prop(node, "alt").to_owned();
    match slot.read(cx).image.clone() {
        Some(image) => img(image)
            .when_some(num_prop(node, "width"), |this, w| this.w(px(w as f32)))
            .when_some(num_prop(node, "height"), |this, h| this.h(px(h as f32)))
            .into_any_element(),
        None => div()
            .text_size(look.text_xs)
            .text_color(look.muted_fg)
            .child(alt)
            .into_any_element(),
    }
}

fn decode_image(src: &str) -> Option<Arc<Image>> {
    let rest = src.strip_prefix("data:image/")?;
    let (kind, rest) = rest.split_once(";base64,")?;
    let format = match kind {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        _ => return None,
    };
    let bytes = decode_base64(rest)?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// 标准 base64（带或不带 `=` 填充）。
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ───────────────────────── 控件 ─────────────────────────

fn button_node(node: &ViewNode, env: &RenderEnv<'_>) -> AnyElement {
    let host = env.host.clone();
    let node_id = node.id.clone();
    let button = Button::new(element_id(env, node))
        .label(str_prop(node, "text").to_owned())
        .busy(bool_prop(node, "loading"))
        .disabled(bool_prop(node, "disabled"))
        .on_click(move |_: &ClickEvent, _, cx| emit(&host, &node_id, event("press"), cx));
    match enum_prop(node, "variant", "outline") {
        "primary" => button.primary(),
        "danger" => button.danger(),
        "ghost" => button.ghost(),
        _ => button.outline(),
    }
    .into_any_element()
}

fn switch_node(node: &ViewNode, env: &RenderEnv<'_>) -> AnyElement {
    let host = env.host.clone();
    let node_id = node.id.clone();
    Switch::new(element_id(env, node))
        .checked(bool_prop(node, "checked"))
        .disabled(bool_prop(node, "disabled"))
        .when_some(opt_str_prop(node, "label"), |this, label| {
            this.label(label.to_owned())
        })
        .on_click(move |checked: &bool, _, cx| {
            emit(
                &host,
                &node_id,
                value_event("change", Value::Bool(*checked)),
                cx,
            );
        })
        .into_any_element()
}

fn checkbox_node(node: &ViewNode, env: &RenderEnv<'_>) -> AnyElement {
    let host = env.host.clone();
    let node_id = node.id.clone();
    Checkbox::new(element_id(env, node))
        .checked(bool_prop(node, "checked"))
        .disabled(bool_prop(node, "disabled"))
        .when_some(opt_str_prop(node, "label"), |this, label| {
            this.label(label.to_owned())
        })
        .on_click(move |checked: &bool, _, cx| {
            emit(
                &host,
                &node_id,
                value_event("change", Value::Bool(*checked)),
                cx,
            );
        })
        .into_any_element()
}

fn tabs_node(node: &ViewNode, env: &RenderEnv<'_>) -> AnyElement {
    let items = array_prop(node, "items");
    let current = str_prop(node, "value");
    let selected = items
        .iter()
        .position(|item| field_str(item, "value") == current)
        .unwrap_or(0);
    let values: Vec<String> = items
        .iter()
        .map(|item| field_str(item, "value").to_owned())
        .collect();
    let host = env.host.clone();
    let node_id = node.id.clone();
    TabBar::new(element_id(env, node))
        .segmented()
        .selected_index(selected)
        .children(
            items
                .iter()
                .map(|item| Tab::new().label(field_str(item, "label").to_owned())),
        )
        .on_click(move |index: &usize, _, cx| {
            if let Some(value) = values.get(*index) {
                emit(
                    &host,
                    &node_id,
                    value_event("change", Value::String(value.clone())),
                    cx,
                );
            }
        })
        .into_any_element()
}

// ───────────── select / input / textarea（带本地状态） ─────────────

#[derive(Clone)]
struct Opt {
    value: SharedString,
    label: SharedString,
}

impl SelectItem for Opt {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

type OptSelect = SelectState<SearchableVec<Opt>>;

struct SelectSlot {
    select: Entity<OptSelect>,
    options: Vec<(String, String)>,
    synced: Option<String>,
    _subscription: Subscription,
}

fn opts(options: &[(String, String)]) -> SearchableVec<Opt> {
    SearchableVec::new(
        options
            .iter()
            .map(|(value, label)| Opt {
                value: value.clone().into(),
                label: label.clone().into(),
            })
            .collect::<Vec<_>>(),
    )
}

fn select_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let options: Vec<(String, String)> = array_prop(node, "options")
        .iter()
        .map(|item| {
            (
                field_str(item, "value").to_owned(),
                field_str(item, "label").to_owned(),
            )
        })
        .collect();
    let value = opt_str_prop(node, "value").map(str::to_owned);
    let slot = window.use_keyed_state(element_id(env, node), cx, {
        let host = env.host.clone();
        let node_id = node.id.clone();
        let options = options.clone();
        move |window, cx| {
            let select = cx.new(|cx| OptSelect::new(opts(&options), None, window, cx));
            let subscription = cx.subscribe(
                &select,
                move |_: &mut SelectSlot, _, event: &SelectEvent<SearchableVec<Opt>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        emit(
                            &host,
                            &node_id,
                            value_event("change", Value::String(value.to_string())),
                            cx,
                        );
                    }
                },
            );
            SelectSlot {
                select,
                options: options.clone(),
                synced: None,
                _subscription: subscription,
            }
        }
    });
    slot.update(cx, |slot, cx| {
        let mut force = false;
        if slot.options != options {
            slot.options = options.clone();
            force = true;
            slot.select.update(cx, |select, cx| {
                select.set_items(opts(&options), window, cx)
            });
        }
        if force || slot.synced != value {
            slot.synced = value.clone();
            if let Some(value) = &value {
                let value = SharedString::from(value.clone());
                slot.select.update(cx, |select, cx| {
                    select.set_selected_value(&value, window, cx)
                });
            }
        }
    });
    let select = slot.read(cx).select.clone();
    Select::new(&select)
        .when_some(opt_str_prop(node, "placeholder"), |this, text| {
            this.placeholder(text.to_owned())
        })
        .disabled(bool_prop(node, "disabled"))
        .w_full()
        .into_any_element()
}

/// 输入框的本地状态：`last_synced` 是编辑器当前内容在宿主眼中的版本。
struct TextSlot<S> {
    state: Entity<S>,
    last_synced: SharedString,
    _subscription: Subscription,
}

type InputSlot = TextSlot<InputState>;
type TextareaSlot = TextSlot<TextareaState>;

/// 编辑器事件 → 宿主：内容变化去抖、失焦 / 回车立即发送。
fn text_event(
    host: &WeakEntity<PluginViewHost>,
    node: &str,
    event_kind: &InputEvent,
    value: SharedString,
    last_synced: &mut SharedString,
    submit: bool,
    cx: &mut App,
) {
    match event_kind {
        InputEvent::Change => {
            if *last_synced == value {
                return;
            }
            *last_synced = value.clone();
            update_host(host, cx, |host, cx| {
                host.input_changed(node, value.to_string(), cx)
            });
        }
        InputEvent::Blur => update_host(host, cx, |host, cx| {
            host.flush_input(node, None, cx);
            cx.notify();
        }),
        InputEvent::PressEnter { .. } if submit => {
            update_host(host, cx, |host, cx| {
                host.flush_input(node, None, cx);
                host.send_event(
                    node,
                    value_event("submit", Value::String(value.to_string())),
                    cx,
                );
            });
        }
        _ => {}
    }
}

fn update_host(
    host: &WeakEntity<PluginViewHost>,
    cx: &mut App,
    f: impl FnOnce(&mut PluginViewHost, &mut Context<PluginViewHost>),
) {
    if host.update(cx, f).is_err() {
        log::debug!("plugin view host released, dropping input event");
    }
}

fn input_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let value = SharedString::from(str_prop(node, "value").to_owned());
    let pending = env.pending.contains_key(&node.id);
    let slot = window.use_keyed_state(element_id(env, node), cx, {
        let host = env.host.clone();
        let node_id = node.id.clone();
        let value = value.clone();
        let placeholder = opt_str_prop(node, "placeholder").map(str::to_owned);
        let masked = enum_prop(node, "kind", "text") == "password";
        move |window, cx| {
            let state = cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .default_value(value.clone())
                    .masked(masked);
                if let Some(placeholder) = &placeholder {
                    state = state.placeholder(placeholder.clone());
                }
                state
            });
            let subscription = cx.subscribe(
                &state,
                move |slot: &mut InputSlot, state, event: &InputEvent, cx| {
                    let current = state.read(cx).value();
                    text_event(
                        &host,
                        &node_id,
                        event,
                        current,
                        &mut slot.last_synced,
                        true,
                        cx,
                    );
                },
            );
            InputSlot {
                state,
                last_synced: value,
                _subscription: subscription,
            }
        }
    });
    slot.update(cx, |slot, cx| {
        if slot.last_synced != value && !pending {
            let focused = slot.state.focus_handle(cx).is_focused(window);
            if !focused {
                slot.last_synced = value.clone();
                slot.state
                    .update(cx, |state, cx| state.set_value(value.clone(), window, cx));
            }
        }
    });
    let state = slot.read(cx).state.clone();
    Input::new(&state)
        .control(cx)
        .w_full()
        .disabled(bool_prop(node, "disabled"))
        .into_any_element()
}

fn textarea_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let value = SharedString::from(str_prop(node, "value").to_owned());
    let pending = env.pending.contains_key(&node.id);
    let rows = node.p.get("rows").and_then(Value::as_u64).unwrap_or(4) as usize;
    let slot = window.use_keyed_state(element_id(env, node), cx, {
        let host = env.host.clone();
        let node_id = node.id.clone();
        let value = value.clone();
        let placeholder = opt_str_prop(node, "placeholder").map(str::to_owned);
        move |window, cx| {
            let state = cx.new(|cx| {
                let mut state = TextareaState::new(window, cx)
                    .default_value(value.clone())
                    .rows(rows);
                if let Some(placeholder) = &placeholder {
                    state = state.placeholder(placeholder.clone());
                }
                state
            });
            let subscription = cx.subscribe(
                &state,
                move |slot: &mut TextareaSlot, state, event: &InputEvent, cx| {
                    let current = state.read(cx).value();
                    text_event(
                        &host,
                        &node_id,
                        event,
                        current,
                        &mut slot.last_synced,
                        false,
                        cx,
                    );
                },
            );
            TextareaSlot {
                state,
                last_synced: value,
                _subscription: subscription,
            }
        }
    });
    slot.update(cx, |slot, cx| {
        if slot.last_synced != value && !pending {
            let focused = slot.state.focus_handle(cx).is_focused(window);
            if !focused {
                slot.last_synced = value.clone();
                slot.state
                    .update(cx, |state, cx| state.set_value(value.clone(), window, cx));
            }
        }
    });
    let state = slot.read(cx).state.clone();
    Textarea::new(&state)
        .w_full()
        .disabled(bool_prop(node, "disabled"))
        .into_any_element()
}

// ───────────────────────── table / list ─────────────────────────

struct Column {
    key: String,
    title: String,
    kind: String,
    width: Option<f32>,
}

struct RowAction {
    id: String,
    label: String,
    danger: bool,
}

/// 表格 / 列表数据：随树版本缓存，主题变化等不改树的重绘不重新克隆。
struct TableData {
    node: String,
    columns: Vec<Column>,
    rows: Vec<Value>,
    actions: Vec<RowAction>,
    empty_text: Option<String>,
}

struct TableSlot {
    revision: u64,
    data: Rc<TableData>,
}

fn table_data(node: &ViewNode, items_prop: &str) -> TableData {
    TableData {
        node: node.id.clone(),
        columns: array_prop(node, "columns")
            .iter()
            .map(|column| Column {
                key: field_str(column, "key").to_owned(),
                title: field_str(column, "title").to_owned(),
                kind: column
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("text")
                    .to_owned(),
                width: column
                    .get("width")
                    .and_then(Value::as_f64)
                    .map(|w| w as f32),
            })
            .collect(),
        rows: array_prop(node, items_prop).to_vec(),
        actions: array_prop(node, "rowActions")
            .iter()
            .map(|action| RowAction {
                id: field_str(action, "id").to_owned(),
                label: field_str(action, "label").to_owned(),
                danger: field_str(action, "tone") == "danger",
            })
            .collect(),
        empty_text: opt_str_prop(node, "emptyText").map(str::to_owned),
    }
}

fn cached_table(
    node: &ViewNode,
    items_prop: &str,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> Rc<TableData> {
    let slot = window.use_keyed_state(element_id(env, node), cx, |_, _| TableSlot {
        revision: u64::MAX,
        data: Rc::new(TableData {
            node: String::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            actions: Vec::new(),
            empty_text: None,
        }),
    });
    if slot.read(cx).revision != env.revision {
        let data = Rc::new(table_data(node, items_prop));
        let revision = env.revision;
        slot.update(cx, |slot, _| {
            slot.revision = revision;
            slot.data = data;
        });
    }
    slot.read(cx).data.clone()
}

fn row_key(row: &Value) -> &str {
    row.get("key").and_then(Value::as_str).unwrap_or("")
}

fn table_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let look = Look::from_cx(cx);
    let data = cached_table(node, "rows", env, window, cx);
    let host = env.host.clone();
    let count = data.rows.len();
    let visible = count.min(TABLE_MAX_VISIBLE_ROWS);
    let header = table_header(&data, &look);
    let mut table = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .border_1()
        .border_color(look.border)
        .rounded(look.radius)
        .overflow_hidden()
        .child(header);
    if count == 0 {
        if let Some(text) = &data.empty_text {
            table = table.child(
                div()
                    .p(look.gap_md)
                    .text_size(look.text_sm)
                    .text_color(look.muted_fg)
                    .child(text.clone()),
            );
        }
        return table.into_any_element();
    }
    let list_data = data.clone();
    table
        .child(
            uniform_list(
                element_id(env, node),
                count,
                move |range: Range<usize>, _window: &mut Window, cx: &mut App| {
                    let look = Look::from_cx(cx);
                    range
                        .filter_map(|ix| {
                            let row = list_data.rows.get(ix)?;
                            Some(table_row(&list_data, ix, row, &look, &host))
                        })
                        .collect::<Vec<_>>()
                },
            )
            .w_full()
            .h(look.row_height * visible as f32),
        )
        .into_any_element()
}

fn column_cell(look: &Look, width: Option<f32>) -> Div {
    let cell = div().px(look.gap_sm).min_w_0().overflow_hidden();
    match width {
        Some(width) => cell.w(px(width)).flex_none(),
        None => cell.flex_1(),
    }
}

fn table_header(data: &TableData, look: &Look) -> Div {
    let mut header = div()
        .flex()
        .items_center()
        .h(look.row_height)
        .border_b_1()
        .border_color(look.border)
        .bg(look.muted)
        .text_size(look.text_xs)
        .font_weight(FontWeight::MEDIUM)
        .text_color(look.muted_fg);
    for column in &data.columns {
        header = header.child(column_cell(look, column.width).child(column.title.clone()));
    }
    if !data.actions.is_empty() {
        header = header.child(div().flex_none().w(look.row_height * 2.0));
    }
    header
}

fn table_row(
    data: &TableData,
    ix: usize,
    row: &Value,
    look: &Look,
    host: &WeakEntity<PluginViewHost>,
) -> AnyElement {
    let key = row_key(row).to_owned();
    let mut line = div()
        .id(ElementId::Name(SharedString::from(format!(
            "{}/row/{ix}",
            data.node
        ))))
        .flex()
        .items_center()
        .w_full()
        .h(look.row_height)
        .text_size(look.text_sm)
        .text_color(look.fg)
        .hover(|style| style.bg(look.row_hover))
        .on_click({
            let host = host.clone();
            let node = data.node.clone();
            let key = key.clone();
            move |_, _, cx| {
                emit(
                    &host,
                    &node,
                    ViewEventDto {
                        key: Some(key.clone()),
                        ..event("select")
                    },
                    cx,
                )
            }
        });
    for column in &data.columns {
        let cell = column_cell(look, column.width);
        line = line.child(cell.child(cell_element(
            &column.kind,
            row.get(&column.key).unwrap_or(&Value::Null),
            look,
        )));
    }
    if !data.actions.is_empty() {
        let mut actions = div().flex().flex_none().items_center().gap(look.gap_xs);
        for action in &data.actions {
            let host = host.clone();
            let node = data.node.clone();
            let key = key.clone();
            let action_id = action.id.clone();
            let button = Button::new(ElementId::Name(SharedString::from(format!(
                "{}/row/{ix}/{}",
                data.node, action.id
            ))))
            .label(action.label.clone())
            .ghost()
            .small()
            .on_click(move |_: &ClickEvent, _, cx| {
                emit(
                    &host,
                    &node,
                    ViewEventDto {
                        key: Some(key.clone()),
                        action: Some(action_id.clone()),
                        ..event("rowAction")
                    },
                    cx,
                )
            });
            actions = actions.child(if action.danger {
                button.danger()
            } else {
                button
            });
        }
        line = line.child(actions);
    }
    line.into_any_element()
}

fn cell_element(kind: &str, value: &Value, look: &Look) -> AnyElement {
    match kind {
        "progress" => {
            let fraction = value.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
            Progress::new("cell-progress")
                .value((fraction * 100.0) as f32)
                .w_full()
                .into_any_element()
        }
        "badge" => {
            let (text, tone) = match value {
                Value::Object(map) => (
                    map.get("text").and_then(Value::as_str).unwrap_or(""),
                    map.get("tone").and_then(Value::as_str).unwrap_or("neutral"),
                ),
                Value::String(text) => (text.as_str(), "neutral"),
                _ => ("", "neutral"),
            };
            badge(text, look.tone(tone), look).into_any_element()
        }
        _ => div()
            .truncate()
            .child(cell_text(kind, value))
            .into_any_element(),
    }
}

fn list_node(
    node: &ViewNode,
    env: &RenderEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let look = Look::from_cx(cx);
    let data = cached_table(node, "items", env, window, cx);
    let host = env.host.clone();
    let count = data.rows.len();
    if count == 0 {
        return div().into_any_element();
    }
    let visible = count.min(LIST_MAX_VISIBLE_ROWS);
    let row_height = look.row_height * 1.5;
    uniform_list(
        element_id(env, node),
        count,
        move |range: Range<usize>, _window: &mut Window, cx: &mut App| {
            let look = Look::from_cx(cx);
            range
                .filter_map(|ix| {
                    let item = data.rows.get(ix)?;
                    Some(list_row(&data.node, ix, item, &look, row_height, &host))
                })
                .collect::<Vec<_>>()
        },
    )
    .w_full()
    .h(row_height * visible as f32)
    .into_any_element()
}

fn list_row(
    node: &str,
    ix: usize,
    item: &Value,
    look: &Look,
    height: Pixels,
    host: &WeakEntity<PluginViewHost>,
) -> AnyElement {
    let key = row_key(item).to_owned();
    let tone = item
        .get("tone")
        .and_then(Value::as_str)
        .unwrap_or("neutral");
    div()
        .id(ElementId::Name(SharedString::from(format!(
            "{node}/item/{ix}"
        ))))
        .flex()
        .items_center()
        .justify_between()
        .gap(look.gap_md)
        .w_full()
        .h(height)
        .px(look.gap_sm)
        .border_b_1()
        .border_color(look.border)
        .hover(|style| style.bg(look.row_hover))
        .on_click({
            let host = host.clone();
            let node = node.to_owned();
            move |_, _, cx| {
                emit(
                    &host,
                    &node,
                    ViewEventDto {
                        key: Some(key.clone()),
                        ..event("select")
                    },
                    cx,
                )
            }
        })
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(look.text_sm)
                        .text_color(look.fg)
                        .child(field_str(item, "title").to_owned()),
                )
                .when(!field_str(item, "description").is_empty(), |this| {
                    this.child(
                        div()
                            .truncate()
                            .text_size(look.text_xs)
                            .text_color(look.muted_fg)
                            .child(field_str(item, "description").to_owned()),
                    )
                }),
        )
        .when(!field_str(item, "badge").is_empty(), |this| {
            this.child(badge(field_str(item, "badge"), look.tone(tone), look))
        })
        .into_any_element()
}

// ───────────────────────── 单元格格式化 ─────────────────────────

/// 文本类单元格的显示文本（`progress` / `badge` 另有专门元素）。
pub(super) fn cell_text(kind: &str, value: &Value) -> String {
    match (kind, value) {
        (_, Value::Null) => String::new(),
        ("bytes", value) => value.as_f64().map_or_else(String::new, format_bytes),
        ("speed", value) => value.as_f64().map_or_else(String::new, format_speed),
        ("percent", value) => value.as_f64().map_or_else(String::new, format_percent),
        ("time", value) => value.as_i64().map_or_else(String::new, format_time_local),
        (_, Value::String(text)) => text.clone(),
        (_, Value::Bool(flag)) => flag.to_string(),
        (_, Value::Number(number)) => number.to_string(),
        (_, other) => other.to_string(),
    }
}

fn trim_decimal(value: f64, digits: usize) -> String {
    let text = format!("{value:.digits$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// 1024 进制的字节数（`0 B`、`1.5 KB`、`2 GB`）。
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if !bytes.is_finite() || bytes < 0.0 {
        return String::new();
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if unit == 0 { 0 } else { 1 };
    format!("{} {}", trim_decimal(value, digits), UNITS[unit])
}

pub fn format_speed(bytes_per_second: f64) -> String {
    let text = format_bytes(bytes_per_second);
    if text.is_empty() {
        text
    } else {
        format!("{text}/s")
    }
}

/// 输入为 0..1 的比例。
pub fn format_percent(fraction: f64) -> String {
    if !fraction.is_finite() {
        return String::new();
    }
    let percent = fraction.clamp(0.0, 1.0) * 100.0;
    format!(
        "{}%",
        trim_decimal(percent, if percent < 10.0 { 1 } else { 0 })
    )
}

/// unix 毫秒 → 本地时间 `YYYY-MM-DD HH:MM:SS`；超出可表示范围时返回空串。
pub fn format_time_local(unix_ms: i64) -> String {
    use chrono::{Local, TimeZone as _};
    Local
        .timestamp_millis_opt(unix_ms)
        .single()
        .map(|at| at.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod format_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bytes_and_speed() {
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(1023.0), "1023 B");
        assert_eq!(format_bytes(1536.0), "1.5 KB");
        assert_eq!(format_bytes(1024.0 * 1024.0 * 2.0), "2 MB");
        assert_eq!(format_speed(2048.0), "2 KB/s");
        assert_eq!(format_bytes(-1.0), "");
    }

    #[test]
    fn percent_clamps_and_trims() {
        assert_eq!(format_percent(0.5), "50%");
        assert_eq!(format_percent(0.123), "12%");
        assert_eq!(format_percent(0.055), "5.5%");
        assert_eq!(format_percent(2.0), "100%");
    }

    #[test]
    fn time_is_formatted_in_local_time() {
        use chrono::{Local, TimeZone as _};
        let at = Local
            .with_ymd_and_hms(2000, 2, 29, 8, 5, 9)
            .single()
            .expect("valid local time");
        assert_eq!(
            format_time_local(at.timestamp_millis()),
            "2000-02-29 08:05:09"
        );
        assert_eq!(format_time_local(i64::MAX), "");
    }

    #[test]
    fn cell_text_follows_column_kind() {
        assert_eq!(cell_text("text", &json!("a")), "a");
        assert_eq!(cell_text("text", &json!(3)), "3");
        assert_eq!(cell_text("bytes", &json!(2048)), "2 KB");
        assert_eq!(cell_text("percent", &json!(0.25)), "25%");
        assert_eq!(cell_text("time", &json!(0)), format_time_local(0));
        assert_eq!(cell_text("text", &Value::Null), "");
    }

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode_base64("aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode_base64("a$b"), None);
    }

    #[test]
    fn image_data_urls_are_restricted() {
        assert!(decode_image("data:image/png;base64,aGk=").is_some());
        assert!(decode_image("data:image/svg+xml;base64,aGk=").is_none());
        assert!(decode_image("https://x/y.png").is_none());
    }
}
