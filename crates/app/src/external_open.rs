//! 系统交来的链接 / `.torrent` 文件（冷启动参数、次实例转交、macOS 打开事件）的去向。
//!
//! 种子与磁力链先向 agent 询问打开策略（`agent.capture.openPolicy`）：来源关联已被用户
//! 关闭则忽略；「免打扰下载」开启则按原捕获入口直接建任务；否则交给新建下载窗口预览
//! 确认。其余链接沿用 `agent.capture.submit`。

use std::path::PathBuf;

use fluxdown_protocol::{
    ApplicationErrorCode, RpcErrorData,
    agent::{CaptureOpenPolicyDto, CaptureOpenPolicyParams},
    capture_link::{self, OpenAssociation},
    method,
};
use serde_json::{Value, json};

/// 一条系统交来的输入。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExternalOpen {
    /// 原始输入所属的可关闭关联；`fluxdown:` 深链解码出的磁力链不受 magnet 开关约束。
    pub association: Option<OpenAssociation>,
    pub source: Source,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// 普通链接（直链、ed2k 等）：不预览。
    Link {
        url: String,
        filename: String,
    },
    Magnet {
        url: String,
        filename: String,
    },
    TorrentFile(PathBuf),
}

/// 询问过打开策略之后对一条种子 / 磁力输入的处理。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// 来源关联已关闭：不建任务，不显示确认。
    Ignore,
    /// 交给新建下载窗口预览确认；携带窗口接受的输入（磁力链或本机种子路径）。
    Confirm(String),
    /// 按原捕获入口提交，由 agent 决定静默建任务或排入确认队列。
    Submit,
}

pub(crate) fn classify(urls: Vec<String>, files: Vec<PathBuf>) -> Vec<ExternalOpen> {
    let mut opens = Vec::with_capacity(urls.len() + files.len());
    for raw in urls {
        // 关联按原始 scheme 判定，须在深链解码之前。
        let association = OpenAssociation::of_url(&raw);
        let filename = capture_link::deep_link_file_name(&raw).unwrap_or_default();
        let url = capture_link::normalize_capture_url(&raw);
        let source = if capture_link::is_magnet_link(&url) {
            Source::Magnet { url, filename }
        } else {
            Source::Link { url, filename }
        };
        opens.push(ExternalOpen {
            association,
            source,
        });
    }
    for file in files {
        opens.push(ExternalOpen {
            association: Some(OpenAssociation::Torrent),
            source: Source::TorrentFile(file),
        });
    }
    opens
}

