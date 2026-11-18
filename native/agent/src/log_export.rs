//! Doctor 日志导出：日志目录发现与无依赖的 stored-zip 打包。
//!
//! agent 没有归档 crate，这里手写 ZIP（无压缩，方法 0），只需要 CRC-32 与固定头部，
//! 任何系统解压器都能直接打开；日志文本本身不压缩换取零依赖。

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{LogExportResult, LogPathsDto};
use regex::Regex;

/// 单个日志文件上限；超过时只保留末尾，避免失控日志撑爆内存与导出包。
const MAX_LOG_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// 每个目录最多收集的日志文件数（按修改时间取最新）。
const MAX_LOG_FILES_PER_DIR: usize = 20;

/// 保留调用方选择的文件与父目录；扩展名不是 .zip 时追加 .zip，拒绝目录、控制字符与设备路径。
pub fn resolve_target(target_path: &str) -> Result<PathBuf, &'static str> {
    if target_path.chars().any(char::is_control) {
        return Err("exportLogs targetPath contains control characters");
    }
    let target_path = target_path.trim();
    if target_path.is_empty() {
        return Err("exportLogs requires targetPath");
    }
    if target_path
        .rsplit(std::path::is_separator)
        .next()
        .is_some_and(|name| matches!(name, "" | "." | ".."))
    {
        return Err("exportLogs targetPath must name a file");
    }
    let mut path = PathBuf::from(target_path);
    if path.file_name().is_none() {
        return Err("exportLogs targetPath must name a file");
    }
    #[cfg(windows)]
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => {
                if !path.has_root()
                    || matches!(
                        prefix.kind(),
                        std::path::Prefix::DeviceNS(_) | std::path::Prefix::Verbatim(_)
                    )
                {
                    return Err("exportLogs targetPath must be a filesystem file");
                }
            }
            std::path::Component::Normal(name) => {
                let name = name.to_str().ok_or("exportLogs targetPath is not UTF-8")?;
                let stem = name
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(' ');
                let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
                    .iter()
                    .any(|device| stem.eq_ignore_ascii_case(device))
                    || (stem.get(..3).is_some_and(|prefix| {
                        prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
                    }) && stem.get(3..).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    }));
                if device
                    || name.ends_with(' ')
                    || name.ends_with('.')
                    || name
                        .chars()
                        .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
                {
                    return Err("exportLogs targetPath must be a filesystem file");
                }
            }
            _ => {}
        }
    }
    if path.is_dir() {
        return Err("exportLogs targetPath must name a file");
    }
    let is_zip = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !is_zip {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("exportLogs targetPath must name a file")?;
        path.set_file_name(format!("{name}.zip"));
        if path.is_dir() {
            return Err("exportLogs targetPath must name a file");
        }
    }
    Ok(path)
}

/// agent 侧诊断日志目录（`agent.log` / `desktop.log` / `fluxdownd.stderr.log`）。
#[must_use]
pub fn agent_log_dir(agent_data_dir: &Path) -> PathBuf {
    agent_data_dir.join("logs")
}

/// 日志目录信息；daemon 目录未知时为空串。
#[must_use]
pub fn log_paths(agent_dir: &Path, daemon_log_dir: Option<&str>) -> LogPathsDto {
    LogPathsDto {
        agent_log_dir: agent_dir.display().to_string(),
        daemon_log_dir: daemon_log_dir.unwrap_or_default().to_owned(),
    }
}

/// NMH 中继自身的诊断日志路径；与 `native/nmh/src/main.rs::log_path` 保持一致。
#[must_use]
pub fn nmh_relay_log_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("TEMP")
            .or_else(|| std::env::var_os("TMP"))
            .map(|tmp| PathBuf::from(tmp).join("fluxdown_nmh.log"))
    }
    #[cfg(not(windows))]
    {
        let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
        let dir = match home {
            Some(home) if cfg!(target_os = "macos") => home
                .join("Library")
                .join("Application Support")
                .join("fluxdown"),
            Some(home) => home.join(".local").join("share").join("fluxdown"),
            None => PathBuf::from("/tmp"),
        };
        Some(dir.join("fluxdown_nmh.log"))
    }
}

