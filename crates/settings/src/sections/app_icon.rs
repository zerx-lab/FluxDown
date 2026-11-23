//! 外观 → 应用图标：默认 / 闪电 / 自定义图片三张卡片 + 「选择图片…」。
//!
//! 控件**不写偏好**：选择经 `agent.platform.setAppIcon` 交给 agent（导入图片、落盘、再写
//! `desktop.app_icon` 并应用到托盘 / 窗口 / 快捷方式），生效由 app 的偏好投影完成。卡片的选中态
//! 读偏好视图；自定义图片的预览经 `agent.platform.appIconImage` 取回，按修订号缓存。

use std::path::Path;
use std::sync::{Arc, LazyLock};

use base64::Engine as _;
use fluxdown_protocol::app_icon::{
    APP_ICON_PREF_KEY, AppIconChoice, AppIconKind, PlatformAppIconImageDto,
    PlatformSetAppIconParams,
};
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, method};
use fluxdown_ui_components::toast::{self, ToastTone};
use fluxdown_ui_components::{ButtonVariant, FluxIcon};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, Image, ImageFormat, InteractiveElement as _,
    IntoElement as _, ParentElement as _, PathPromptOptions, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, img, prelude::FluentBuilder as _,
    px,
};
use gpui_component::{Icon, WindowExt as _, h_flex, v_flex};
use serde_json::{Value, json};

use super::SectionContext;
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{Control, SettingsRow, row_button};

const DEFAULT_PNG: &[u8] = include_bytes!("../../../../assets/logo/fluxdown_logo.png");
const BOLT_PNG: &[u8] = include_bytes!("../../../../assets/logo/fluxdown_bolt.png");

static DEFAULT_IMAGE: LazyLock<Arc<Image>> =
    LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Png, DEFAULT_PNG.to_vec())));
static BOLT_IMAGE: LazyLock<Arc<Image>> =
    LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Png, BOLT_PNG.to_vec())));

type SelectHandler = Box<dyn Fn(&mut Window, &mut App)>;

const CARD_WIDTH: f32 = 96.;
const PREVIEW_SIZE: f32 = 40.;
/// 自定义预览向 agent 要的边长：40px 卡片在 2x 屏幕下的像素数，再留余量。
const CUSTOM_PREVIEW_PIXELS: u32 = 96;

pub(crate) fn item(ctx: &SectionContext) -> SettingsRow {
    ctx.item("appIcon", Some("appIconDesc"), field(ctx))
        .vertical()
        .keywords([ctx.t("searchKeywordsAppIcon")])
}

/// 自定义预览：已导入图片的修订号与 96px PNG。
struct CustomPreview {
    image: Arc<Image>,
}

/// 设置行的跨渲染状态（按行键保存在窗口里）。
#[derive(Default)]
struct PreviewSlot {
    custom: Option<CustomPreview>,
    /// 最近一次发起取图时对应的偏好修订号（`Some(None)` = 偏好不是自定义）；相同则不重复请求。
    requested_for: Option<Option<String>>,
    /// 请求序号：只采纳最后一次请求的回包，避免乱序回包用旧图覆盖新图。
    sequence: u64,
}

impl PreviewSlot {
    /// 偏好指向的自定义修订号变化（含首次渲染）时重新取图。
    fn refresh(
        &mut self,
        wanted: Option<String>,
        store: &Entity<SettingsStore>,
        cx: &mut Context<Self>,
    ) {
        if self.requested_for.as_ref() == Some(&wanted) {
            return;
        }
        self.requested_for = Some(wanted);
        self.sequence += 1;
        let sequence = self.sequence;
        let call = store.read(cx).raw_call(
            method::AGENT_PLATFORM_APP_ICON_IMAGE,
            json!({ "size": CUSTOM_PREVIEW_PIXELS }),
        );
        cx.spawn(async move |this, cx| {
            let outcome = call.await.and_then(parse_preview);
            let Ok(()) = this.update(cx, move |slot, cx| {
                if slot.sequence != sequence {
                    return;
                }
                match outcome {
                    Ok(preview) => slot.custom = Some(preview),
                    // 没有导入过图片：自定义卡片回到「选择图片」入口。
                    Err(error) if error.code == ApplicationErrorCode::NotFound => {
                        slot.custom = None;
                    }
                    // 其他错误保留已有预览，并清掉请求记录：下次渲染重新取图（不主动重绘，避免失败时空转）。
                    Err(_) => {
                        slot.requested_for = None;
                        return;
                    }
                }
                cx.notify();
            }) else {
                // 设置视图已释放，不再有预览要更新。
                return;
            };
        })
        .detach();
    }
}

