//! 应用内更新状态 → 文案键与可用操作的纯判定。
//!
//! 设置「关于」页与 app 层的更新通知共用，保证两处对同一份 `UpdateStatusDto` 给出一致的
//! 状态文本与按钮；agent 是状态唯一来源，这里不保存任何状态。改动须同步镜像
//! `web/src/lib/update.ts` 与原生 Android `mobile/Android/core/.../update/UpdateView.kt`。

use fluxdown_protocol::{
    UpdateFailure, UpdateInstallKind, UpdateManualReason, UpdatePhase, UpdateStatusDto,
};
use fluxdown_ui_i18n::Translator;

/// 一条待插值的文案：键 + `{name}` 参数 + 可选尾注。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateLine {
    pub key: &'static str,
    pub vars: Vec<(&'static str, String)>,
    pub suffix: Option<String>,
}

impl UpdateLine {
    fn plain(key: &'static str) -> Self {
        Self {
            key,
            vars: Vec::new(),
            suffix: None,
        }
    }

    fn with_version(key: &'static str, version: &str) -> Self {
        Self {
            key,
            vars: vec![("v", version.to_owned())],
            suffix: None,
        }
    }

    #[must_use]
    pub fn text(&self, translator: &Translator) -> String {
        let vars: Vec<(&str, &str)> = self
            .vars
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let mut text = translator.text_with(self.key, &vars);
        if let Some(suffix) = &self.suffix {
            text.push_str(suffix);
        }
        text
    }
}

/// 手动升级原因 → 说明文案键（托管包按安装形态区分）。
#[must_use]
pub fn manual_reason_key(reason: UpdateManualReason, kind: UpdateInstallKind) -> &'static str {
    match reason {
        UpdateManualReason::ManagedPackage => match kind {
            UpdateInstallKind::Docker => "updateManualDocker",
            UpdateInstallKind::Synology => "updateManualSynology",
            UpdateInstallKind::Qnap => "updateManualQnap",
            UpdateInstallKind::Openwrt => "updateManualOpenwrt",
            UpdateInstallKind::Fnos => "updateManualFnos",
            // 归 AUR / 发行版仓库等非官方系统包所有：官方 deb / Arch 包永远可应用内更新。
            UpdateInstallKind::LinuxDeb | UpdateInstallKind::LinuxArch => {
                "updateManualSystemPackage"
            }
            _ => "updateManualUnsupported",
        },
        UpdateManualReason::NotWritable => "updateManualNotWritable",
        UpdateManualReason::NoAsset => "updateManualNoAsset",
        UpdateManualReason::ElevationUnavailable => "updateManualElevation",
        UpdateManualReason::ReadOnlyLocation => "updateManualReadOnly",
        UpdateManualReason::UnofficialBuild => "updateManualUnofficial",
        UpdateManualReason::Unsupported | UpdateManualReason::Unknown => "updateManualUnsupported",
    }
}

/// 失败分类 → 文案键；缺省按未知失败。
#[must_use]
pub fn failure_key(failure: Option<UpdateFailure>, kind: UpdateInstallKind) -> &'static str {
    match failure {
        Some(UpdateFailure::Network) => "updateFailedNetwork",
        Some(UpdateFailure::Verify) => "updateFailedVerify",
        Some(UpdateFailure::Storage) => "updateFailedStorage",
        Some(UpdateFailure::Install) if kind == UpdateInstallKind::WindowsPortable => {
            "updateFailedPortableInstall"
        }
        Some(UpdateFailure::Install) => "updateFailedInstall",
        Some(UpdateFailure::ElevationCancelled) => "updateFailedElevationCancelled",
        Some(UpdateFailure::InstallIncomplete) => "updateFailedIncomplete",
        Some(UpdateFailure::Unknown) | None => "updateFailedUnknown",
    }
}

pub(crate) fn portable_recovery(status: &UpdateStatusDto) -> bool {
    status.phase == UpdatePhase::Failed
        && status.install_kind == UpdateInstallKind::WindowsPortable
        && status.failure == Some(UpdateFailure::Install)
        && status.manual_reason == Some(UpdateManualReason::Unsupported)
}

/// 下载进度百分比（0–100）；大小未知时为 0。
#[must_use]
pub fn download_percent(status: &UpdateStatusDto) -> u8 {
    if status.asset_size == 0 {
        return 0;
    }
    let percent = u128::from(status.downloaded_bytes.min(status.asset_size)) * 100
        / u128::from(status.asset_size);
    u8::try_from(percent).unwrap_or(100)
}

/// 当前阶段的状态行；尚未检查（`Idle`）或无信息可说时为 `None`。
#[must_use]
pub fn status_line(status: &UpdateStatusDto) -> Option<UpdateLine> {
    let latest = status.latest_version.as_str();
    match status.phase {
        UpdatePhase::Idle => None,
        UpdatePhase::Checking => Some(UpdateLine::plain("checking")),
        UpdatePhase::UpToDate => {
            let mut line = UpdateLine::plain("upToDate");
            if !latest.is_empty() {
                line.suffix = Some(format!(" (v{latest})"));
            }
            Some(line)
        }
        UpdatePhase::Available => status
            .has_update
            .then(|| UpdateLine::with_version("newVersionFound", latest)),
        UpdatePhase::Downloading => Some(UpdateLine {
            key: "updateDownloadingProgress",
            vars: vec![
                ("v", latest.to_owned()),
                ("percent", download_percent(status).to_string()),
            ],
            suffix: None,
        }),
        UpdatePhase::Ready => Some(UpdateLine::with_version("updateReadyToast", latest)),
        UpdatePhase::Installing => Some(UpdateLine::plain("updateInstalling")),
        UpdatePhase::Failed => Some(UpdateLine::plain(failure_key(
            status.failure,
            status.install_kind,
        ))),
    }
}

