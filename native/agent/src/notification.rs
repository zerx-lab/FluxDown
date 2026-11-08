//! 系统通知统一入口：macOS 使用 agent helper bundle 的 UserNotifications 身份；
//! Windows 使用 FluxDown AUMID 与持久化的受限操作令牌；Linux 保持 D-Bus 通知。
//! 完成通知点击正文定位文件，两个按钮分别打开文件 / 所在文件夹。
//! 投递与权限查询同步阻塞，调用方放进 `spawn_blocking`；macOS delegate 在主线程初始化。

use std::path::PathBuf;

mod actions;
use actions::CompletionActions;
pub(crate) use actions::completion_path;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod windows;

/// 在 macOS 主线程、事件循环启动前安装原生通知 delegate；此处不请求权限。
#[cfg(all(target_os = "macos", feature = "desktop"))]
pub(crate) fn initialize() {
    if let Err(error) = macos::initialize() {
        tracing::warn!(%error, "native notification initialization unavailable");
    }
}

/// Windows 通知激活只执行持久化令牌指向的文件操作，不启动下载服务。
#[cfg(windows)]
pub fn handle_activation(args: &[String]) -> Option<Result<(), String>> {
    windows::handle_activation(args)
}
#[cfg(any(windows, target_os = "linux"))]
use std::path::Path;
use std::sync::Arc;
#[cfg(any(windows, target_os = "linux"))]
use std::sync::{Mutex, PoisonError};

/// 通知显示的应用名。
#[cfg(any(windows, target_os = "linux"))]
const APP_NAME: &str = "FluxDown";

/// 通知图标（写入 agent 数据目录，供 Windows `IconUri` 与 Linux 通知图标使用）。
#[cfg(any(windows, target_os = "linux"))]
const ICON_BYTES: &[u8] = include_bytes!("../../../assets/logo/fluxdown_logo.png");

/// 用户选择的应用图标在通知里的形态（见 `app_icon`）。`tag` 是图标身份（`bolt` /
/// `custom-<revision>`）：同一个 tag 同一张图，文件名据此区分，Windows 通知图标缓存按路径失效。
#[derive(Clone, Debug)]
#[cfg_attr(
    not(all(feature = "desktop", any(windows, target_os = "linux"))),
    allow(
        dead_code,
        reason = "只有桌面构建的 Windows / Linux 才会构造并读取自选通知图标"
    )
)]
pub(crate) struct NotifierIcon {
    pub tag: String,
    pub png: Arc<[u8]>,
}

/// 图标文件名：默认 `notification_icon.png`，自选图标 `notification_icon-<tag>.png`。
#[cfg(any(windows, target_os = "linux", test))]
fn icon_file_name(tag: Option<&str>) -> String {
    tag.map_or_else(
        || "notification_icon.png".to_owned(),
        |tag| format!("notification_icon-{tag}.png"),
    )
}

/// `name` 是不是应当清理的旧通知图标文件（同族命名、但不是当前这份）。
#[cfg(any(windows, target_os = "linux", test))]
fn is_stale_icon_file(name: &str, keep: &str) -> bool {
    name != keep && name.starts_with("notification_icon") && name.ends_with(".png")
}

/// Windows toast 的 AppUserModelID。刻意不与 Flutter 版（`Com.FluxDown.App`，注册表键
/// 不区分大小写）共用：那个键上挂着 Flutter 的 COM 激活器，共用会让点击本通知去拉起
/// Flutter 应用。
#[cfg(windows)]
const WINDOWS_AUMID: &str = "dev.zerx.fluxdown";

/// Linux 打包安装的桌面入口 id（`packaging/linux/com.fluxdown.app.desktop`，不含后缀）。
#[cfg(target_os = "linux")]
const LINUX_DESKTOP_ENTRY: &str = "com.fluxdown.app";

/// 发送系统通知；平台应用身份在首次发送时准备，应用图标变化后按新图标重新准备。
pub struct Notifier {
    #[cfg(any(windows, target_os = "linux"))]
    data_dir: PathBuf,
    #[cfg(any(windows, target_os = "linux"))]
    icon_state: Mutex<IconState>,
}