/// 收集目录下的 `*.log` 文件（含轮转产物如 `app.log.1`），返回 `(文件名, 内容)`。
pub async fn collect_log_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut candidates: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_log_name(&name) {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        candidates.push((modified, name, entry.path()));
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.truncate(MAX_LOG_FILES_PER_DIR);
    let mut files = Vec::with_capacity(candidates.len());
    for (_, name, path) in candidates {
        if let Some(bytes) = read_tail(&path, MAX_LOG_FILE_BYTES).await {
            files.push((name, bytes));
        }
    }
    files
}

fn is_log_name(name: &str) -> bool {
    name.ends_with(".log")
        || name.rsplit_once(".log.").is_some_and(|(_, suffix)| {
            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
        })
}

/// 读取文件；超过 `limit` 时只读末尾并在开头加一行截断标记。
pub async fn read_tail(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    let mut bytes = Vec::new();
    if len > limit {
        let skipped = len - limit;
        file.seek(std::io::SeekFrom::Start(skipped)).await.ok()?;
        bytes.extend_from_slice(format!("[truncated: first {skipped} bytes omitted]\n").as_bytes());
    }
    file.read_to_end(&mut bytes).await.ok()?;
    Some(bytes)
}

/// 原子写出：先写临时文件再 rename，返回字节数。
pub async fn write_atomic(target: &Path, bytes: &[u8]) -> Result<LogExportResult, std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temp = target.with_extension(format!("zip.{}.tmp", std::process::id()));
    let result = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, target).await
    }
    .await;
    if let Err(error) = result {
        if let Err(cleanup_error) = tokio::fs::remove_file(&temp).await
            && cleanup_error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %temp.display(), %cleanup_error, "failed to remove log export temporary file");
        }
        return Err(error);
    }
    Ok(LogExportResult {
        path: target.display().to_string(),
        bytes: bytes.len() as u64,
    })
}

/// 无压缩 ZIP 写入器（PKZIP APPNOTE 4.4，方法 0 stored，UTF-8 文件名）。
pub struct ZipWriter {
    body: Vec<u8>,
    central: Vec<u8>,
    entries: u16,
    dos_time: u16,
    dos_date: u16,
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipWriter {
    #[must_use]
    pub fn new() -> Self {
        let (dos_time, dos_date) = dos_datetime(SystemTime::now());
        Self {
            body: Vec::new(),
            central: Vec::new(),
            entries: 0,
            dos_time,
            dos_date,
        }
    }

    /// 追加一个条目；ZIP32 限制单文件与条目数，超限静默截断到限制内。
    pub fn add(&mut self, name: &str, data: &[u8]) {
        if self.entries == u16::MAX {
            return;
        }
        let data = &data[..data.len().min(u32::MAX as usize)];
        let name = name.as_bytes();
        let name = &name[..name.len().min(u16::MAX as usize)];
        let crc = crc32(data);
        let size = data.len() as u32;
        let offset = self.body.len() as u32;

        // 本地文件头。
        self.body.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        self.body.extend_from_slice(&10_u16.to_le_bytes()); // version needed
        self.body.extend_from_slice(&0x0800_u16.to_le_bytes()); // UTF-8 名称
        self.body.extend_from_slice(&0_u16.to_le_bytes()); // stored
        self.body.extend_from_slice(&self.dos_time.to_le_bytes());
        self.body.extend_from_slice(&self.dos_date.to_le_bytes());
        self.body.extend_from_slice(&crc.to_le_bytes());
        self.body.extend_from_slice(&size.to_le_bytes());
        self.body.extend_from_slice(&size.to_le_bytes());
        self.body
            .extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.body.extend_from_slice(&0_u16.to_le_bytes()); // extra
        self.body.extend_from_slice(name);
        self.body.extend_from_slice(data);

        // 中央目录记录。
        self.central
            .extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        self.central.extend_from_slice(&20_u16.to_le_bytes()); // version made by
        self.central.extend_from_slice(&10_u16.to_le_bytes()); // version needed
        self.central.extend_from_slice(&0x0800_u16.to_le_bytes());
        self.central.extend_from_slice(&0_u16.to_le_bytes());
        self.central.extend_from_slice(&self.dos_time.to_le_bytes());
        self.central.extend_from_slice(&self.dos_date.to_le_bytes());
        self.central.extend_from_slice(&crc.to_le_bytes());
        self.central.extend_from_slice(&size.to_le_bytes());
        self.central.extend_from_slice(&size.to_le_bytes());
        self.central
            .extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.central.extend_from_slice(&0_u16.to_le_bytes()); // extra
        self.central.extend_from_slice(&0_u16.to_le_bytes()); // comment
        self.central.extend_from_slice(&0_u16.to_le_bytes()); // disk
        self.central.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
        self.central.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name);
        self.entries += 1;
    }

