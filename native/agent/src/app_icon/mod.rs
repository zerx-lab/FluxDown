//! 桌面应用图标（默认 / 内置备选图标 / 用户导入的自定义图片）的 agent 侧服务。
//!
//! 偏好 [`APP_ICON_PREF_KEY`] 只由 agent 写：`agent.platform.setAppIcon` 先把图片落盘，再写偏好，保证
//! 偏好引用的文件一定存在（见 [`resolve_request`]）。[`AppIconService`] 订阅偏好变化并在启动时对账一次，
//! 把当前选择落到 agent 负责的表面：通知图标、Windows / X11 现有窗口、Windows `.lnk`、Linux XDG
//! 覆盖；托盘由外壳控制器读同一偏好（见 [`tray_icon_source`]），macOS Dock 与新窗口由桌面进程自己设置。
//!
//! 空闲静默：没有定时器、没有周期 IO；只有偏好真的变化（或启动对账）才会做事，且所有写入先比较。

pub(crate) mod image;
mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use fluxdown_protocol::app_icon::{
    APP_ICON_PREF_KEY, AppIconChoice, AppIconKind, BuiltinAppIcon, PlatformAppIconImageDto,
    PlatformSetAppIconParams,
};
use fluxdown_protocol::{AgentEvent, AgentPreferencesDto, ServiceEvent};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::event_hub::AgentEventHub;
use crate::notification::{Notifier, NotifierIcon};
use crate::platform::PlatformError;
use crate::shell::{ShellState, TrayIconSource};
use crate::sync::SyncService;
#[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
pub(crate) use store::write_atomic;
pub(crate) use store::{IconStore, StoreError};

/// 内置备选图标（512²，各自带圆角底）；与 `scripts/gen_icons.ts` 产出一一对应。
pub(crate) const fn builtin_png(icon: BuiltinAppIcon) -> &'static [u8] {
    match icon {
        BuiltinAppIcon::Bolt => include_bytes!("../../../../assets/logo/fluxdown_bolt.png"),
        BuiltinAppIcon::Classic => include_bytes!("../../../../assets/logo/fluxdown_classic.png"),
        BuiltinAppIcon::Frost => include_bytes!("../../../../assets/logo/fluxdown_frost.png"),
        BuiltinAppIcon::Sunset => include_bytes!("../../../../assets/logo/fluxdown_sunset.png"),
    }
}

/// 默认图标源图（X11 `_NET_WM_ICON` 还原用）。
#[cfg(target_os = "linux")]
pub(crate) const DEFAULT_LOGO_PNG: &[u8] =
    include_bytes!("../../../../assets/logo/fluxdown_logo.png");

/// 托盘图标边长：Windows 通知区 16–32px（含缩放），Linux SNI 宿主自行缩放。
const TRAY_ICON_PX: u32 = if cfg!(windows) { 64 } else { 128 };
/// 通知图标边长。
const NOTIFICATION_ICON_PX: u32 = 256;

/// 图标文件错误 → RPC 层统一的 [`PlatformError`]：缺母版 = `NotFound`，其余带原因。
pub(crate) fn platform_error(error: StoreError) -> PlatformError {
    match error {
        StoreError::NoCustomIcon => PlatformError::NotFound(PathBuf::from("custom app icon")),
        StoreError::Io(error) => PlatformError::Io(error),
        error @ StoreError::Image(_) => PlatformError::Failed(error.to_string()),
    }
}

/// 偏好值实际生效的选择：自定义但母版缺失或修订号与磁盘不符时回落默认。
pub(crate) fn effective_choice(choice: &AppIconChoice, store: &IconStore) -> AppIconChoice {
    match choice {
        AppIconChoice::Custom { revision } => match store.master_revision() {
            Some(current) if current == *revision => choice.clone(),
            _ => AppIconChoice::Default,
        },
        _ => choice.clone(),
    }
}