/// 当前应用图标与按它准备好的平台身份。
#[cfg(any(windows, target_os = "linux"))]
#[derive(Default)]
struct IconState {
    icon: Option<NotifierIcon>,
    /// `(图标 tag, 已准备的身份)`；tag 与当前图标一致才可复用。
    prepared: Option<(Option<String>, Prepared)>,
}

/// 首次发送前准备好的平台身份。
#[cfg(any(windows, target_os = "linux"))]
#[derive(Clone)]
struct Prepared {
    #[cfg(windows)]
    identity: Result<(), String>,
    /// 已落盘的通知图标（写入失败为 `None`，此时退回主题图标名）。
    #[cfg(target_os = "linux")]
    icon: Option<PathBuf>,
}

impl Notifier {
    /// `data_dir`：Windows / Linux 的通知身份资源与操作令牌目录。
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = data_dir;
        Self {
            #[cfg(any(windows, target_os = "linux"))]
            data_dir,
            #[cfg(any(windows, target_os = "linux"))]
            icon_state: Mutex::new(IconState::default()),
        }
    }

    #[cfg(feature = "desktop")]
    /// 切换通知使用的应用图标（`None` = 内置默认图）。只记录选择：文件与 Windows 注册表
    /// 在下一次发送通知时才按需写入，没有通知就没有任何 IO。
    pub(crate) fn set_icon(&self, icon: Option<NotifierIcon>) {
        #[cfg(any(windows, target_os = "linux"))]
        {
            let mut state = self
                .icon_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.icon = icon;
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = icon;
    }

    /// 按当前图标准备（或复用）平台身份。
    #[cfg(any(windows, target_os = "linux"))]
    fn prepared(&self) -> Prepared {
        let mut state = self
            .icon_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tag = state.icon.as_ref().map(|icon| icon.tag.clone());
        if let Some((prepared_tag, prepared)) = &state.prepared
            && *prepared_tag == tag
        {
            return prepared.clone();
        }
        let prepared = prepare(&self.data_dir, state.icon.as_ref());
        state.prepared = Some((tag, prepared.clone()));
        prepared
    }

    /// 阻塞发送一条通知；失败只记日志（通知是尽力而为的旁路效果）。
    pub fn show(&self, title: &str, body: &str) {
        if let Err(error) = self.try_show(title, body) {
            tracing::warn!(error = %error, "could not show system notification");
        }
    }

    /// 阻塞发送测试或信息通知；成功表示系统接收请求，不保证横幅可见。
    pub fn try_show(&self, title: &str, body: &str) -> Result<(), String> {
        self.send(title, body, None, false)
    }

    pub(crate) fn show_completion(
        &self,
        title: &str,
        body: &str,
        path: Option<PathBuf>,
        open_file_label: String,
        open_folder_label: String,
        silent: bool,
    ) {
        let actions = path.map(|path| CompletionActions {
            path,
            open_file_label,
            open_folder_label,
        });
        if let Err(error) = self.send(title, body, actions.as_ref(), silent) {
            tracing::warn!(%error, "could not show completion notification");
        }
    }

    /// 阻塞发送一条下载失败通知；`silent` 为真时抑制系统提示音（失败提示音由 agent 自己播放）。
    pub(crate) fn show_failure(&self, title: &str, body: &str, silent: bool) {
        if let Err(error) = self.send(title, body, None, silent) {
            tracing::warn!(%error, "could not show failure notification");
        }
    }

    #[cfg(target_os = "macos")]
    fn send(
        &self,
        title: &str,
        body: &str,
        actions: Option<&CompletionActions>,
        // 通知中心的 content 默认不带声音：macOS 天然静音，无需额外处理。
        _silent: bool,
    ) -> Result<(), String> {
        macos::show(title, body, actions)
    }

    #[cfg(windows)]
    fn send(
        &self,
        title: &str,
        body: &str,
        actions: Option<&CompletionActions>,
        silent: bool,
    ) -> Result<(), String> {
        let prepared = self.prepared();
        prepared.identity.as_ref().map_err(Clone::clone)?;
        windows::show(&self.data_dir, title, body, actions, silent)
    }

    #[cfg(target_os = "linux")]
    fn send(
        &self,
        title: &str,
        body: &str,
        _actions: Option<&CompletionActions>,
        silent: bool,
    ) -> Result<(), String> {
        let prepared = self.prepared();
        let mut notification = notify_rust::Notification::new();
        notification.appname(APP_NAME).summary(title).body(body);
        if silent {
            notification.hint(notify_rust::Hint::SuppressSound(true));
        }
        apply_platform_identity(&mut notification, &prepared);
        notification
            .show()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Android / iOS：通知由宿主应用经平台 API 发送（前台服务通知 / UserNotifications），
    /// agent 进程内没有可用的系统通知后端。
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    fn send(
        &self,
        _title: &str,
        _body: &str,
        _actions: Option<&CompletionActions>,
        _silent: bool,
    ) -> Result<(), String> {
        Err("system notifications are managed by the host application on this platform".to_owned())
    }
}

/// 系统通知能否送达（只读探测，不发送通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationAvailability {
    /// 系统会显示本应用的通知；附带探测到的细节（如通知服务名）。
    Available(String),
    /// 用户在系统设置里关闭了通知。
    Blocked(String),
    /// 系统没有可用的通知服务（如 Linux 会话里没有通知守护进程）。
    Unavailable(String),
    /// 系统尚未获得授权决定，或没有可识别的授权状态。
    Unverifiable,
}

