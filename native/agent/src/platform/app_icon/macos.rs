//! macOS：让应用 bundle 本身（Finder / 访达「显示简介」/ 活动监视器 / 程序坞未运行时的图标）跟随
//! 应用图标设置，做法是 Finder 的「自定义图标」：`NSWorkspace.setIcon:forFile:options:` 在 bundle
//! 根写 `Icon\r` 文件与 `com.apple.FinderInfo` 标记，**不碰 bundle 内任何被签名封装的内容**。
//!
//! # 目标
//!
//! 外层 `FluxDown.app` 与其 `Contents/Helpers/FluxDownAgent.app`（agent 所在的辅助 bundle，活动
//! 监视器显示的是它）。不是这套打包布局（开发期的扁平布局）时什么都不做。
//!
//! # 对账（比较后再写，空闲静默）
//!
//! 由 [`super::binary_record`] 判定：bundle 根目录是否已有 `Icon\r`，加上 `binary-icon.json` 里该
//! bundle 上次落实的偏好值，二者一致才跳过。更新后新 bundle 没有 `Icon\r`，下一次对账（启动即做）
//! 自动重新应用。默认图标 = `setIcon(nil)` 移除我们设置过的图标，bundle 回到出厂的已签名原状。
//!
//! # 升级安全
//!
//! 带 `Icon\r` 的 bundle 通不过 `codesign --verify --strict`（“resource fork, Finder information,
//! or similar detritus not allowed”），但非 strict 校验与 `codesign -dv` 都不受影响。更新器只对
//! **下载下来的新 bundle** 做 strict 校验，对运行中的 bundle 只读 `-dv` 的 TeamIdentifier，并把旧
//! bundle 整体改名、新 bundle 换入，所以旧 bundle 上的自定义图标不会阻碍升级。
//!
//! # 线程
//!
//! `setIcon:forFile:options:` 文档标明线程安全；调用方在阻塞线程池里执行，每次包一层自动释放池。

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use fluxdown_protocol::app_icon::{AppIconChoice, BuiltinAppIcon};
use objc2::AnyThread as _;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
use objc2_foundation::{NSData, NSString};

use super::binary_record::{BinaryRecord, BundleAction, bundle_action};
use crate::app_icon::IconStore;
use crate::app_icon::image::{self as pipeline, ImageError};

/// 内置图标的 macOS 版（已按 Apple HIG 内缩，512²）。
const fn builtin_bundle_png(icon: BuiltinAppIcon) -> &'static [u8] {
    match icon {
        BuiltinAppIcon::Bolt => {
            include_bytes!("../../../../../assets/logo/macos/app_icon_bolt_512.png")
        }
        BuiltinAppIcon::Classic => {
            include_bytes!("../../../../../assets/logo/macos/app_icon_classic_512.png")
        }
        BuiltinAppIcon::Frost => {
            include_bytes!("../../../../../assets/logo/macos/app_icon_frost_512.png")
        }
        BuiltinAppIcon::Sunset => {
            include_bytes!("../../../../../assets/logo/macos/app_icon_sunset_512.png")
        }
    }
}

/// Finder 在 bundle 根目录存放自定义图标的文件名（以回车符结尾）。
const CUSTOM_ICON_FILE: &str = "Icon\r";
/// 自定义图标的画布边长（Finder 会按需缩放）。
const BUNDLE_ICON_CANVAS: u32 = 1024;

/// 把选择落到外层与辅助 bundle 上。非打包布局无操作。
pub(super) fn sync_bundle_icons(
    store: &IconStore,
    choice: &AppIconChoice,
    icons_dir: &Path,
) -> Result<(), String> {
    let bundles = bundle_targets();
    if bundles.is_empty() {
        tracing::debug!("not running from the packaged app bundle; skipping bundle icon sync");
        return Ok(());
    }
    sync_bundles(&bundles, store, choice, icons_dir)
}

