//! 桌面进程启动参数、单实例锁与外部链接接入。
//!
//! 开机自启以 `--minimized` 拉起；系统把 `magnet:` / `ed2k:` / `fluxdown:` 链接或
//! 直链交给本进程时，主实例统一处理，后续实例经本机激活通道转交后退出。
//! 种子 / 磁力链先经 agent 检查来源关联，再进入新建下载确认；其他链接沿用捕获路径。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fluxdown_protocol::{AgentSnapshot, capture_link};

/// 已解析的命令行。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaunchOptions {
    /// 启动后最小化主窗口（自启动场景）。
    pub minimized: bool,
    /// 由 agent 为待确认的捕获 / 选择请求拉起：不开主窗口，只开确认窗口。
    pub capture_only: bool,
    /// 直接打开或聚焦设置窗口，不打开主窗口。
    pub settings: bool,
    /// 直接打开新建下载窗口（agent 全局快捷键「新建下载」拉起；已有主实例时转交它处理）。
    pub new_download: bool,
    /// 仅唤起已有 UI；绝不新建 UI 或启动本机后台服务。
    pub activate_existing: bool,
    /// 需要交给 agent 的外部链接。
    pub urls: Vec<String>,
    /// 需要经 agent 上传后建任务的本机 `.torrent` 文件。
    pub torrent_files: Vec<PathBuf>,
    /// agent 静默建成单个任务后拉起界面时携带：该任务按用户开始处理（弹进度窗口）。
    pub progress_task: Option<String>,
    /// 应用内更新完成后由 agent / 安装器拉起：旧桌面进程仍在退出，需等其释放单实例锁，
    /// 且就绪后提示已更新（不影响「普通启动」判定）。
    pub after_update: bool,
    /// 托盘「重启」由 agent 拉起：与 `--after-update` 一样要等旧实例释放单实例锁，但没有
    /// 更新发生，不提示已更新（同样不影响「普通启动」判定）。
    pub relaunch: bool,
}

impl LaunchOptions {
    #[must_use]
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--minimized" | "--start-minimized" => options.minimized = true,
                "--capture" => options.capture_only = true,
                "--settings" => options.settings = true,
                "--new-download" => options.new_download = true,
                "--activate-existing" => options.activate_existing = true,
                "--after-update" => options.after_update = true,
                "--relaunch" => options.relaunch = true,
                "--progress-task" => {
                    options.progress_task = args.next().filter(|task_id| !task_id.is_empty());
                }
                value if capture_link::is_capture_url(value) => options.urls.push(value.to_owned()),
                value if value.starts_with("--") => {}
                value => {
                    if let Some(path) = torrent_path(value) {
                        options.torrent_files.push(path);
                    }
                }
            }
        }
        options
    }

    /// 普通启动：没有外部链接 / 种子文件，也不是自启 / 确认 / 唤起模式。
    #[must_use]
    pub fn is_plain(&self) -> bool {
        !self.minimized
            && !self.capture_only
            && !self.settings
            && !self.new_download
            && !self.activate_existing
            && self.urls.is_empty()
            && self.torrent_files.is_empty()
    }

    /// 由 agent 拉起的确认 / 进度界面。agent 只在没有 UI 连接时这样拉起，此时仍持有单实例锁的
    /// 旧实例多半正在退出：应等它释放锁后接替，而不是把请求转发给不会再处理的旧实例。
    #[must_use]
    pub fn agent_initiated(&self) -> bool {
        self.capture_only || self.progress_task.is_some()
    }
}

/// 「启动时最小化到托盘」偏好键（agent 偏好，与 agent 自启判定同一键）。
const START_MINIMIZED_TO_TRAY_KEY: &str = "start_minimized_to_tray";