/// 不可一键更新时的手动升级说明（仅在确有新版本时）。
#[must_use]
pub fn manual_line(status: &UpdateStatusDto) -> Option<UpdateLine> {
    if portable_recovery(status) {
        return Some(UpdateLine::plain("updatePortableRecoveryHint"));
    }
    if !status.has_update {
        return None;
    }
    status
        .manual_reason
        .map(|reason| UpdateLine::plain(manual_reason_key(reason, status.install_kind)))
}

/// 是否展示「更新并重启」：有新版本、可一键更新，且当前阶段允许发起安装。
/// 下载中但用户已点过安装（`install_pending`）时不再重复展示。
#[must_use]
pub fn can_install(status: &UpdateStatusDto) -> bool {
    status.has_update
        && status.manual_reason.is_none()
        && match status.phase {
            UpdatePhase::Available | UpdatePhase::Ready | UpdatePhase::Failed => true,
            UpdatePhase::Downloading => !status.install_pending,
            _ => false,
        }
}

/// 下载中可取消。
#[must_use]
pub fn can_cancel(status: &UpdateStatusDto) -> bool {
    status.phase == UpdatePhase::Downloading
}

/// 手动升级的打开地址：优先资产直链，其次发布页；无新版本或非手动时为 `None`。
#[must_use]
pub fn manual_url(status: &UpdateStatusDto) -> Option<&str> {
    if !status.has_update || status.manual_reason.is_none() {
        return None;
    }
    [
        status.download_url.as_str(),
        status.release_page_url.as_str(),
    ]
    .into_iter()
    .find(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(phase: UpdatePhase) -> UpdateStatusDto {
        UpdateStatusDto {
            phase,
            latest_version: "2.0.0".to_owned(),
            has_update: true,
            ..UpdateStatusDto::default()
        }
    }

    #[test]
    fn percent_is_clamped_and_safe_on_unknown_size() {
        let mut dto = status(UpdatePhase::Downloading);
        assert_eq!(download_percent(&dto), 0);
        dto.asset_size = 200;
        dto.downloaded_bytes = 50;
        assert_eq!(download_percent(&dto), 25);
        dto.downloaded_bytes = 900;
        assert_eq!(download_percent(&dto), 100);
    }

    #[test]
    fn install_offered_only_when_auto_installable() {
        for phase in [
            UpdatePhase::Available,
            UpdatePhase::Ready,
            UpdatePhase::Failed,
        ] {
            assert!(can_install(&status(phase)), "{phase:?}");
        }
        let mut downloading = status(UpdatePhase::Downloading);
        assert!(can_install(&downloading));
        assert!(can_cancel(&downloading));
        downloading.install_pending = true;
        assert!(!can_install(&downloading));
        for phase in [
            UpdatePhase::Idle,
            UpdatePhase::Checking,
            UpdatePhase::UpToDate,
            UpdatePhase::Installing,
        ] {
            assert!(!can_install(&status(phase)), "{phase:?}");
        }
        let mut failed = status(UpdatePhase::Failed);
        failed.has_update = false;
        assert!(!can_install(&failed));
        let mut manual = status(UpdatePhase::Available);
        manual.manual_reason = Some(UpdateManualReason::NoAsset);
        assert!(!can_install(&manual));
    }

    #[test]
    fn manual_url_prefers_asset_link() {
        let mut manual = status(UpdatePhase::Available);
        manual.manual_reason = Some(UpdateManualReason::NotWritable);
        manual.release_page_url = "https://r".to_owned();
        assert_eq!(manual_url(&manual), Some("https://r"));
        manual.download_url = "https://d".to_owned();
        assert_eq!(manual_url(&manual), Some("https://d"));
        manual.manual_reason = None;
        assert_eq!(manual_url(&manual), None);
    }

    #[test]
    fn reason_keys_follow_install_kind_for_managed_packages() {
        assert_eq!(
            manual_reason_key(UpdateManualReason::ManagedPackage, UpdateInstallKind::Qnap),
            "updateManualQnap"
        );
        assert_eq!(
            manual_reason_key(
                UpdateManualReason::ManagedPackage,
                UpdateInstallKind::LinuxArch
            ),
            "updateManualSystemPackage"
        );
        assert_eq!(
            manual_reason_key(
                UpdateManualReason::ManagedPackage,
                UpdateInstallKind::LinuxDeb
            ),
            "updateManualSystemPackage"
        );
        assert_eq!(
            manual_reason_key(UpdateManualReason::Unknown, UpdateInstallKind::Docker),
            "updateManualUnsupported"
        );
        assert_eq!(
            failure_key(None, UpdateInstallKind::Docker),
            "updateFailedUnknown"
        );
    }

    #[test]
    fn status_lines_follow_phase() {
        assert_eq!(status_line(&status(UpdatePhase::Idle)), None);
        assert_eq!(
            status_line(&status(UpdatePhase::Checking)).map(|l| l.key),
            Some("checking")
        );
        let mut up = status(UpdatePhase::UpToDate);
        up.has_update = false;
        assert_eq!(
            status_line(&up).and_then(|l| l.suffix),
            Some(" (v2.0.0)".to_owned())
        );
        let mut failed = status(UpdatePhase::Failed);
        failed.failure = Some(UpdateFailure::Verify);
        assert_eq!(
            status_line(&failed).map(|l| l.key),
            Some("updateFailedVerify")
        );
        assert_eq!(
            manual_line(&status(UpdatePhase::Available)),
            None,
            "auto-installable shows no manual note"
        );
    }
}