/// `agent.platform.setAppIcon`：导入 / 校验图片，返回要写入偏好的选择。阻塞（读写文件、解码）。
pub(crate) fn resolve_request(
    store: &IconStore,
    params: &PlatformSetAppIconParams,
) -> Result<AppIconChoice, PlatformError> {
    match params.choice {
        AppIconKind::Default => Ok(AppIconChoice::Default),
        AppIconKind::Bolt => Ok(AppIconChoice::Builtin(BuiltinAppIcon::Bolt)),
        AppIconKind::Classic => Ok(AppIconChoice::Builtin(BuiltinAppIcon::Classic)),
        AppIconKind::Frost => Ok(AppIconChoice::Builtin(BuiltinAppIcon::Frost)),
        AppIconKind::Sunset => Ok(AppIconChoice::Builtin(BuiltinAppIcon::Sunset)),
        AppIconKind::Custom => {
            let revision = match params.source_path.as_deref() {
                Some(source) => store.import(Path::new(source)),
                None => store.master_revision().ok_or(StoreError::NoCustomIcon),
            }
            .map_err(|error| {
                tracing::debug!(%error, "app icon import rejected");
                platform_error(error)
            })?;
            Ok(AppIconChoice::Custom { revision })
        }
    }
}

/// `agent.platform.appIconImage`：当前母版缩放到 `size`（钳制到协议范围）。阻塞。
pub(crate) fn icon_image(
    store: &IconStore,
    size: u32,
) -> Result<PlatformAppIconImageDto, PlatformError> {
    let (revision, master) = store.master().map_err(platform_error)?;
    let png = image::resized_png(&master, image::clamp_request_size(size))
        .map_err(|error| PlatformError::Failed(error.to_string()))?;
    Ok(PlatformAppIconImageDto {
        revision,
        png: base64::engine::general_purpose::STANDARD.encode(png),
    })
}

/// `agent.platform.syncWindowIcons`：只做「现有窗口」这一步。阻塞。
pub(crate) fn sync_window_icons(
    store: &IconStore,
    choice: &AppIconChoice,
) -> Result<(), PlatformError> {
    crate::platform::app_icon::sync_window_icons(store, &effective_choice(choice, store))
}

/// 偏好值解析（缺省 / 非法 = 默认）。
pub(crate) fn choice_of(preferences: &AgentPreferencesDto) -> AppIconChoice {
    AppIconChoice::from_preference(preferences.values.get(APP_ICON_PREF_KEY))
}

/// 托盘图标：默认由外壳按平台选内置图；其余是缩放后的 PNG。macOS 托盘保持单色模板图。
/// 图片读不出来时回落默认（偏好随后由 [`AppIconService`] 修正）。
pub(crate) fn tray_icon_source(choice: &AppIconChoice, store: &IconStore) -> TrayIconSource {
    if cfg!(target_os = "macos") {
        return TrayIconSource::Default;
    }
    match scaled_png(store, choice, TRAY_ICON_PX) {
        Ok(Some(png)) => TrayIconSource::Image {
            key: choice.to_preference(),
            png: png.into(),
        },
        Ok(None) => TrayIconSource::Default,
        Err(error) => {
            tracing::warn!(%error, "tray app icon unavailable; using the default icon");
            TrayIconSource::Default
        }
    }
}

fn scaled_png(
    store: &IconStore,
    choice: &AppIconChoice,
    size: u32,
) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(source) = store.image_for(choice)? else {
        return Ok(None);
    };
    Ok(Some(image::resized_png(&source, size)?))
}

/// 通知图标；默认返回 `None`（用内置图）。仅 Windows / Linux 的通知使用。
fn notifier_icon(store: &IconStore, choice: &AppIconChoice) -> Option<NotifierIcon> {
    if !cfg!(any(windows, target_os = "linux")) {
        return None;
    }
    match scaled_png(store, choice, NOTIFICATION_ICON_PX) {
        Ok(Some(png)) => Some(NotifierIcon {
            tag: choice.to_preference().replace(':', "-"),
            png: png.into(),
        }),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "notification app icon unavailable; using the default icon");
            None
        }
    }
}

/// 把选择落到 agent 负责的各个表面；每一步独立，失败只记日志。阻塞，调用方放进 `spawn_blocking`。
///
/// 返回程序本体图标（Windows exe）是否因被占用而**待办**，见 [`BinaryRetry`]。
fn apply_choice(store: &IconStore, notifier: &Notifier, choice: &AppIconChoice) -> bool {
    notifier.set_icon(notifier_icon(store, choice));
    if let Err(error) = crate::platform::app_icon::sync_window_icons(store, choice) {
        tracing::warn!(%error, "could not apply app icon to existing windows");
    }
    if let Err(error) = crate::platform::app_icon::sync_persistent_entries(store, choice) {
        tracing::warn!(%error, "could not apply app icon to persistent entries");
    }
    sync_binary_icon(store, choice)
}