/// 对给定 bundle 逐个对账并落实（与布局探测分开，便于单测）。
fn sync_bundles(
    bundles: &[PathBuf],
    store: &IconStore,
    choice: &AppIconChoice,
    icons_dir: &Path,
) -> Result<(), String> {
    let wanted = match choice {
        AppIconChoice::Default => None,
        other => Some(other.to_preference()),
    };
    let mut record = BinaryRecord::load(icons_dir);
    let mut icon: Option<Vec<u8>> = None;
    let mut failures = Vec::new();
    let mut changed = false;
    for bundle in bundles {
        let key = bundle.to_string_lossy().into_owned();
        let has_icon = bundle.join(CUSTOM_ICON_FILE).exists();
        let action = bundle_action(wanted.as_deref(), record.get(&key), has_icon);
        let result = match action {
            BundleAction::Skip => continue,
            BundleAction::Forget => Ok(()),
            BundleAction::Remove => set_bundle_icon(bundle, None),
            BundleAction::Apply => bundle_icon_png(store, choice, &mut icon)
                .and_then(|png| set_bundle_icon(bundle, Some(png))),
        };
        match result {
            Ok(()) => {
                changed |= match &wanted {
                    Some(preference) if action == BundleAction::Apply => {
                        record.set(&key, preference)
                    }
                    _ => record.remove(&key),
                };
                tracing::info!(bundle = %bundle.display(), ?action, "app bundle icon updated");
            }
            Err(error) => failures.push(format!("{}: {error}", bundle.display())),
        }
    }
    if changed && let Err(error) = record.save(icons_dir) {
        tracing::warn!(%error, "could not save the app bundle icon record");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "could not set the app bundle icon ({})",
            failures.join("; ")
        ))
    }
}

/// 外层 bundle 与辅助 bundle；不是打包布局返回空。
fn bundle_targets() -> Vec<PathBuf> {
    let Some(outer) = crate::platform::host_app_bundle() else {
        return Vec::new();
    };
    let helper = std::env::current_exe().ok().and_then(|exe| {
        crate::platform::host_macos_dir(&exe)?;
        // `<Helper>.app/Contents/MacOS/<agent>` → `<Helper>.app`
        Some(exe.parent()?.parent()?.parent()?.to_path_buf())
    });
    let mut targets = vec![outer];
    targets.extend(helper);
    targets
}

/// 非默认选择对应的 bundle 图标 PNG（懒生成，多个 bundle 共用）。
fn bundle_icon_png<'a>(
    store: &IconStore,
    choice: &AppIconChoice,
    cache: &'a mut Option<Vec<u8>>,
) -> Result<Cow<'a, [u8]>, String> {
    match choice {
        AppIconChoice::Default => Err("the default icon needs no bundle image".to_owned()),
        AppIconChoice::Builtin(icon) => Ok(Cow::Borrowed(builtin_bundle_png(*icon))),
        AppIconChoice::Custom { .. } => {
            if cache.is_none() {
                let (_, master) = store.master().map_err(|error| error.to_string())?;
                let composed = pipeline::compose_hig_icon(&master, BUNDLE_ICON_CANVAS);
                *cache = Some(
                    pipeline::encode_png(&composed)
                        .map_err(|error: ImageError| error.to_string())?,
                );
            }
            cache
                .as_deref()
                .map(Cow::Borrowed)
                .ok_or_else(|| "bundle icon image missing".to_owned())
        }
    }
}

/// `Some(png)` 设置自定义图标，`None` 移除。
fn set_bundle_icon(bundle: &Path, png: Option<Cow<'_, [u8]>>) -> Result<(), String> {
    autoreleasepool(|_| {
        let image = match png {
            Some(bytes) => {
                let data = NSData::with_bytes(&bytes);
                Some(
                    NSImage::initWithData(NSImage::alloc(), &data)
                        .ok_or_else(|| "NSImage could not decode the icon".to_owned())?,
                )
            }
            None => None,
        };
        let path = NSString::from_str(&bundle.to_string_lossy());
        let applied = NSWorkspace::sharedWorkspace().setIcon_forFile_options(
            image.as_deref(),
            &path,
            NSWorkspaceIconCreationOptions(0),
        );
        if applied {
            Ok(())
        } else {
            Err(
                "NSWorkspace refused to change the bundle icon (permission or App Management)"
                    .to_owned(),
            )
        }
    })
}
