//! 不建任务的种子预览契约。文件下标一律沿用种子内的原始顺序。

use serde::{Deserialize, Serialize};

use crate::BtFileDto;

/// `daemon.torrent.prepareMagnet` 参数：在发起任何网络请求前预约一次可取消的磁力预览。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagnetPreviewParams {
    pub url: String,
}

/// 一次性的预览预约，须在两分钟内解析或取消。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MagnetPreviewTicket {
    pub preview_id: String,
}

/// `agent.capture.previewTorrentFile` 参数：由 agent 读取的本机文件，不建下载任务。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFilePreviewParams {
    pub path: String,
}

/// 已上传的种子，保留到确认、显式丢弃或 blob 过期为止。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentPreviewParams {
    pub torrent_blob_id: String,
}

/// `daemon.torrent.confirm` 参数：对一次预览的显式确认。文件下标用种子内的原始下标，
/// 空选择被拒绝。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TorrentConfirmParams {
    pub torrent_blob_id: String,
    pub save_dir: String,
    pub selected_file_indices: Vec<i32>,
    #[serde(default)]
    pub queue_id: String,
    #[serde(default)]
    pub start_paused: bool,
    /// 落盘名（下单前改名）：空 = 用种子内的原始名称。多文件种子改的是外层容器
    /// 目录名，单文件种子改的是文件本身的文件名；必须是单个路径组件，见
    /// [`validate_custom_name`]。空选择之外，改名不影响索引语义。
    #[serde(default)]
    pub custom_name: String,
    /// 用户看到的原始来源（磁力链 URL；`.torrent` 文件任务为空）。确认后建任务时
    /// 作为展示来源写进任务行，`origin_url` /「复制链接」/ URL 去重才与确认前一致；
    /// 不传的话任务只剩 `torrent-file://local` 哨兵。
    #[serde(default)]
    pub source_url: String,
    /// 草稿里的其余选项。与上面的显式确认字段冲突时以显式字段为准；
    /// 其中的种子 / 音频附件会被拒绝。
    #[serde(default)]
    pub options: Option<crate::CreateTaskRequest>,
}

/// 用户自填下载名不可落盘的原因。
///
/// 判据与引擎的文件名清洗（`fluxdown_engine::naming::sanitize_filename`）逐条对齐：
/// 凡是清洗会改动或删除的输入，这里**拒绝**而不是静默替换——用户手输的名字被悄悄
/// 改成别的字符串比报错更难接受。引擎不依赖本 crate（`native/engine` 只依赖
/// `fluxdown_logfile`），所以两张表是复制而非共用；`native/daemon` 有一个跨 crate
/// 的一致性测试守着「接受 ⇒ 清洗后不变」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomNameIssue {
    /// 只有空白（调用方一般把空名当「不改名」，这是全空白输入）。
    Empty,
    /// 路径分隔符、Windows 非法字符、控制符、双向控制符或不可见字符。
    IllegalCharacter(char),
    /// 以 `.` 或空格开头 / 结尾（Windows 与引擎清洗都会去掉，落盘名与输入不一致）。
    TrailingDotOrSpace,
    /// Windows 保留设备名（`CON`、`NUL.txt`、`CON .txt` 等）。
    ReservedName,
    /// 超过 [`MAX_CUSTOM_NAME_BYTES`]。
    TooLong,
}

/// 自填名的字节上限，与引擎清洗预算（含临时后缀 / dedup 后缀余量）一致。
pub const MAX_CUSTOM_NAME_BYTES: usize = 200;