/// 本进程冷启动了 FluxDown 服务时，是否只留托盘而不开主窗口：偏好开启且托盘确实可见
/// （可用且驻留），否则隐藏主窗口会留下既无窗口也无托盘的状态。
#[must_use]
pub fn start_in_tray(snapshot: &AgentSnapshot) -> bool {
    snapshot.shell.tray_available
        && snapshot.shell.resident
        && snapshot
            .preferences
            .values
            .get(START_MINIMIZED_TO_TRAY_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

/// `.torrent` 路径或 `file://` URL → 已存在的本机文件路径。
#[must_use]
pub fn torrent_path(value: &str) -> Option<PathBuf> {
    capture_link::torrent_file_path(value).filter(|path| path.is_file())
}

/// 单实例锁：持有期间文件锁不释放；第二个进程 `try_acquire` 失败。
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// 尝试成为主实例。`None` 表示已有实例在运行。
    pub fn try_acquire(dir: &Path) -> Result<Option<Self>, std::io::Error> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("desktop.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

/// 锁与队列文件所在目录：与 agent 数据目录同级。
#[must_use]
pub fn instance_dir(agent_token_path: &Path) -> PathBuf {
    agent_token_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::temp_dir().join("fluxdown"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_urls() {
        let options = LaunchOptions::from_args(
            [
                "--minimized",
                "--activate-existing",
                "magnet:?xt=urn:btih:abc",
                "/tmp/x.torrent",
                "--foo",
            ]
            .map(str::to_owned),
        );
        assert!(options.minimized);
        assert!(options.activate_existing);
        assert_eq!(options.urls, vec!["magnet:?xt=urn:btih:abc"]);
        let options =
            LaunchOptions::from_args(["--capture", "--progress-task", "task-1"].map(str::to_owned));
        assert!(options.capture_only);
        assert_eq!(options.progress_task.as_deref(), Some("task-1"));
        assert!(options.torrent_files.is_empty());
        let options = LaunchOptions::from_args(["--after-update"].map(str::to_owned));
        assert!(options.after_update);
        assert!(options.is_plain(), "after-update stays a plain launch");
        let options = LaunchOptions::from_args(["--relaunch"].map(str::to_owned));
        assert!(options.relaunch);
        assert!(!options.after_update, "relaunch must not claim an update");
        assert!(options.is_plain(), "relaunch stays a plain launch");
        let file = std::env::temp_dir().join(format!("fluxdown-{}.torrent", std::process::id()));
        std::fs::write(&file, b"d8:announce0:e").expect("write");
        let options = LaunchOptions::from_args([file.display().to_string()]);
        assert_eq!(options.torrent_files, vec![file.clone()]);
        assert_eq!(
            torrent_path(&format!("file://{}", file.display())),
            Some(file.clone())
        );
        if let Err(error) = std::fs::remove_file(file) {
            log::warn!("could not remove torrent argument fixture: {error}");
        }
    }

    #[test]
    fn start_in_tray_requires_preference_and_visible_tray() {
        let snapshot = |pref: Option<bool>, available: bool, resident: bool| {
            let mut snapshot = AgentSnapshot::default();
            if let Some(pref) = pref {
                snapshot
                    .preferences
                    .values
                    .insert(START_MINIMIZED_TO_TRAY_KEY.to_owned(), pref.into());
            }
            snapshot.shell.tray_available = available;
            snapshot.shell.resident = resident;
            snapshot
        };
        assert!(start_in_tray(&snapshot(Some(true), true, true)));
        assert!(!start_in_tray(&snapshot(None, true, true)));
        assert!(!start_in_tray(&snapshot(Some(false), true, true)));
        // 托盘不可用或未驻留（关闭了「关闭时最小化到托盘」）：隐藏会让应用无处可见。
        assert!(!start_in_tray(&snapshot(Some(true), false, true)));
        assert!(!start_in_tray(&snapshot(Some(true), true, false)));
    }

    #[test]
    fn only_bare_launch_is_plain() {
        assert!(LaunchOptions::from_args(Vec::<String>::new()).is_plain());
        for arg in [
            "--minimized",
            "--capture",
            "--settings",
            "--new-download",
            "--activate-existing",
            "magnet:?xt=urn:btih:abc",
        ] {
            assert!(
                !LaunchOptions::from_args([arg.to_owned()]).is_plain(),
                "{arg}"
            );
        }
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = std::env::temp_dir().join(format!("fluxdown-lock-{}", std::process::id()));
        let first = InstanceLock::try_acquire(&dir).expect("lock");
        assert!(first.is_some());
        let second = InstanceLock::try_acquire(&dir).expect("lock");
        assert!(second.is_none());
        drop(first);
        let replacement = InstanceLock::try_acquire(&dir).expect("replacement lock");
        assert!(replacement.is_some());
        drop(replacement);
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove instance lock fixture: {error}");
        }
    }
}