fn parse_preview(value: Value) -> Result<CustomPreview, RpcErrorData> {
    let internal = || RpcErrorData::new(ApplicationErrorCode::Internal, false);
    let dto: PlatformAppIconImageDto = serde_json::from_value(value).map_err(|_| internal())?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(dto.png)
        .map_err(|_| internal())?;
    Ok(CustomPreview {
        image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
    })
}

fn field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let default_label = ctx.t("appIconDefault");
    let bolt_label = ctx.t("appIconBolt");
    let custom_label = ctx.t("appIconCustom");
    let choose_label = ctx.t("appIconChooseImage");
    Control::custom(
        move |disabled: bool, key: &SharedString, window: &mut Window, cx: &mut App| {
            let slot = window.use_keyed_state(
                SharedString::from(format!("{key}-app-icon")),
                cx,
                |_, _| PreviewSlot::default(),
            );
            let current = AppIconChoice::from_preference(store.read(cx).pref(APP_ICON_PREF_KEY));
            if !disabled {
                let wanted = match &current {
                    AppIconChoice::Custom { revision } => Some(revision.clone()),
                    _ => None,
                };
                slot.update(cx, |slot, cx| slot.refresh(wanted, &store, cx));
            }
            let custom_image = slot
                .read(cx)
                .custom
                .as_ref()
                .map(|preview| Arc::clone(&preview.image));
            let has_custom = custom_image.is_some();

            let state = active_theme(cx);
            let tokens = state.tokens().clone();
            let extended = state.extended().clone();
            let card_width = state.text_extent(CARD_WIDTH);
            let make_card = |id: &'static str,
                             label: &SharedString,
                             image: Option<Arc<Image>>,
                             selected: bool,
                             on_select: SelectHandler| {
                card(
                    CardView {
                        id,
                        label: label.clone(),
                        image,
                        selected,
                    },
                    disabled,
                    &tokens,
                    &extended,
                    card_width,
                    on_select,
                )
            };

            let select = |kind: AppIconKind| -> SelectHandler {
                let store = store.clone();
                let translator = translator.clone();
                Box::new(move |window, cx| {
                    set_icon(&store, &translator, kind, None, window.window_handle(), cx)
                })
            };
            let pick = {
                let store = store.clone();
                let translator = translator.clone();
                move |window: &mut Window, cx: &mut App| {
                    choose_image(&store, &translator, window.window_handle(), cx)
                }
            };
            // 已导入过图片 → 直接切回；否则等同「选择图片…」。
            let select_custom: SelectHandler = if has_custom {
                select(AppIconKind::Custom)
            } else {
                Box::new(pick.clone())
            };

            v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .flex_wrap()
                        .child(make_card(
                            "app-icon-default",
                            &default_label,
                            Some(Arc::clone(&DEFAULT_IMAGE)),
                            current == AppIconChoice::Default,
                            select(AppIconKind::Default),
                        ))
                        .child(make_card(
                            "app-icon-bolt",
                            &bolt_label,
                            Some(Arc::clone(&BOLT_IMAGE)),
                            current == AppIconChoice::Bolt,
                            select(AppIconKind::Bolt),
                        ))
                        .child(make_card(
                            "app-icon-custom",
                            &custom_label,
                            custom_image,
                            current.kind() == AppIconKind::Custom,
                            select_custom,
                        )),
                )
                .child(
                    h_flex().child(
                        row_button(
                            "app-icon-choose",
                            choose_label.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(disabled)
                        .on_click(move |_, window, cx| pick(window, cx)),
                    ),
                )
                .into_any_element()
        },
    )
}