/// Windows 保留设备名（大小写无关，只比扩展名前的 stem）。
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 引擎清洗会替换 / 删除、因此这里必须拒绝的字符。
///
/// 除 Windows 非法字符与控制符外还包含：双向控制符（`invoice\u{202E}fdp.exe` 会
/// 在界面上显示成 `invoiceexe.pdf`）、零宽 / BOM 字符（引擎直接删除，名字会凭空
/// 少字）、行 / 段分隔符（不是 `Cc`，但会渲染成换行）。
fn is_illegal_name_character(character: char) -> bool {
    matches!(
        character,
        '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
    ) || character.is_control()
        || matches!(
            character,
            '\u{061C}'
                | '\u{200B}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

impl CustomNameIssue {
    /// 面向 RPC 错误正文的英文说明（宿主按 `code` 本地化外壳提示）。
    pub fn message(self) -> String {
        match self {
            Self::Empty => "the download name is empty".to_owned(),
            Self::IllegalCharacter(character) => format!(
                "the download name contains a character that cannot be saved: {character:?}"
            ),
            Self::TrailingDotOrSpace => {
                "the download name cannot end with a dot or a space".to_owned()
            }
            Self::ReservedName => "the download name is a reserved Windows device name".to_owned(),
            Self::TooLong => format!("the download name exceeds {MAX_CUSTOM_NAME_BYTES} bytes"),
        }
    }
}

/// 校验用户自填的落盘名。`Ok(())` 表示这个名字可以原样成为单个路径组件。
///
/// 不做任何替换 / 截断：需要清洗的输入在这里一律报错，由调用方决定提示还是拒绝。
/// 反过来说，凡是本函数接受的名字，引擎的 `sanitize_filename` 都会原样保留。
///
/// ```
/// use fluxdown_protocol::torrent_preview::validate_custom_name;
/// assert!(validate_custom_name("我的电影.mkv").is_ok());
/// assert!(validate_custom_name("../escape").is_err());
/// assert!(validate_custom_name("name.").is_err());
/// ```
pub fn validate_custom_name(name: &str) -> Result<(), CustomNameIssue> {
    if name.trim().is_empty() {
        return Err(CustomNameIssue::Empty);
    }
    if let Some(character) = name
        .chars()
        .find(|character| is_illegal_name_character(*character))
    {
        return Err(CustomNameIssue::IllegalCharacter(character));
    }
    // 引擎清洗与 Windows 都会把名字首尾的点 / 空格归一化掉（`...a` → `a`），
    // 那样落盘名就与用户输入不一致了，所以两端都拒绝。
    if name.starts_with(['.', ' ']) || name.ends_with(['.', ' ']) {
        return Err(CustomNameIssue::TrailingDotOrSpace);
    }
    // Windows 连 stem 里的尾随空格也一起归一化（`CON .txt` 仍是 `CON` 设备），
    // 所以比较前把 stem 尾部的空格与点去掉。
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.']);
    if RESERVED_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(CustomNameIssue::ReservedName);
    }
    if name.len() > MAX_CUSTOM_NAME_BYTES {
        return Err(CustomNameIssue::TooLong);
    }
    Ok(())
}

/// 确认表单用的元数据；`torrent_blob_id` 指向同一份 blob，供随后确认时提交。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentPreviewDto {
    pub torrent_blob_id: String,
    pub name: String,
    pub total_bytes: i64,
    pub files: Vec<BtFileDto>,
}

#[cfg(test)]
mod tests {
    use super::{TorrentPreviewDto, TorrentPreviewParams};
    use crate::method;
    use serde_json::json;

    #[test]
    fn torrent_confirmation_draft_options_round_trip() {
        let wire = json!({"torrentBlobId": "torrent:preview", "saveDir": "/downloads",
            "selectedFileIndices": [2], "queueId": "queue", "startPaused": true,
            "sourceUrl": "magnet:?xt=test", "customName": "资料",
            "options": {"url": "magnet:?xt=test", "proxyUrl": "socks5://localhost:1080", "userAgent": "draft", "checksum": "sha256=abc"}});
        let params: super::TorrentConfirmParams =
            serde_json::from_value(wire).expect("draft confirmation");
        let encoded = serde_json::to_value(&params).expect("wire");
        let restored: super::TorrentConfirmParams =
            serde_json::from_value(encoded).expect("round trip");
        assert_eq!(restored.source_url, "magnet:?xt=test");
        assert_eq!(restored.selected_file_indices, [2]);
        assert!(restored.start_paused);
        assert_eq!(
            restored.options.expect("options").proxy_url,
            "socks5://localhost:1080"
        );
    }