/// 读取系统级通知开关（阻塞；调用方放进 `spawn_blocking`）。
#[cfg(windows)]
#[must_use]
pub fn availability() -> NotificationAvailability {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    let read_dword = |path: &str, name: &str| {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(path, KEY_READ)
            .and_then(|key| key.get_value::<u32, _>(name))
            .ok()
    };
    if read_dword(
        "Software\\Microsoft\\Windows\\CurrentVersion\\PushNotifications",
        "ToastEnabled",
    ) == Some(0)
    {
        return NotificationAvailability::Blocked(
            "notifications from all apps are turned off in Windows Settings".to_owned(),
        );
    }
    if read_dword(
        &format!(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Notifications\\Settings\\{WINDOWS_AUMID}"
        ),
        "Enabled",
    ) == Some(0)
    {
        return NotificationAvailability::Blocked(
            "FluxDown notifications are turned off in Windows Settings".to_owned(),
        );
    }
    NotificationAvailability::Available(format!("toast sender {WINDOWS_AUMID}"))
}

/// 询问会话总线上的通知服务（阻塞；调用方放进 `spawn_blocking`）。
#[cfg(target_os = "linux")]
#[must_use]
pub fn availability() -> NotificationAvailability {
    match notify_rust::get_server_information() {
        Ok(info) => NotificationAvailability::Available(format!(
            "{} {} ({})",
            info.name, info.version, info.vendor
        )),
        Err(error) => NotificationAvailability::Unavailable(error.to_string()),
    }
}

/// 移动平台的通知授权由宿主应用管理，agent 无法探测。
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
#[must_use]
pub fn availability() -> NotificationAvailability {
    NotificationAvailability::Unverifiable
}

/// 只读查询 FluxDown helper 的 macOS 通知授权，不触发权限弹窗。
#[cfg(target_os = "macos")]
#[must_use]
pub fn availability() -> NotificationAvailability {
    macos::availability()
}

#[cfg(windows)]
fn prepare(data_dir: &Path, icon: Option<&NotifierIcon>) -> Prepared {
    Prepared {
        identity: write_icon(data_dir, icon)
            .ok_or_else(|| "could not prepare FluxDown notification icon".to_owned())
            .and_then(|icon| {
                register_windows_aumid(Some(&icon)).map_err(|error| {
                    format!("could not register notification AppUserModelID: {error}")
                })
            }),
    }
}

#[cfg(target_os = "linux")]
fn prepare(data_dir: &Path, icon: Option<&NotifierIcon>) -> Prepared {
    Prepared {
        icon: write_icon(data_dir, icon),
    }
}