/// 程序本体（Windows exe / macOS bundle）图标；返回是否待办。
fn sync_binary_icon(store: &IconStore, choice: &AppIconChoice) -> bool {
    match crate::platform::app_icon::sync_binary_icon(store, choice) {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(%error, "could not apply app icon to the program binary");
            false
        }
    }
}

/// 程序本体图标补丁的待办状态（Windows：桌面进程占着 exe，无法替换）。
///
/// 没有定时器常驻：只有补丁真的待办时才会等官方 UI 断开（桌面进程随后退出），再按有界退避重试几次；
/// 仍不成功就回到等待，直到下一次 UI 断开、偏好变化或 agent 重启（启动对账总会再比较一遍）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryRetry {
    Idle,
    /// 等官方 UI 断开：`seen` 是待办（或上一轮退避）时的断开代数，只认严格更新的代数
    /// （见 `ShellState::wait_ui_left_after`），之前已发生的断开不触发。
    WaitingForUi {
        seen: u64,
    },
    /// 第 `step` 次退避，`due` 时刻重试。
    Backoff {
        step: usize,
        due: tokio::time::Instant,
        seen: u64,
    },
}

/// UI 断开后等待桌面进程退出的退避序列；用尽后回到 [`BinaryRetry::WaitingForUi`]。
const BINARY_RETRY_BACKOFF: [std::time::Duration; 3] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(10),
];

impl BinaryRetry {
    /// 第 `step` 次退避；序列用尽则继续等 UI。
    fn backoff(step: usize, now: tokio::time::Instant, seen: u64) -> Self {
        match BINARY_RETRY_BACKOFF.get(step) {
            Some(delay) => Self::Backoff {
                step,
                due: now + *delay,
                seen,
            },
            None => Self::WaitingForUi { seen },
        }
    }

    /// 刚发现待办：已经没有 UI 连接就直接开始退避（进程可能正在退出），否则等 UI 断开。
    fn pending(ui_connected: bool, now: tokio::time::Instant, generation: u64) -> Self {
        if ui_connected {
            Self::WaitingForUi { seen: generation }
        } else {
            Self::backoff(0, now, generation)
        }
    }

    fn due(self) -> Option<tokio::time::Instant> {
        match self {
            Self::Backoff { due, .. } => Some(due),
            Self::Idle | Self::WaitingForUi { .. } => None,
        }
    }

    fn waiting_since(self) -> Option<u64> {
        match self {
            Self::WaitingForUi { seen } => Some(seen),
            Self::Idle | Self::Backoff { .. } => None,
        }
    }
}

/// 串行化「改母版 + 写偏好」：`setAppIcon` 在导入与写偏好之间持有，服务回落默认前持有，避免回落
/// 覆盖刚导入的新选择。
static PREFERENCE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn preference_lock() -> tokio::sync::MutexGuard<'static, ()> {
    PREFERENCE_LOCK.lock().await
}

/// 订阅偏好、启动对账并串行落实应用图标。
pub(crate) struct AppIconService {
    store: IconStore,
    sync: Arc<SyncService>,
    events: AgentEventHub,
    notifier: Arc<Notifier>,
    /// 官方 UI 连接状态：程序本体图标补丁被运行中的桌面进程挡住时，等它断开后重试。
    shell: Arc<ShellState>,
    /// 旧 Flutter 客户端的 engine 数据目录（一次性迁移读取其图标文件与便携设置）。
    legacy_data_dir: PathBuf,
}

impl AppIconService {
    pub(crate) fn new(
        agent_data_dir: &Path,
        legacy_data_dir: PathBuf,
        sync: Arc<SyncService>,
        events: AgentEventHub,
        notifier: Arc<Notifier>,
        shell: Arc<ShellState>,
    ) -> Self {
        Self {
            store: IconStore::new(agent_data_dir),
            sync,
            events,
            notifier,
            shell,
            legacy_data_dir,
        }
    }

