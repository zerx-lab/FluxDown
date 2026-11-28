//! 应用图标（Dock / Cmd-Tab / 任务栏 / 窗口管理器 / 标题栏）：默认、内置备选图标或用户导入的图片，
//! 运行期可切换。
//!
//! 选择存为设备本地偏好 [`APP_ICON_PREF_KEY`]，**只由 agent 写入**（`agent.platform.setAppIcon`
//! 成功之后，图片文件已落盘）。本模块把偏好投影到进程内能安全做的部分，其余由 agent 执行：
//!
//! - agent：托盘、Windows / X11 已存在的窗口图标、Windows `.lnk`、Linux XDG 图标覆盖、通知图标。
//! - 本进程：macOS 应用图标（Dock + Cmd-Tab）、X11 **新**窗口的 `_NET_WM_ICON`
//!   （[`window_options`]）、标题栏 logo、Windows 新窗口 / 重连后请求 agent 补设窗口图标。
//!
//! 默认图标的静态来源：Windows 由 `build.rs` 嵌入 exe 资源（窗口类图标），macOS 由 `.app` 的
//! icns 提供（裸二进制运行时才需要补）；Wayland 没有窗口图标协议，靠 `app_id` 匹配 `.desktop`。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use fluxdown_protocol::app_icon::{
    APP_ICON_PREF_KEY, AppIconChoice, BuiltinAppIcon, PlatformAppIconImageDto,
    PlatformAppIconImageParams,
};
use fluxdown_protocol::method;
use gpui::{App, Global, Image, ImageFormat, WindowOptions};

use crate::agent_client::AgentClient;

/// 与 Linux 桌面入口、Windows 资源 `InternalName`、macOS bundle id 一致的应用 ID。
pub const APP_ID: &str = "com.fluxdown.app";

/// 标题栏 / X11 使用的默认与内置备选图标（方形 PNG）。
const DEFAULT_LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo/fluxdown_logo.png");
const fn builtin_logo_png(icon: BuiltinAppIcon) -> &'static [u8] {
    match icon {
        BuiltinAppIcon::Bolt => include_bytes!("../../../assets/logo/fluxdown_bolt.png"),
        BuiltinAppIcon::Classic => include_bytes!("../../../assets/logo/fluxdown_classic.png"),
        BuiltinAppIcon::Frost => include_bytes!("../../../assets/logo/fluxdown_frost.png"),
        BuiltinAppIcon::Sunset => include_bytes!("../../../assets/logo/fluxdown_sunset.png"),
    }
}
/// macOS 风格（Apple HIG 内缩）的 Dock 素材，来自公共品牌资源。
#[cfg(target_os = "macos")]
const DEFAULT_DOCK_PNG: &[u8] = include_bytes!("../../../assets/logo/macos/app_icon_512.png");
#[cfg(target_os = "macos")]
const fn builtin_dock_png(icon: BuiltinAppIcon) -> &'static [u8] {
    match icon {
        BuiltinAppIcon::Bolt => include_bytes!("../../../assets/logo/macos/app_icon_bolt_512.png"),
        BuiltinAppIcon::Classic => {
            include_bytes!("../../../assets/logo/macos/app_icon_classic_512.png")
        }
        BuiltinAppIcon::Frost => {
            include_bytes!("../../../assets/logo/macos/app_icon_frost_512.png")
        }
        BuiltinAppIcon::Sunset => {
            include_bytes!("../../../assets/logo/macos/app_icon_sunset_512.png")
        }
    }
}

/// 向 agent 取自定义图标母版的边长：覆盖 Dock（512）与 X11（256），再大只是白占内存。
const CUSTOM_IMAGE_SIZE: u32 = 512;
/// `_NET_WM_ICON` 按 u32 逐像素塞进窗口属性，256² 足够任务栏 / 切换器使用。
#[cfg(target_os = "linux")]
const X11_ICON_SIZE: u32 = 256;
/// Apple HIG：图标形状占画布的 824/1024，四周留透明边距（同 `scripts/gen_icons.ts`）。
#[cfg(any(target_os = "macos", test))]
const HIG_CONTENT: u32 = 824;
#[cfg(any(target_os = "macos", test))]
const HIG_CANVAS: u32 = 1024;