#[cfg(target_os = "linux")]
fn apply_platform_identity(notification: &mut notify_rust::Notification, prepared: &Prepared) {
    let icon = prepared.icon.as_deref().map_or_else(
        || LINUX_DESKTOP_ENTRY.to_owned(),
        |path| path.display().to_string(),
    );
    notification
        .icon(&icon)
        .hint(notify_rust::Hint::DesktopEntry(
            LINUX_DESKTOP_ENTRY.to_owned(),
        ));
}

/// 把通知图标写到数据目录（内容相同则不重写），并清理旧图标文件；失败返回 `None`。
/// 默认用内置图，自选图标用其 PNG 与带 tag 的文件名。
#[cfg(any(windows, target_os = "linux"))]
fn write_icon(data_dir: &Path, icon: Option<&NotifierIcon>) -> Option<PathBuf> {
    let name = icon_file_name(icon.map(|icon| icon.tag.as_str()));
    let bytes: &[u8] = icon.map_or(ICON_BYTES, |icon| &icon.png);
    let path = data_dir.join(&name);
    if !std::fs::read(&path).is_ok_and(|existing| existing == bytes)
        && let Err(error) =
            std::fs::create_dir_all(data_dir).and_then(|()| std::fs::write(&path, bytes))
    {
        tracing::warn!(path = %path.display(), error = %error, "could not write notification icon");
        return None;
    }
    remove_stale_icons(data_dir, &name);
    Some(path)
}

/// 删除数据目录里不再使用的旧通知图标；失败只记调试日志。
#[cfg(any(windows, target_os = "linux"))]
fn remove_stale_icons(data_dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| is_stale_icon_file(name, keep));
        if stale && let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::debug!(path = %entry.path().display(), %error, "could not remove stale notification icon");
        }
    }
}

/// 登记 toast 的显示名与图标（每次启动覆盖一次：图标路径随数据目录变化而更新）。
#[cfg(windows)]
fn register_windows_aumid(icon: Option<&Path>) -> std::io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey_with_flags(
        format!("Software\\Classes\\AppUserModelId\\{WINDOWS_AUMID}"),
        KEY_WRITE,
    )?;
    key.set_value("DisplayName", &APP_NAME)?;
    if let Some(icon) = icon {
        key.set_value("IconUri", &icon.display().to_string())?;
    }
    Ok(())
}

/// 下载完成通知文案：单个任务
/// 标题「下载完成」、正文文件名；多个任务标题「N 个任务下载完成」、正文「最后一个文件名
/// 等 N-1 个文件」。`text(key, count)` 按 en / zh 基线键取文案并替换 `{count}`。
pub fn completion_text(
    file_names: &[String],
    text: impl Fn(&str, Option<usize>) -> String,
) -> Option<(String, String)> {
    let last = file_names.last()?;
    if file_names.len() == 1 {
        return Some((text("downloadCompleted", None), last.clone()));
    }
    let count = file_names.len();
    Some((
        text("batchDownloadCompleted", Some(count)),
        format!("{last} {}", text("andMoreFiles", Some(count - 1))),
    ))
}

/// RSS 自动下载通知文案：标题「RSS 自动新建了 N 个下载任务」，
/// 正文为首个条目标题。
pub fn rss_auto_download_text(
    titles: &[String],
    text: impl Fn(&str, Option<usize>) -> String,
) -> Option<(String, String)> {
    let first = titles.first()?;
    Some((
        text("rssAutoDownloadedToast", Some(titles.len())),
        first.clone(),
    ))
}