    /// 写出中央目录与结束记录，返回完整归档字节。
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        let central_offset = self.body.len() as u32;
        let central_size = self.central.len() as u32;
        self.body.extend_from_slice(&self.central);
        self.body.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        self.body.extend_from_slice(&0_u16.to_le_bytes()); // this disk
        self.body.extend_from_slice(&0_u16.to_le_bytes()); // central dir disk
        self.body.extend_from_slice(&self.entries.to_le_bytes());
        self.body.extend_from_slice(&self.entries.to_le_bytes());
        self.body.extend_from_slice(&central_size.to_le_bytes());
        self.body.extend_from_slice(&central_offset.to_le_bytes());
        self.body.extend_from_slice(&0_u16.to_le_bytes()); // comment
        self.body
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// UNIX 时间 → MS-DOS 时间/日期字段（UTC；秒精度 2s）。
fn dos_datetime(time: SystemTime) -> (u16, u16) {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let year = year.clamp(1980, 2107);
    let dos_time = ((hour as u16) << 11) | ((minute as u16) << 5) | (second as u16 / 2);
    let dos_date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    (dos_time, dos_date)
}

/// 自 1970-01-01 起的天数 → 公历 (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 对导出的文本逐条套用共享脱敏规则（`fluxdown_logfile::SANITIZE_PATTERNS`，单一来源）；
/// 非 UTF-8 字节按有损解码，不会因编码问题跳过脱敏。
#[must_use]
pub fn sanitize_text(bytes: &[u8]) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    for (regex, replacement) in sanitize_rules() {
        if let Cow::Owned(next) = regex.replace_all(&text, *replacement) {
            text = next;
        }
    }
    text.into_bytes()
}

/// 编译一次并缓存；模式是编译期常量，编译失败属于规则表缺陷，测试会钉住条数一致。
static SANITIZE_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    fluxdown_logfile::SANITIZE_PATTERNS
        .iter()
        .filter_map(|(pattern, replacement)| match Regex::new(pattern) {
            Ok(regex) => Some((regex, *replacement)),
            Err(error) => {
                tracing::error!(pattern, error = %error, "log sanitize rule does not compile");
                None
            }
        })
        .collect()
});

fn sanitize_rules() -> &'static [(Regex, &'static str)] {
    &SANITIZE_RULES
}

/// daemon 快照等 JSON 导出：先按键名抹掉密钥类值（代理账号密码、Webhook 端点及其签名密钥 /
/// 自定义头、任何名字含 password / secret / token 的字段），再套用文本规则；解析失败时只做
/// 文本脱敏。
#[must_use]
pub fn sanitize_json_export(bytes: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return sanitize_text(bytes);
    };
    redact_secret_values(&mut value);
    match serde_json::to_vec_pretty(&value) {
        Ok(redacted) => sanitize_text(&redacted),
        Err(_) => sanitize_text(bytes),
    }
}