struct CardView {
    id: &'static str,
    label: SharedString,
    /// `None` = 尚无图片，显示「+」占位。
    image: Option<Arc<Image>>,
    selected: bool,
}

fn card(
    view: CardView,
    disabled: bool,
    tokens: &fluxdown_ui_theme::SemanticThemeTokens,
    extended: &fluxdown_ui_theme::ExtendedTokens,
    width: gpui::Pixels,
    on_select: SelectHandler,
) -> AnyElement {
    let CardView {
        id,
        label,
        image,
        selected,
    } = view;
    let colors = tokens.colors;
    let row_hover = extended.colors.row_hover;
    let check_size = extended.icon.sm;
    let preview = match image {
        Some(image) => img(image)
            .size(px(PREVIEW_SIZE))
            .rounded(tokens.radius.md)
            .into_any_element(),
        None => div()
            .size(px(PREVIEW_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded(tokens.radius.md)
            .border_1()
            .border_color(colors.border)
            .child(
                Icon::new(FluxIcon::Plus)
                    .size(extended.icon.md)
                    .text_color(colors.muted_foreground),
            )
            .into_any_element(),
    };
    div()
        .id(id)
        .w(width)
        .p(tokens.spacing.sm)
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
                .on_click(move |_, window, cx| {
                    if !selected {
                        on_select(window, cx);
                    }
                })
        })
        .child(
            v_flex()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(preview)
                .child(
                    h_flex()
                        .max_w_full()
                        .gap(tokens.spacing.xxs)
                        .items_center()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(colors.foreground)
                        .child(div().min_w_0().truncate().child(label))
                        .when(selected, |this| {
                            this.child(
                                Icon::new(FluxIcon::Check)
                                    .size(check_size)
                                    .text_color(colors.primary),
                            )
                        }),
                ),
        )
        .into_any_element()
}

/// 弹出系统文件选择框，选中后导入并切到自定义图标。
fn choose_image(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    window_handle: AnyWindowHandle,
    cx: &mut App,
) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(SharedString::from(
            translator.text("appIconChooseImage").to_owned(),
        )),
    });
    let store = store.clone();
    let translator = translator.clone();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };
        cx.update(|cx| {
            set_icon(
                &store,
                &translator,
                AppIconKind::Custom,
                Some(path.as_path()),
                window_handle,
                cx,
            )
        });
    })
    .detach();
}

/// 请 agent 切换应用图标；带 `source_path` = 导入该图片并切换。失败以设置通知展示。
fn set_icon(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    kind: AppIconKind,
    source_path: Option<&Path>,
    window_handle: AnyWindowHandle,
    cx: &mut App,
) {
    let translator = translator.clone();
    let call = serde_json::to_value(set_icon_params(kind, source_path)).map(|params| {
        store
            .read(cx)
            .raw_call(method::AGENT_PLATFORM_SET_APP_ICON, params)
    });
    cx.spawn(async move |cx| {
        let message = match call {
            Ok(call) => match call.await {
                Ok(_) => return,
                Err(error) => rpc_error_text(&translator, &error),
            },
            Err(error) => error.to_string(),
        };
        let notification = toast::titled(
            ToastTone::Error,
            translator.text("appIconApplyFailed").to_owned(),
            message,
        );
        let Ok(()) = window_handle.update(cx, move |_, window: &mut Window, cx| {
            window.push_notification(notification, cx);
        }) else {
            // 设置窗口已关闭，不再有可展示的位置。
            return;
        };
    })
    .detach();
}

/// `agent.platform.setAppIcon` 的 wire 参数。
fn set_icon_params(kind: AppIconKind, source_path: Option<&Path>) -> PlatformSetAppIconParams {
    PlatformSetAppIconParams {
        choice: kind,
        source_path: source_path.map(|path| path.to_string_lossy().into_owned()),
    }
}