/// 偏好投影出的进程内图标状态。
struct AppIconState {
    /// 最近一次按偏好请求的选择；`None` = 尚未应用过任何偏好。自定义图片异步取回，
    /// 回包以它判定是否已过期。
    requested: Option<AppIconChoice>,
    /// X11 新窗口使用的当前图标。
    #[cfg(target_os = "linux")]
    window_rgba: Option<Arc<image::RgbaImage>>,
    /// agent 连接是否在线（离线 → 在线时补发窗口图标同步）。
    #[cfg(windows)]
    online: bool,
}

impl Global for AppIconState {}

/// 一个选择解析出的图像素材。
struct Resolved {
    /// 标题栏 / X11 用的方形 PNG。
    logo_png: Cow<'static, [u8]>,
    /// macOS 应用图标 PNG（已按 HIG 内缩）。
    #[cfg(target_os = "macos")]
    dock_png: Cow<'static, [u8]>,
}

impl Resolved {
    /// 默认 / 内置图标的随包素材；自定义返回 `None`（须向 agent 取）。
    fn builtin(choice: &AppIconChoice) -> Option<Self> {
        match choice {
            AppIconChoice::Default => Some(Self {
                logo_png: Cow::Borrowed(DEFAULT_LOGO_PNG),
                #[cfg(target_os = "macos")]
                dock_png: Cow::Borrowed(DEFAULT_DOCK_PNG),
            }),
            AppIconChoice::Builtin(icon) => Some(Self {
                logo_png: Cow::Borrowed(builtin_logo_png(*icon)),
                #[cfg(target_os = "macos")]
                dock_png: Cow::Borrowed(builtin_dock_png(*icon)),
            }),
            AppIconChoice::Custom { .. } => None,
        }
    }

    /// agent 返回的自定义图片（透明底、等比居中的方形 PNG）。
    fn custom(png: Vec<u8>) -> Result<Self, image::ImageError> {
        #[cfg(target_os = "macos")]
        let dock_png = Cow::Owned(compose_hig_icon(&png, CUSTOM_IMAGE_SIZE)?);
        Ok(Self {
            logo_png: Cow::Owned(png),
            #[cfg(target_os = "macos")]
            dock_png,
        })
    }
}

/// 安装进程级状态。须在 GPUI `App` 启动后、打开任何窗口前调用（主线程）。
pub fn install(cx: &mut App) {
    cx.set_global(AppIconState {
        requested: None,
        #[cfg(target_os = "linux")]
        window_rgba: decode_window_icon(DEFAULT_LOGO_PNG),
        #[cfg(windows)]
        online: false,
    });
}

