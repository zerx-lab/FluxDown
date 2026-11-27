//! macOS 默认处理程序的确认式写入与按文件口径的查询（`NSWorkspace`，全部为安全 API）。
//!
//! macOS 12 起，改某类文件 / 链接的默认程序会弹「改用 X 还是继续使用 Y」确认框。Launch
//! Services C 接口（`LSSetDefault*`）弹框后立即返回，调用方既等不到也读不到用户的选择；
//! `setDefaultApplicationAtURL:…completionHandler:` 在用户作答（或系统直接拒绝）后才回调。
//! 这里阻塞等待回调，调用方随后复查实际默认值来判定接受 / 拒绝——回调里的错误不区分
//! 「用户拒绝」与「系统拒绝」。所有函数同步阻塞，只能在阻塞线程调用。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};

use super::PlatformError;

/// 等待用户在系统确认框作答的上限；超时按失败处理，晚到的回调被丢弃。
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(300);
/// 回调到达后 Launch Services 默认值落定的最长等待。
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// 系统是否提供带确认回调的设置接口（macOS 12+）；否则调用方退回 Launch Services C 接口
/// （macOS 11 不弹确认框）。
pub(super) fn confirmed_setters_available() -> bool {
    objc2::available!(macos = 12.0)
}

/// 已安装程序：bundle id 与 bundle 位置。
pub(super) struct App {
    pub(super) id: String,
    url: Retained<NSURL>,
}

/// 本程序（外层 `FluxDown.app`）；不在 app bundle 内运行时返回 `None`。
pub(super) fn host_app() -> Option<App> {
    let id = super::host_bundle_id()?;
    let url = match super::host_app_bundle() {
        Some(path) => NSURL::from_directory_path(path)?,
        None => NSBundle::mainBundle().bundleURL(),
    };
    Some(App { id, url })
}

/// Launch Services 为 bundle id 选定的那份安装；未安装返回 `None`。
pub(super) fn app_for_bundle_id(id: &str) -> Option<App> {
    let url = NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(id))?;
    Some(App {
        id: id.to_owned(),
        url,
    })
}

fn bundle_id_of(app: &NSURL) -> Option<String> {
    NSBundle::bundleWithURL(app)?
        .bundleIdentifier()
        .map(|id| id.to_string())
}

/// 临时空文件：按扩展名查询 / 设置「这类文件」的默认程序。与 Finder 同口径——按文件实际
/// UTI 查默认程序，其他程序导出的同扩展名 UTI 压过本 bundle 声明时同样生效。Drop 时删除。
pub(super) struct ProbeFile {
    path: PathBuf,
    url: Retained<NSURL>,
}

impl ProbeFile {
    pub(super) fn create(extension: &str) -> Result<Self, PlatformError> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fluxdown-association-{}-{sequence}.{extension}",
            std::process::id()
        ));
        std::fs::File::create_new(&path)?;
        match NSURL::from_file_path(&path) {
            Some(url) => Ok(Self { path, url }),
            None => {
                remove_probe(&path);
                Err(PlatformError::Failed(format!(
                    "cannot build a file URL for {}",
                    path.display()
                )))
            }
        }
    }

    /// 打开这类文件的默认程序 bundle id。
    pub(super) fn default_app(&self) -> Option<String> {
        let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&self.url)?;
        bundle_id_of(&app)
    }

    /// 能打开这类文件的全部程序（默认程序在前）。仅 macOS 12+，调用前先查
    /// [`confirmed_setters_available`]。
    pub(super) fn candidate_apps(&self) -> Vec<App> {
        NSWorkspace::sharedWorkspace()
            .URLsForApplicationsToOpenURL(&self.url)
            .iter()
            .filter_map(|url| {
                Some(App {
                    id: bundle_id_of(&url)?,
                    url,
                })
            })
            .collect()
    }

    /// 把 `app` 设为这类文件的默认程序，阻塞到系统回调。仅 macOS 12+。
    pub(super) fn set_default_app(&self, app: &App) -> Result<(), PlatformError> {
        wait_for_completion(|handler| {
            NSWorkspace::sharedWorkspace()
                .setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
                    &app.url,
                    &self.url,
                    Some(handler),
                );
        })
    }
}

impl Drop for ProbeFile {
    fn drop(&mut self) {
        remove_probe(&self.path);
    }
}

fn remove_probe(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "failed to remove association probe file");
    }
}

/// 把 `app` 设为 `scheme` 链接的默认程序，阻塞到系统回调。仅 macOS 12+。
pub(super) fn set_default_scheme_app(app: &App, scheme: &str) -> Result<(), PlatformError> {
    let scheme = NSString::from_str(scheme);
    wait_for_completion(|handler| {
        NSWorkspace::sharedWorkspace()
            .setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
                &app.url,
                &scheme,
                Some(handler),
            );
    })
}

/// 发起一次带回调的设置并等待回调。回调报错（用户拒绝或系统拒绝）不算失败：调用方复查
/// 实际默认值；只有等不到回调才是失败。
fn wait_for_completion(
    start: impl FnOnce(&DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), PlatformError> {
    let (sender, receiver) = mpsc::sync_channel::<bool>(1);
    let handler = RcBlock::new(move |error: *mut NSError| {
        if sender.try_send(error.is_null()).is_err() {
            tracing::debug!("default handler completion arrived after the wait ended");
        }
    });
    start(&handler);
    match receiver.recv_timeout(CONFIRM_TIMEOUT) {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::info!("system reported the default handler change as declined or refused");
            Ok(())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::Failed(
            "timed out waiting for the default handler confirmation".to_owned(),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(PlatformError::Failed(
            "default handler completion was dropped".to_owned(),
        )),
    }
}

/// 轮询 `check` 直到成立或 [`SETTLE_TIMEOUT`]：回调与 Launch Services 默认值落定之间可能有
/// 短暂延迟。
pub(super) fn settles(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SETTLE_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只读查询：经真实 Launch Services 按文件口径取默认程序与候选，探针文件随 Drop 删除。
    #[test]
    fn probe_file_queries_real_handlers_and_cleans_up() {
        let probe = ProbeFile::create("pdf").expect("create probe file");
        let path = probe.path.clone();
        assert!(path.is_file());
        let default = probe.default_app().expect("pdf has a default handler");
        assert!(!default.is_empty());
        if confirmed_setters_available() {
            let candidates = probe.candidate_apps();
            assert!(candidates.iter().any(|app| app.id == default));
            assert!(candidates.iter().any(|app| app.id == "com.apple.Preview"));
        }
        drop(probe);
        assert!(!path.exists());
    }
}