    /// 运行到 `cancel`：先一次性迁移旧选择，再对账并响应偏好变化。工作串行：一次只处理一个选择，
    /// 处理期间到达的事件在广播队列里排队，之后按最新值判断。
    pub(crate) async fn run(self, cancel: CancellationToken) {
        let (mut receiver, _) = self.events.subscribe_and_snapshot();
        let has_preference = self
            .events
            .inspect(|snapshot| snapshot.preferences.values.contains_key(APP_ICON_PREF_KEY));
        if !has_preference {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = self.migrate_legacy_choice() => {}
            }
        }
        let mut applied: Option<AppIconChoice> = None;
        let mut retry = BinaryRetry::Idle;
        self.reconcile(self.current_choice(), &mut applied, &mut retry)
            .await;
        loop {
            let retry_due = retry.due();
            let waiting_since = retry.waiting_since();
            tokio::select! {
                () = cancel.cancelled() => return,
                generation = self.shell.wait_ui_left_after(waiting_since.unwrap_or_default()),
                    if waiting_since.is_some() =>
                {
                    retry = BinaryRetry::backoff(0, tokio::time::Instant::now(), generation);
                    continue;
                }
                () = tokio::time::sleep_until(retry_due.unwrap_or_else(tokio::time::Instant::now)),
                    if retry_due.is_some() =>
                {
                    self.retry_binary_icon(applied.as_ref(), &mut retry).await;
                    continue;
                }
                frame = receiver.recv() => match frame {
                    Ok(frame) => {
                        if !matches!(
                            frame.event,
                            ServiceEvent::Agent(AgentEvent::PreferencesChanged(_))
                        ) {
                            continue;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
            // 一律按当前快照判断，不用事件携带的值：排队中的旧事件（如导入新图片之前的
            // `custom:<旧修订号>`）会让母版校验失败，误把刚写入的新选择改回默认。
            self.reconcile(self.current_choice(), &mut applied, &mut retry)
                .await;
        }
    }

    fn current_choice(&self) -> AppIconChoice {
        self.events
            .inspect(|snapshot| choice_of(&snapshot.preferences))
    }

    /// 校验偏好引用的文件，必要时回落默认并改写偏好；选择与上次落实的不同才执行。
    async fn reconcile(
        &self,
        wanted: AppIconChoice,
        applied: &mut Option<AppIconChoice>,
        retry: &mut BinaryRetry,
    ) {
        let store = self.store.clone();
        let candidate = wanted.clone();
        let effective =
            match tokio::task::spawn_blocking(move || effective_choice(&candidate, &store)).await {
                Ok(effective) => effective,
                Err(error) => {
                    tracing::error!(%error, "app icon validation task failed");
                    return;
                }
            };
        if effective != wanted {
            // 与 `setAppIcon` 的「导入 + 写偏好」互斥：持锁后偏好仍是这次校验的值，才说明母版确实
            // 缺失 / 不符；否则期间有新导入，交给它触发的下一次事件。
            let _guard = preference_lock().await;
            if self.current_choice() != wanted {
                return;
            }
            tracing::warn!(
                preference = %wanted.to_preference(),
                "custom app icon image is missing or changed; falling back to the default icon"
            );
            self.write_preference(&effective).await;
        }
        if applied.as_ref() == Some(&effective) {
            return;
        }
        let store = self.store.clone();
        let notifier = Arc::clone(&self.notifier);
        let choice = effective.clone();
        // 先取代数再落实：落实期间发生的断开也算「更新的断开」。
        let generation = self.shell.ui_left_generation();
        match tokio::task::spawn_blocking(move || apply_choice(&store, &notifier, &choice)).await {
            Ok(binary_pending) => {
                *applied = Some(effective);
                *retry = if binary_pending {
                    BinaryRetry::pending(
                        self.shell.ui_clients() > 0,
                        tokio::time::Instant::now(),
                        generation,
                    )
                } else {
                    BinaryRetry::Idle
                };
            }
            Err(error) => tracing::error!(%error, "app icon apply task failed"),
        }
    }

    /// 程序本体图标补丁的一次重试（比较后再写，已完成则什么都不做）。
    async fn retry_binary_icon(&self, applied: Option<&AppIconChoice>, retry: &mut BinaryRetry) {
        let BinaryRetry::Backoff { step, .. } = *retry else {
            return;
        };
        let Some(choice) = applied.cloned() else {
            *retry = BinaryRetry::Idle;
            return;
        };
        let store = self.store.clone();
        // 先取代数再重试：重试期间发生的断开也算「更新的断开」。
        let generation = self.shell.ui_left_generation();
        let pending =
            match tokio::task::spawn_blocking(move || sync_binary_icon(&store, &choice)).await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::error!(%error, "app icon binary retry task failed");
                    false
                }
            };
        *retry = if pending {
            BinaryRetry::backoff(step + 1, tokio::time::Instant::now(), generation)
        } else {
            BinaryRetry::Idle
        };
    }

    async fn write_preference(&self, choice: &AppIconChoice) {
        if let Err(error) = self
            .sync
            .set_local_preference(
                APP_ICON_PREF_KEY.to_owned(),
                Value::String(choice.to_preference()),
                false,
            )
            .await
        {
            tracing::warn!(%error, "could not write app icon preference");
        }
    }

    /// 旧 Flutter 的选择只迁移一次（偏好缺省时）：`bolt` 直接沿用；`custom` 仅在旧版导入的图片
    /// 仍在磁盘上时导入；`default` / 其他值写入 `default` 作为已迁移标记。找不到旧设置时什么都
    /// 不写。任何失败都只记日志，不影响启动。
    async fn migrate_legacy_choice(&self) {
        let Some(legacy) = crate::device_identity::load_flutter_preference(
            &self.legacy_data_dir,
            LEGACY_CHOICE_KEY,
            true,
        )
        .await
        else {
            return;
        };
        let choice = match legacy.trim() {
            "bolt" => AppIconChoice::Builtin(BuiltinAppIcon::Bolt),
            "custom" => {
                let store = self.store.clone();
                let dir = self.legacy_data_dir.clone();
                match tokio::task::spawn_blocking(move || import_legacy_custom(&store, &dir)).await
                {
                    Ok(Some(revision)) => AppIconChoice::Custom { revision },
                    Ok(None) => AppIconChoice::Default,
                    Err(error) => {
                        tracing::warn!(%error, "legacy app icon import task failed");
                        return;
                    }
                }
            }
            _ => AppIconChoice::Default,
        };
        tracing::info!(choice = %choice.to_preference(), "migrated the Flutter-era app icon choice");
        self.write_preference(&choice).await;
    }
}

/// 旧 Flutter 版保存选择的键。
const LEGACY_CHOICE_KEY: &str = "app_icon_choice";
/// 旧版导入图片落在 `<engine 数据目录>/icons/`：预览图优先（已是规整尺寸），回退原图。
const LEGACY_CUSTOM_FILES: [&str; 2] = ["custom_icon_preview.png", "custom_icon.png"];

fn import_legacy_custom(store: &IconStore, legacy_data_dir: &Path) -> Option<String> {
    for name in LEGACY_CUSTOM_FILES {
        let path = legacy_data_dir.join("icons").join(name);
        if !path.is_file() {
            continue;
        }
        match store.import(&path) {
            Ok(revision) => return Some(revision),
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "legacy custom app icon not importable");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::app_icon::{APP_ICON_IMAGE_MAX_SIZE, APP_ICON_IMAGE_MIN_SIZE};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-app-icon-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_png(dir: &Path, name: &str, color: [u8; 4]) -> PathBuf {
        let image = ::image::RgbaImage::from_pixel(24, 24, ::image::Rgba(color));
        let path = dir.join(name);
        std::fs::write(&path, image::encode_png(&image).expect("encode")).expect("write");
        path
    }

    fn custom_request(source: Option<&Path>) -> PlatformSetAppIconParams {
        PlatformSetAppIconParams {
            choice: AppIconKind::Custom,
            source_path: source.map(|path| path.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn missing_master_falls_back_to_default() {
        let dir = temp_dir("missing-master");
        let store = IconStore::new(&dir);
        let custom = AppIconChoice::Custom {
            revision: "0a1b2c3d4e5f".to_owned(),
        };

        assert_eq!(effective_choice(&custom, &store), AppIconChoice::Default);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn master_with_a_different_revision_falls_back_to_default() {
        let dir = temp_dir("stale-revision");
        let store = IconStore::new(&dir);
        let revision = store
            .import(&write_png(&dir, "a.png", [255, 0, 0, 255]))
            .expect("import");

        let current = AppIconChoice::Custom {
            revision: revision.clone(),
        };
        let stale = AppIconChoice::Custom {
            revision: "ffffffffffff".to_owned(),
        };

        assert_eq!(effective_choice(&current, &store), current);
        assert_eq!(effective_choice(&stale, &store), AppIconChoice::Default);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn built_in_choices_never_need_files() {
        let dir = temp_dir("builtin");
        let store = IconStore::new(&dir);

        for icon in BuiltinAppIcon::ALL {
            let choice = AppIconChoice::Builtin(icon);
            assert_eq!(effective_choice(&choice, &store), choice);
        }
        assert_eq!(
            effective_choice(&AppIconChoice::Default, &store),
            AppIconChoice::Default
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn switching_back_to_custom_requires_an_imported_image() {
        let dir = temp_dir("switch-back");
        let store = IconStore::new(&dir);

        assert!(matches!(
            resolve_request(&store, &custom_request(None)),
            Err(PlatformError::NotFound(_))
        ));

        let source = write_png(&dir, "a.png", [10, 20, 30, 255]);
        let imported = resolve_request(&store, &custom_request(Some(&source))).expect("import");
        let again = resolve_request(&store, &custom_request(None)).expect("switch back");
        assert_eq!(imported, again);
        assert!(matches!(imported, AppIconChoice::Custom { .. }));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn unreadable_import_is_an_error_and_leaves_no_master() {
        let dir = temp_dir("bad-import");
        let store = IconStore::new(&dir);
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"nope").expect("write");

        assert!(matches!(
            resolve_request(&store, &custom_request(Some(&bad))),
            Err(PlatformError::Failed(_))
        ));
        assert!(matches!(
            resolve_request(&store, &custom_request(Some(Path::new("relative.png")))),
            Err(PlatformError::Failed(_))
        ));
        assert_eq!(store.master_revision(), None);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn icon_image_returns_current_revision_at_a_clamped_size() {
        let dir = temp_dir("image-rpc");
        let store = IconStore::new(&dir);
        assert!(matches!(
            icon_image(&store, 64),
            Err(PlatformError::NotFound(_))
        ));
        let revision = store
            .import(&write_png(&dir, "a.png", [1, 2, 3, 255]))
            .expect("import");

        for (requested, expected) in [
            (1, APP_ICON_IMAGE_MIN_SIZE),
            (64, 64),
            (100_000, APP_ICON_IMAGE_MAX_SIZE),
        ] {
            let dto = icon_image(&store, requested).expect("image");
            let png = base64::engine::general_purpose::STANDARD
                .decode(dto.png)
                .expect("base64");
            let decoded = image::decode_limited(&png).expect("png");
            assert_eq!(dto.revision, revision);
            assert_eq!(decoded.dimensions(), (expected, expected));
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn legacy_custom_import_prefers_the_preview_and_skips_broken_files() {
        let legacy = temp_dir("legacy");
        let icons = legacy.join("icons");
        std::fs::create_dir_all(&icons).expect("icons dir");
        write_png(&icons, "custom_icon.png", [0, 255, 0, 255]);
        std::fs::write(icons.join("custom_icon_preview.png"), b"broken").expect("broken preview");
        let agent = temp_dir("legacy-agent");
        let store = IconStore::new(&agent);

        let revision = import_legacy_custom(&store, &legacy).expect("falls back to original");
        assert_eq!(store.master_revision().as_deref(), Some(revision.as_str()));

        std::fs::remove_file(icons.join("custom_icon.png")).expect("remove original");
        assert_eq!(import_legacy_custom(&store, &legacy), None);
        std::fs::remove_dir_all(&legacy).expect("cleanup");
        std::fs::remove_dir_all(&agent).expect("cleanup");
    }

    #[test]
    fn tray_icon_follows_the_choice_and_falls_back_when_the_image_is_gone() {
        let dir = temp_dir("tray");
        let store = IconStore::new(&dir);
        let custom = AppIconChoice::Custom {
            revision: "0a1b2c3d4e5f".to_owned(),
        };

        assert_eq!(
            tray_icon_source(&AppIconChoice::Default, &store),
            TrayIconSource::Default
        );
        assert_eq!(tray_icon_source(&custom, &store), TrayIconSource::Default);
        if !cfg!(target_os = "macos") {
            for icon in BuiltinAppIcon::ALL {
                assert!(matches!(
                    tray_icon_source(&AppIconChoice::Builtin(icon), &store),
                    TrayIconSource::Image { ref key, .. } if key == icon.key()
                ));
            }
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