    #[test]
    fn torrent_confirmation_requires_explicit_choices() {
        use super::TorrentConfirmParams;
        let wire = json!({"torrentBlobId": "torrent:preview", "saveDir": "C:\\Downloads",
            "selectedFileIndices": [0, 2]});
        let params: TorrentConfirmParams =
            serde_json::from_value(wire.clone()).expect("confirmation");
        assert_eq!(params.selected_file_indices, vec![0, 2]);
        assert!(!params.start_paused);
        assert!(params.queue_id.is_empty());
        for missing in ["torrentBlobId", "saveDir", "selectedFileIndices"] {
            let mut incomplete = wire.clone();
            incomplete.as_object_mut().expect("object").remove(missing);
            assert!(serde_json::from_value::<TorrentConfirmParams>(incomplete).is_err());
        }
        let mut renamed = wire;
        renamed["fileName"] = json!("renamed");
        assert!(
            serde_json::from_value::<TorrentConfirmParams>(renamed).is_err(),
            "the rename target is customName; fileName stays an unknown field"
        );
        let mut custom = json!({"torrentBlobId": "torrent:preview", "saveDir": "C:\\Downloads",
            "selectedFileIndices": [0, 2], "customName": "改名后的名字",
            "sourceUrl": "magnet:?xt=urn:btih:abc"});
        let params: TorrentConfirmParams =
            serde_json::from_value(custom.clone()).expect("custom name");
        assert_eq!(params.custom_name, "改名后的名字");
        assert_eq!(params.source_url, "magnet:?xt=urn:btih:abc");
        custom.as_object_mut().expect("object").remove("customName");
        custom.as_object_mut().expect("object").remove("sourceUrl");
        let params: TorrentConfirmParams =
            serde_json::from_value(custom).expect("omitted custom name");
        assert!(params.custom_name.is_empty());
        assert!(
            params.source_url.is_empty(),
            "older clients omit sourceUrl; the task then keeps the sentinel origin"
        );
        assert!(method::is_slow_daemon_method(
            method::DAEMON_TORRENT_CONFIRM
        ));
    }