/// 为窗口补上平台图标信息；所有窗口创建都必须经过这里（`WindowRegistry` 统一调用）。
pub fn window_options(mut options: WindowOptions, cx: &App) -> WindowOptions {
    if options.app_id.is_none() {
        options.app_id = Some(APP_ID.to_owned());
    }
    #[cfg(target_os = "linux")]
    if options.icon.is_none() {
        options.icon = cx
            .try_global::<AppIconState>()
            .and_then(|state| state.window_rgba.clone());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
    options
}

/// 偏好 → 应用图标。偏好快照 / 事件回流都会调用，选择未变化时立即返回。
pub fn apply_preference(
    values: &BTreeMap<String, serde_json::Value>,
    client: &Arc<AgentClient>,
    cx: &mut App,
) {
    let choice = AppIconChoice::from_preference(values.get(APP_ICON_PREF_KEY));
    let state = cx.global_mut::<AppIconState>();
    if state.requested.as_ref() == Some(&choice) {
        return;
    }
    let startup = state.requested.is_none();
    state.requested = Some(choice.clone());
    match Resolved::builtin(&choice) {
        Some(resolved) => commit(&choice, resolved, startup, cx),
        None => fetch_custom(client, startup, cx),
    }
}

/// 向 agent 取自定义图片并应用。回包的修订号与当前请求不符 = 期间又换了图片，丢弃。
fn fetch_custom(client: &Arc<AgentClient>, startup: bool, cx: &mut App) {
    let client = Arc::clone(client);
    cx.spawn(async move |cx| {
        let reply: Result<PlatformAppIconImageDto, _> = client
            .call(
                method::AGENT_PLATFORM_APP_ICON_IMAGE,
                Some(PlatformAppIconImageParams {
                    size: CUSTOM_IMAGE_SIZE,
                }),
            )
            .await;
        cx.update(|cx| match reply {
            Ok(dto) => finish_custom(dto, startup, cx),
            Err(error) => log::warn!("custom app icon image unavailable: {:?}", error.code),
        });
    })
    .detach();
}

fn finish_custom(dto: PlatformAppIconImageDto, startup: bool, cx: &mut App) {
    let requested = cx.global::<AppIconState>().requested.clone();
    if !reply_is_current(requested.as_ref(), &dto.revision) {
        log::debug!("stale custom app icon image ignored: {}", dto.revision);
        return;
    }
    let png = match base64::engine::general_purpose::STANDARD.decode(dto.png) {
        Ok(png) => png,
        Err(error) => {
            log::warn!("custom app icon image is not valid base64: {error}");
            return;
        }
    };
    match Resolved::custom(png) {
        Ok(resolved) => {
            if let Some(choice) = requested {
                commit(&choice, resolved, startup, cx);
            }
        }
        Err(error) => log::warn!("custom app icon image could not be decoded: {error:#}"),
    }
}

/// 回包是否仍对应当前请求的自定义图标。
fn reply_is_current(requested: Option<&AppIconChoice>, reply_revision: &str) -> bool {
    matches!(requested, Some(AppIconChoice::Custom { revision }) if revision == reply_revision)
}

/// 把解析好的素材应用到本进程负责的各处。
fn commit(choice: &AppIconChoice, resolved: Resolved, startup: bool, cx: &mut App) {
    let is_default = *choice == AppIconChoice::Default;
    let logo = (!is_default).then(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            resolved.logo_png.to_vec(),
        ))
    });
    fluxdown_ui_shell::set_app_logo(logo, cx);

    #[cfg(target_os = "linux")]
    {
        cx.global_mut::<AppIconState>().window_rgba = decode_window_icon(&resolved.logo_png);
    }
    #[cfg(target_os = "macos")]
    // 打包的 `.app` 启动时默认图标已由 icns 提供，无需覆盖；之后的运行期切换（含切回默认）照常设置。
    if !(startup && is_default && macos::running_inside_bundle()) {
        macos::set_application_icon(&resolved.dock_png);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = startup;
    #[cfg(windows)]
    if !is_default {
        sync_window_icons(cx);
    }
}

/// Windows：请 agent 给本进程已存在的窗口补设当前图标（失败只记日志，不影响窗口）。
/// 默认图标时无事可做：窗口类图标来自 exe 资源，agent 在切回默认时已清除窗口级覆盖。
#[cfg(windows)]
pub fn sync_window_icons(cx: &App) {
    let non_default = cx
        .try_global::<AppIconState>()
        .and_then(|state| state.requested.as_ref())
        .is_some_and(|choice| *choice != AppIconChoice::Default);
    if !non_default {
        return;
    }
    let Some(desktop) = cx.try_global::<crate::app::Desktop>() else {
        return;
    };
    let client = Arc::clone(&desktop.client);
    let runner = Arc::clone(&client);
    runner.spawn_background(async move {
        let result = client
            .call::<serde_json::Value, serde_json::Value>(
                method::AGENT_PLATFORM_SYNC_WINDOW_ICONS,
                Some(serde_json::json!({})),
            )
            .await;
        if let Err(error) = result {
            log::warn!("failed to sync window icons: {:?}", error.code);
        }
    });
}

/// Windows：agent 连接状态变化。agent 重启后窗口图标随旧进程失效，重新上线时补同步一次。
#[cfg(windows)]
pub fn agent_link_changed(online: bool, cx: &mut App) {
    if !cx.has_global::<AppIconState>() {
        return;
    }
    let state = cx.global_mut::<AppIconState>();
    let reconnected = online && !state.online;
    state.online = online;
    if reconnected {
        sync_window_icons(cx);
    }
}