impl ExternalOpen {
    /// 用于失败日志的输入种类。
    pub(crate) fn kind(&self) -> &'static str {
        match self.source {
            Source::Link { .. } | Source::Magnet { .. } => "link",
            Source::TorrentFile(_) => "torrent file",
        }
    }

    /// 需要先问打开策略的输入（可预览确认的种子 / 磁力链）给出策略请求参数。
    pub(crate) fn policy_params(&self) -> Option<CaptureOpenPolicyParams> {
        match self.source {
            Source::Link { .. } => None,
            Source::Magnet { .. } | Source::TorrentFile(_) => Some(CaptureOpenPolicyParams {
                association: self.association,
            }),
        }
    }

    /// 原捕获入口的调用：`silent = true` 表示系统打开，agent 按「免打扰下载」偏好直接
    /// 建任务或排入确认队列，并自行拦截已关闭的关联。
    pub(crate) fn submit_call(&self) -> (&'static str, Value) {
        match &self.source {
            Source::Link { url, filename } | Source::Magnet { url, filename } => (
                method::AGENT_CAPTURE_SUBMIT,
                json!({
                    "request": { "url": url, "filename": filename },
                    "silent": true,
                    "association": self.association,
                }),
            ),
            Source::TorrentFile(path) => (
                method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE,
                json!({
                    "path": path.display().to_string(),
                    "silent": true,
                    "association": self.association,
                }),
            ),
        }
    }

    fn confirmation_input(&self) -> Option<String> {
        match &self.source {
            Source::Link { .. } => None,
            Source::Magnet { url, .. } => Some(url.clone()),
            Source::TorrentFile(path) => Some(path.to_string_lossy().into_owned()),
        }
    }

    /// 由策略应答决定去向。agent 不认识该方法（比桌面旧的常驻 agent）时回到原捕获入口，
    /// 行为与引入预览确认之前一致；其他错误原样上报。
    pub(crate) fn decide(
        &self,
        policy: Result<CaptureOpenPolicyDto, RpcErrorData>,
    ) -> Result<Decision, RpcErrorData> {
        match policy {
            Ok(policy) if policy.ignored => Ok(Decision::Ignore),
            Ok(policy) if policy.silent_download => Ok(Decision::Submit),
            Ok(_) => Ok(self
                .confirmation_input()
                .map_or(Decision::Submit, Decision::Confirm)),
            Err(error) if error.code == ApplicationErrorCode::Unsupported => Ok(Decision::Submit),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, Source, classify};
    use fluxdown_protocol::{
        ApplicationErrorCode, RpcErrorData, agent::CaptureOpenPolicyDto,
        capture_link::OpenAssociation, method,
    };
    use std::path::PathBuf;

    fn policy(ignored: bool, silent_download: bool) -> Result<CaptureOpenPolicyDto, RpcErrorData> {
        Ok(CaptureOpenPolicyDto {
            ignored,
            silent_download,
        })
    }

    #[test]
    fn torrents_and_magnets_ask_for_the_open_policy_and_other_links_do_not() {
        let path = PathBuf::from("C:\\下载 2026\\100%20 完整😀.torrent");
        let opens = classify(
            vec![
                "MAGNET:?xt=urn:btih:abc".into(),
                "fluxdown:magnet:?xt=urn:btih:abc".into(),
                "https://example.com/archive.torrent".into(),
                "ed2k://file/test".into(),
                "magnet:xt=urn:btih:abc".into(),
            ],
            vec![path.clone()],
        );
        assert_eq!(opens[0].association, Some(OpenAssociation::Magnet));
        assert!(opens[0].policy_params().is_some());
        // 深链解码出的磁力链同样预览确认，但不受 magnet 关联开关约束。
        assert_eq!(opens[1].association, None);
        assert_eq!(
            opens[1].source,
            Source::Magnet {
                url: "magnet:?xt=urn:btih:abc".into(),
                filename: String::new(),
            }
        );
        assert_eq!(opens[1].policy_params().expect("policy").association, None);
        // HTTP `.torrent` 直链、ed2k 与引擎不认的 `magnet:` 写法都不走预览。
        for open in &opens[2..5] {
            assert!(matches!(open.source, Source::Link { .. }));
            assert!(open.policy_params().is_none());
        }
        assert_eq!(opens[3].association, Some(OpenAssociation::Ed2k));
        assert_eq!(opens[5].association, Some(OpenAssociation::Torrent));
        assert_eq!(opens[5].source, Source::TorrentFile(path));
        assert_eq!(opens[5].kind(), "torrent file");
    }

    #[test]
    fn open_policy_selects_ignore_confirmation_or_the_capture_entry() {
        let magnet = "magnet:?xt=urn:btih:abc";
        let path = PathBuf::from("C:\\下载\\100%20 资料.torrent");
        let opens = classify(vec![magnet.into()], vec![path.clone()]);
        for open in &opens {
            assert_eq!(open.decide(policy(true, false)), Ok(Decision::Ignore));
            assert_eq!(
                open.decide(policy(true, true)),
                Ok(Decision::Ignore),
                "a disabled association wins over silent download"
            );
            assert_eq!(open.decide(policy(false, true)), Ok(Decision::Submit));
        }
        assert_eq!(
            opens[0].decide(policy(false, false)),
            Ok(Decision::Confirm(magnet.into()))
        );
        assert_eq!(
            opens[1].decide(policy(false, false)),
            Ok(Decision::Confirm(path.to_string_lossy().into_owned()))
        );
    }

    #[test]
    fn agents_without_the_policy_method_keep_the_capture_entry_and_other_errors_surface() {
        let opens = classify(vec!["magnet:?xt=urn:btih:abc".into()], Vec::new());
        assert_eq!(
            opens[0].decide(Err(RpcErrorData::new(
                ApplicationErrorCode::Unsupported,
                false
            ))),
            Ok(Decision::Submit)
        );
        let unavailable = RpcErrorData::new(ApplicationErrorCode::Unavailable, true);
        assert_eq!(opens[0].decide(Err(unavailable.clone())), Err(unavailable));
    }

    #[test]
    fn capture_entry_calls_declare_the_system_open_and_use_host_defaults() {
        let magnet = "MAGNET:?xt=urn:btih:abc";
        let path = PathBuf::from("C:\\下载\\100%20 资料.torrent");
        let opens = classify(
            vec![magnet.into(), "https://example.com/a.zip".into()],
            vec![path.clone()],
        );
        let (link_method, link) = opens[0].submit_call();
        assert_eq!(link_method, method::AGENT_CAPTURE_SUBMIT);
        assert_eq!(link["request"]["url"], magnet);
        assert_eq!(link["association"], "magnet");
        assert_eq!(link["silent"], true);
        let (plain_method, plain) = opens[1].submit_call();
        assert_eq!(plain_method, method::AGENT_CAPTURE_SUBMIT);
        assert_eq!(plain["request"]["url"], "https://example.com/a.zip");
        assert_eq!(plain["association"], serde_json::Value::Null);
        let (file_method, file) = opens[2].submit_call();
        assert_eq!(file_method, method::AGENT_CAPTURE_SUBMIT_TORRENT_FILE);
        assert_eq!(file["path"].as_str(), path.to_str());
        assert_eq!(file["silent"], true);
        assert_eq!(file["association"], "torrent");
        for key in ["saveDir", "queueId", "startPaused", "customName"] {
            assert!(file.get(key).is_none(), "{key} must use host defaults");
        }
    }
}