    #[test]
    fn custom_download_name_accepts_plain_components_and_rejects_unportable_ones() {
        use super::{CustomNameIssue, MAX_CUSTOM_NAME_BYTES, validate_custom_name};
        for name in [
            "改名后的名字",
            "Movie.2026.1080p.mkv",
            "第 1 集 (1).mp4",
            "a.b.c",
            "trailing-dash-",
        ] {
            assert_eq!(validate_custom_name(name), Ok(()), "{name}");
        }
        for (name, expected_char) in [
            ("a/b", '/'),
            ("a\\b", '\\'),
            ("a:b", ':'),
            ("a*b", '*'),
            ("a?b", '?'),
            ("a\"b", '"'),
            ("a<b", '<'),
            ("a>b", '>'),
            ("a|b", '|'),
            ("a\tb", '\t'),
            ("a\u{202E}b", '\u{202E}'),
        ] {
            assert_eq!(
                validate_custom_name(name),
                Err(CustomNameIssue::IllegalCharacter(expected_char)),
                "{name:?}"
            );
        }
        assert_eq!(
            validate_custom_name("name."),
            Err(CustomNameIssue::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_custom_name("name "),
            Err(CustomNameIssue::TrailingDotOrSpace)
        );
        // 引擎清洗会把首尾的点和空格去掉（`...a` → `a`），所以开头也要拒。
        assert_eq!(
            validate_custom_name(".hidden"),
            Err(CustomNameIssue::TrailingDotOrSpace)
        );
        assert_eq!(
            validate_custom_name(" leading"),
            Err(CustomNameIssue::TrailingDotOrSpace)
        );
        // 零宽 / BOM / 行分隔符：引擎会删掉或原样留成换行，名字会与输入不一致。
        for invisible in [
            "\u{200B}",
            "a\u{200B}b",
            "\u{FEFF}",
            "a\u{2028}b",
            "a\u{2029}b",
        ] {
            assert!(
                matches!(
                    validate_custom_name(invisible),
                    Err(CustomNameIssue::IllegalCharacter(_))
                ),
                "{invisible:?}"
            );
        }
        for reserved in ["CON", "nul", "NUL.txt", "com1", "LPT9.tar", "CON .txt"] {
            assert_eq!(
                validate_custom_name(reserved),
                Err(CustomNameIssue::ReservedName),
                "{reserved}"
            );
        }
        assert_eq!(validate_custom_name("   "), Err(CustomNameIssue::Empty));
        let longest = "a".repeat(MAX_CUSTOM_NAME_BYTES);
        assert_eq!(validate_custom_name(&longest), Ok(()));
        let too_long = "a".repeat(MAX_CUSTOM_NAME_BYTES + 1);
        assert_eq!(
            validate_custom_name(&too_long),
            Err(CustomNameIssue::TooLong)
        );
        // 多字节名字按字节而不是字符计长。
        let cjk = "字".repeat(MAX_CUSTOM_NAME_BYTES / 3);
        assert_eq!(validate_custom_name(&cjk), Ok(()));
        let cjk_too_long = "字".repeat((MAX_CUSTOM_NAME_BYTES / 3) + 1);
        assert_eq!(
            validate_custom_name(&cjk_too_long),
            Err(CustomNameIssue::TooLong)
        );
        assert!(
            !CustomNameIssue::ReservedName.message().is_empty(),
            "issues carry a diagnostic for the daemon error body"
        );
    }

    #[test]
    fn torrent_preview_wire_retains_unicode_paths_and_original_indices() {
        let wire = json!({
            "torrentBlobId": "torrent:preview",
            "name": "资料",
            "totalBytes": 4,
            "files": [{ "index": 2, "path": "目录/说明.txt", "size": 4 }],
        });
        let preview: TorrentPreviewDto =
            serde_json::from_value(wire.clone()).expect("decode preview");
        assert_eq!(serde_json::to_value(preview).expect("encode preview"), wire);
        assert!(serde_json::from_value::<TorrentPreviewParams>(json!({})).is_err());
        for name in [
            method::AGENT_CAPTURE_PREVIEW_TORRENT_FILE,
            method::DAEMON_TORRENT_CONFIRM,
            method::DAEMON_TORRENT_PREVIEW,
            method::DAEMON_TORRENT_DISCARD_PREVIEW,
            method::DAEMON_TORRENT_PREPARE_MAGNET,
            method::DAEMON_TORRENT_RESOLVE_MAGNET,
            method::DAEMON_TORRENT_CANCEL_MAGNET,
        ] {
            assert_eq!(
                method::ALL_METHODS
                    .iter()
                    .filter(|known| **known == name)
                    .count(),
                1
            );
        }
        assert!(method::is_slow_daemon_method(
            method::DAEMON_TORRENT_PREVIEW
        ));
        // 丢弃会等正在进行的确认，不能堵住有序通道。
        assert!(method::is_slow_daemon_method(
            method::DAEMON_TORRENT_DISCARD_PREVIEW
        ));
        assert!(method::is_slow_daemon_method(
            method::DAEMON_TORRENT_RESOLVE_MAGNET
        ));
        assert!(!method::is_slow_daemon_method(
            method::DAEMON_TORRENT_CANCEL_MAGNET
        ));
    }
}