/// 缩放到 X11 窗口图标尺寸；解码失败时保持无图标（窗口管理器回落）。
#[cfg(target_os = "linux")]
fn decode_window_icon(png: &[u8]) -> Option<Arc<image::RgbaImage>> {
    match image::load_from_memory(png) {
        Ok(decoded) => {
            let resized = decoded.resize(
                X11_ICON_SIZE,
                X11_ICON_SIZE,
                image::imageops::FilterType::Triangle,
            );
            Some(Arc::new(resized.into_rgba8()))
        }
        Err(error) => {
            log::warn!("window icon could not be decoded: {error:#}");
            None
        }
    }
}

/// 把方形图片按 Apple HIG 内缩到 `canvas` 画布的 824/1024 并居中，四周透明；输出 PNG。
#[cfg(any(target_os = "macos", test))]
fn compose_hig_icon(source: &[u8], canvas: u32) -> Result<Vec<u8>, image::ImageError> {
    use image::imageops::{FilterType, overlay};

    let content = (canvas * HIG_CONTENT + HIG_CANVAS / 2) / HIG_CANVAS;
    let resized = image::load_from_memory(source)?
        .resize(content, content, FilterType::Lanczos3)
        .into_rgba8();
    let mut out = image::RgbaImage::new(canvas, canvas);
    let x = i64::from((canvas - resized.width()) / 2);
    let y = i64::from((canvas - resized.height()) / 2);
    overlay(&mut out, &resized, x, y);
    let mut encoded = std::io::Cursor::new(Vec::new());
    out.write_to(&mut encoded, image::ImageFormat::Png)?;
    Ok(encoded.into_inner())
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    /// 可执行文件位于 `*.app/Contents/MacOS/` 之内时，Dock 图标由 bundle 的 icns 提供。
    pub(super) fn running_inside_bundle() -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.ends_with("Contents/MacOS")))
            .unwrap_or(false)
    }

    /// 设置运行期应用图标（Dock 与 Cmd-Tab / 强制退出窗口）。
    pub(super) fn set_application_icon(png: &[u8]) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("app icon must be set on the main thread");
            return;
        };
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png)) else {
            log::warn!("app icon image could not be decoded by AppKit");
            return;
        };
        let application = NSApplication::sharedApplication(mtm);
        // SAFETY: 这是 `crates/*` 唯一获批的 unsafe 例外（`rule://no-unsafe-in-rust`）。
        // `setApplicationIconImage:` 在 objc2 中标为 unsafe 仅因参数可为 `None`（恢复 bundle 图标），
        // 本调用恒传 `Some`。主线程由上面的 `MainThreadMarker` 证明；参数是刚创建且仍存活的
        // 有效 `NSImage`，AppKit 自行持有一份引用，不转移所有权。
        unsafe { application.setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::app_icon::{AppIconChoice, BuiltinAppIcon};

    use super::{compose_hig_icon, reply_is_current};

    fn custom(revision: &str) -> AppIconChoice {
        AppIconChoice::Custom {
            revision: revision.to_owned(),
        }
    }

    #[test]
    fn stale_or_foreign_replies_are_rejected() {
        assert!(reply_is_current(Some(&custom("a1b2")), "a1b2"));
        assert!(!reply_is_current(Some(&custom("a1b2")), "c3d4"));
        assert!(!reply_is_current(
            Some(&AppIconChoice::Builtin(BuiltinAppIcon::Bolt)),
            "a1b2"
        ));
        assert!(!reply_is_current(Some(&AppIconChoice::Default), "a1b2"));
        assert!(!reply_is_current(None, "a1b2"));
    }

    #[test]
    fn hig_inset_centers_content_with_transparent_margin() -> Result<(), image::ImageError> {
        let mut source = image::RgbaImage::new(64, 64);
        source
            .pixels_mut()
            .for_each(|pixel| *pixel = image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png)?;

        let composed =
            image::load_from_memory(&compose_hig_icon(png.get_ref(), 512)?)?.into_rgba8();
        assert_eq!(composed.dimensions(), (512, 512));
        // 内容边长 round(512 × 824/1024) = 412，四周各留 50。
        assert_eq!(composed.get_pixel(10, 10).0[3], 0);
        assert_eq!(composed.get_pixel(49, 256).0[3], 0);
        assert_eq!(composed.get_pixel(50, 256).0[3], 255);
        assert_eq!(composed.get_pixel(461, 256).0[3], 255);
        assert_eq!(composed.get_pixel(462, 256).0[3], 0);
        Ok(())
    }
}