const REDACTED_VALUE: &str = "***";

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "proxy_username" | "proxy_password" | "proxy_rules" | "webhook.endpoints"
    ) || ["password", "secret", "token", "authorization", "cookie"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn redact_secret_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key) {
                    *child = serde_json::Value::String(REDACTED_VALUE.to_owned());
                } else {
                    redact_secret_values(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_secret_values),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ZipWriter, civil_from_days, crc32, is_log_name, resolve_target, sanitize_json_export,
        sanitize_rules, sanitize_text,
    };

    #[test]
    fn every_shared_sanitize_pattern_compiles_into_a_rule() {
        assert_eq!(
            sanitize_rules().len(),
            fluxdown_logfile::SANITIZE_PATTERNS.len()
        );
    }

    #[test]
    fn log_text_loses_credentials_tokens_and_user_names() {
        let log = "webhook failed: https://api.telegram.org/bot123456:AAH-secret_token/sendMessage\n\
                   gotify POST https://push.example.com/message?token=abc123&x=1\n\
                   proxy http://user:hunter2@proxy.local:8080 via /home/alice/.config\n\
                   Cookie: sid=abcdef\nAuthorization: Bearer topsecret\n\
                   private plugin GET https://cloud.example.com/api/v1/private-plugins/packages/TkT-9f8e7d6c5b4a (120s)\n\
                   C:\\Users\\Bob\\AppData\\fluxdown\n";
        let cleaned = String::from_utf8(sanitize_text(log.as_bytes())).expect("utf8");
        for leaked in [
            "AAH-secret_token",
            "abc123",
            "hunter2",
            "alice",
            "sid=abcdef",
            "topsecret",
            "Bob",
            "TkT-9f8e7d6c5b4a",
        ] {
            assert!(!cleaned.contains(leaked), "{leaked} leaked: {cleaned}");
        }
        // 排障需要的主机与错误上下文保留。
        assert!(cleaned.contains("api.telegram.org"));
        assert!(cleaned.contains("push.example.com"));
        assert!(cleaned.contains("/api/v1/private-plugins/packages/[REDACTED]"));
        assert!(cleaned.contains("proxy.local:8080"));
    }

    #[test]
    fn daemon_snapshot_export_masks_secret_config_but_keeps_the_rest() {
        let snapshot = serde_json::json!({
            "config": {
                "revision": 7,
                "values": {
                    "proxy_username": "alice",
                    "proxy_password": "hunter2",
                    "proxy_url": "http://127.0.0.1:7890",
                    "webhook.endpoints":
                        "[{\"url\":\"https://hooks.example.com/x\",\"signSecret\":\"s3cr3t\"}]",
                    "max_concurrent_downloads": "3",
                }
            },
            "tasks": [{ "id": "t1", "signSecret": "s3cr3t", "url": "https://h.example/f" }],
        });
        let raw = serde_json::to_vec(&snapshot).expect("serialize snapshot");
        let cleaned = sanitize_json_export(&raw);
        let text = String::from_utf8(cleaned.clone()).expect("utf8");
        for leaked in ["alice", "hunter2", "s3cr3t", "hooks.example.com"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
        let value: serde_json::Value = serde_json::from_slice(&cleaned).expect("still JSON");
        assert_eq!(value["config"]["revision"], 7);
        assert_eq!(
            value["config"]["values"]["proxy_url"],
            "http://127.0.0.1:7890"
        );
        assert_eq!(value["config"]["values"]["max_concurrent_downloads"], "3");
        assert_eq!(value["tasks"][0]["url"], "https://h.example/f");
        // 无法解析的导出内容也按文本规则兜底。
        let broken = sanitize_json_export(b"not json: https://u:pw@host/x");
        assert!(!String::from_utf8_lossy(&broken).contains(":pw@"));
    }

    #[test]
    fn crc32_matches_reference_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn civil_dates_round_trip_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_699), (2026, 9, 3));
    }

    #[test]
    fn stored_zip_layout_is_well_formed() {
        let mut zip = ZipWriter::new();
        zip.add("a.txt", b"hello");
        zip.add("dir/b.log", b"");
        let bytes = zip.finish();
        assert_eq!(&bytes[0..4], &[0x50, 0x4b, 0x03, 0x04]);
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]), 2);
        let central_offset = u32::from_le_bytes([
            bytes[eocd + 16],
            bytes[eocd + 17],
            bytes[eocd + 18],
            bytes[eocd + 19],
        ]) as usize;
        assert_eq!(
            &bytes[central_offset..central_offset + 4],
            &[0x50, 0x4b, 0x01, 0x02]
        );
        let first_entry_len = 30 + "a.txt".len() + "hello".len();
        let second_entry_len = 30 + "dir/b.log".len();
        assert_eq!(central_offset, first_entry_len + second_entry_len);
    }

    #[test]
    fn log_names_and_targets_are_normalized() {
        assert!(is_log_name("app.log"));
        assert!(is_log_name("app.log.3"));
        assert!(!is_log_name("app.log.bak"));
        assert!(!is_log_name("state.json"));
        let root = std::env::temp_dir().join(format!("fluxdown_logs_{}", uuid::Uuid::new_v4()));
        for (selected, exported) in [
            ("out.txt", "out.txt.zip"),
            ("out.ZIP", "out.ZIP"),
            ("bundle", "bundle.zip"),
        ] {
            assert_eq!(
                resolve_target(&root.join(selected).display().to_string()).expect("file target"),
                root.join(exported)
            );
        }
    }

    #[test]
    fn export_target_rejects_directory_and_non_file_inputs() {
        let root = std::env::temp_dir().join(format!("fluxdown_logs_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("bundle.zip")).expect("create export directories");
        assert!(resolve_target(&root.display().to_string()).is_err());
        assert!(resolve_target(&root.join("bundle").display().to_string()).is_err());
        for target in [
            "",
            "   ",
            ".",
            "..",
            "/",
            "missing/",
            "missing/.",
            "missing/..",
            "archive\0.zip",
            "archive\n.zip",
        ] {
            assert!(resolve_target(target).is_err(), "{target:?}");
        }
        std::fs::remove_dir_all(root).expect("remove export directories");
    }

    #[tokio::test]
    async fn export_archive_writes_only_the_selected_file() {
        let root = std::env::temp_dir().join(format!("fluxdown_logs_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root)
            .await
            .expect("create export directory");
        let chosen = root.join("logs & 名字 (oct).ZIP");
        let neighbor = root.join("other.zip");
        tokio::fs::write(&chosen, b"old archive")
            .await
            .expect("create old archive");
        tokio::fs::write(&neighbor, b"unrelated file")
            .await
            .expect("create neighbor");
        let target = resolve_target(&chosen.display().to_string()).expect("selected file");
        assert_eq!(target, chosen);
        let mut zip = ZipWriter::new();
        zip.add("agent.log", b"log line\n");
        let bytes = zip.finish();
        let result = super::write_atomic(&target, &bytes)
            .await
            .expect("export archive");
        assert_eq!(result.path, chosen.display().to_string());
        assert_eq!(tokio::fs::read(&chosen).await.expect("read archive"), bytes);
        assert_eq!(
            tokio::fs::read(&neighbor).await.expect("read neighbor"),
            b"unrelated file"
        );
        tokio::fs::remove_dir_all(root)
            .await
            .expect("remove export directory");
    }

    #[cfg(windows)]
    #[test]
    fn export_target_rejects_windows_devices_and_alternate_streams() {
        let root = std::env::temp_dir().join(format!("fluxdown_logs_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create export directory");
        for name in [
            "NUL.zip",
            "NUL .zip",
            "con.txt",
            "COM1.zip",
            "lpt¹.zip",
            r"CON\archive.zip",
            "archive.zip:stream",
            r"alias.\archive.zip",
        ] {
            let target = root.join(name).display().to_string();
            assert!(resolve_target(&target).is_err(), "{target:?}");
        }
        for target in [
            r"C:archive.zip",
            r"\\.\NUL",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\archive.zip",
        ] {
            assert!(resolve_target(target).is_err(), "{target:?}");
        }
        for target in [
            root.join("COM10.zip"),
            std::fs::canonicalize(&root)
                .expect("verbatim export directory")
                .join("archive.zip"),
        ] {
            assert_eq!(
                resolve_target(&target.display().to_string()).expect("filesystem file"),
                target
            );
        }
        std::fs::remove_dir_all(root).expect("remove export directory");
    }
}