/// 英文基线文案（headless 构建没有文案目录 / 目录加载失败时）；与 `assets/i18n/en.json` 同文。
#[must_use]
pub fn english_text(key: &str, count: Option<usize>) -> String {
    let template = match key {
        "downloadCompleted" => "Download Complete",
        "subtitleError" => "Download Failed",
        "batchDownloadFailed" => "{count} Downloads Failed",
        "openFile" => "Open File",
        "openFolder" => "Open Folder",
        "rssAutoDownloadedToast" => "RSS added {count} download(s)",
        "batchDownloadCompleted" => "{count} Downloads Complete",
        "andMoreFiles" => "and {count} more",
        "torrentFileAssociation" => "Associate .torrent Files",
        "magnetLinkAssociation" => "Take Over Magnet Links",
        "ed2kLinkAssociation" => "Associate ed2k Links",
        "associationOffIgnored" => {
            "This association is turned off in Settings, so FluxDown did not add a download."
        }
        "doctorTestNotificationTitle" => "FluxDown test notification",
        "doctorTestNotificationBody" => "If you can see this, download notifications work.",
        other => other,
    };
    count.map_or_else(
        || template.to_owned(),
        |count| template.replace("{count}", &count.to_string()),
    )
}

/// 按界面语言取通知文案：desktop 构建首次使用时加载 en / zh 基线目录；headless 构建或
/// 目录加载失败时回退 [`english_text`]。
#[derive(Default)]
pub struct NoticeText {
    #[cfg(feature = "desktop")]
    catalog: std::sync::OnceLock<Option<std::sync::Arc<fluxdown_ui_i18n::I18nCatalog>>>,
}

impl NoticeText {
    /// `locale`：界面语言偏好（`None` = 跟随系统）。
    #[cfg(feature = "desktop")]
    #[must_use]
    pub fn text(&self, key: &str, locale: Option<&str>) -> String {
        let catalog = self.catalog.get_or_init(|| {
            fluxdown_ui_i18n::I18nCatalog::load_embedded()
                .map(std::sync::Arc::new)
                .map_err(|error| {
                    tracing::warn!(error = %error, "notification translations unavailable");
                })
                .ok()
        });
        let Some(catalog) = catalog else {
            return english_text(key, None);
        };
        let locale = locale.map_or_else(fluxdown_ui_i18n::system_locale, str::to_owned);
        catalog.translator(&locale).text(key).to_owned()
    }

    /// headless 构建不带文案目录：固定英文。
    #[cfg(not(feature = "desktop"))]
    #[must_use]
    pub fn text(&self, key: &str, _locale: Option<&str>) -> String {
        english_text(key, None)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        completion_text, english_text as english, icon_file_name, is_stale_icon_file,
        rss_auto_download_text,
    };

    #[test]
    fn notification_app_icon_files_are_tagged_and_only_stale_ones_are_removed() {
        assert_eq!(icon_file_name(None), "notification_icon.png");
        assert_eq!(
            icon_file_name(Some("custom-0a1b")),
            "notification_icon-custom-0a1b.png"
        );

        let keep = icon_file_name(Some("bolt"));
        assert!(!is_stale_icon_file(&keep, &keep));
        assert!(is_stale_icon_file("notification_icon.png", &keep));
        assert!(is_stale_icon_file(
            "notification_icon-custom-0a1b.png",
            &keep
        ));
        // 数据目录里的其它文件（令牌、状态）永远不动。
        assert!(!is_stale_icon_file("agent-state.json", &keep));
        assert!(!is_stale_icon_file("notification_actions.json", &keep));
    }

    #[test]
    fn rss_auto_download_text_reports_count_and_first_title() {
        assert_eq!(rss_auto_download_text(&[], english), None);
        let titles = ["Ep 1".to_owned(), "Ep 2".to_owned()];
        assert_eq!(
            rss_auto_download_text(&titles, english),
            Some(("RSS added 2 download(s)".to_owned(), "Ep 1".to_owned()))
        );
    }

    #[test]
    fn single_and_batch_completion_text_match_flutter_rules() {
        assert_eq!(completion_text(&[], english), None);
        assert_eq!(
            completion_text(&["a.bin".to_owned()], english),
            Some(("Download Complete".to_owned(), "a.bin".to_owned()))
        );
        assert_eq!(
            completion_text(
                &["a.bin".to_owned(), "b.bin".to_owned(), "c.bin".to_owned()],
                english
            ),
            Some((
                "3 Downloads Complete".to_owned(),
                "c.bin and 2 more".to_owned()
            ))
        );
    }
}
