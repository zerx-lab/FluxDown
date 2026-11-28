//! `fd_get_logs`:读取最近的诊断日志(agent / daemon / desktop 三个组件)。
//!
//! RPC 对应:`agent.diagnostics.logPaths` 取两个日志目录,文件由本模块直接读(只读文件尾部)。
//! 日志写入时已按日志脱敏规则处理,这里的结果仍会经 `mod.rs` 的统一收尾再过一遍脱敏。
//!
//! 文件布局与行格式:
//! - `agent.log` / `desktop.log`(agent 日志目录,轮转副本 `*.log.1`):`<UTC 时间戳> <级别> <文本>`;
//! - `fluxdown_<日期>[.<序号>].log`(daemon 日志目录):`<HH:MM:SS.mmm> <级别> <文本>`,日期只在文件名里,
//!   输出时补回行首。
//!
//! 续行(多行日志)缩进后接在上一条记录后面,过滤与计数都以「记录」为单位。

use std::path::{Path, PathBuf};

use fluxdown_logfile::Level;
use fluxdown_protocol::{LogPathsDto, method};
use serde::Deserialize;
use serde_json::{Value, json};

use super::output::truncate_chars;
use super::tasks::{check_max, invalid};
use super::{ToolContext, ToolError, parse_args};

const DEFAULT_LINES: u32 = 200;
const MAX_LINES: u32 = 500;
const MAX_QUERY_CHARS: usize = 200;
/// 每个文件只读尾部这么多字节(单个日志文件上限 2 MiB,足够覆盖最近的记录)。
const TAIL_BYTES: u64 = 1024 * 1024;
/// 每个组件最多回溯的文件数(当前文件 + 轮转 / 前几天)。
const MAX_FILES_PER_SOURCE: usize = 4;
/// 每个组件的 JSON 骨架(组件名 / 文件名列表 / 计数字段)预留的字节数。
const SOURCE_OVERHEAD_BYTES: usize = 320;
/// 顶层字段(`level` / `query` / `order` ……)预留的字节数;`query` 最长 200 字符,转义后可能翻倍。
const ENVELOPE_OVERHEAD_BYTES: usize = 160 + 2 * MAX_QUERY_CHARS;
const MAX_RECORD_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogSource {
    Agent,
    Daemon,
    Desktop,
}

impl LogSource {
    const ALL: [Self; 3] = [Self::Agent, Self::Daemon, Self::Desktop];

    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Daemon => "daemon",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    fn level(self) -> Level {
        match self {
            Self::Error => Level::Error,
            Self::Warn => Level::Warn,
            Self::Info => Level::Info,
            Self::Debug => Level::Debug,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogArgs {
    #[serde(default)]
    source: Option<LogSource>,
    #[serde(default)]
    level: Option<LogLevel>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    lines: Option<u32>,
}

/// 一条日志记录:首行 + 续行。
#[derive(Debug, PartialEq, Eq)]
struct Record {
    level: Level,
    text: String,
}

/// 日志行是否是一条新记录的开头(agent 的 UTC 时间戳或 daemon 的 `HH:MM:SS.mmm`)。
fn is_record_start(line: &str) -> bool {
    let bytes = line.as_bytes();
    let utc = bytes.len() >= 20 && bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b'T';
    let clock = bytes.len() >= 12 && bytes[2] == b':' && bytes[5] == b':' && bytes[8] == b'.';
    (utc || clock) && bytes[0].is_ascii_digit()
}

/// 会话分隔横幅(`====== <时间> pid=… reason=… ======`);其后的会话头行不属于上一条记录。
fn is_banner(line: &str) -> bool {
    line.starts_with("======")
}

/// 日志文本 → 记录。`date` 非空时补在每条记录行首(daemon 行本身没有日期)。
fn parse_records(text: &str, date: Option<&str>) -> Vec<Record> {
    let mut records: Vec<Record> = Vec::new();
    let mut attached = false;
    for line in text.lines() {
        if is_record_start(line) {
            let level = line
                .split_whitespace()
                .nth(1)
                .and_then(Level::parse)
                .unwrap_or(Level::Info);
            let text = match date {
                Some(date) => format!("{date} {line}"),
                None => line.to_owned(),
            };
            records.push(Record { level, text });
            attached = true;
        } else if is_banner(line) {
            attached = false;
        } else if attached && let Some(last) = records.last_mut() {
            last.text.push('\n');
            last.text.push_str(line);
        }
    }
    records
}

/// 选择结果:按时间从旧到新排列的行。
#[derive(Debug, Default, PartialEq, Eq)]
struct Selection {
    lines: Vec<String>,
    /// 还有更早的匹配行因条数或字节预算没有返回。
    truncated: bool,
}

/// 从最新的文件开始回溯,取满足级别 / 关键字的最近 `limit` 条记录,且总字节不超过 `budget`。
/// `files_newest_first` 的每个元素是一个文件里按时间升序的记录。
fn select(
    files_newest_first: &[Vec<Record>],
    min: Level,
    query: Option<&str>,
    limit: usize,
    budget: usize,
) -> Selection {
    let mut picked: Vec<String> = Vec::new();
    let mut used = 0_usize;
    let mut truncated = false;
    'files: for records in files_newest_first {
        for record in records.iter().rev() {
            if record.level > min {
                continue;
            }
            if let Some(query) = query
                && !record.text.to_ascii_lowercase().contains(query)
            {
                continue;
            }
            let line = truncate_chars(&record.text, MAX_RECORD_CHARS);
            // JSON 编码后的长度(含引号与转义)+ 分隔逗号。
            let cost =
                serde_json::to_string(&line).map_or(line.len() + 4, |encoded| encoded.len() + 1);
            if picked.len() == limit || used + cost > budget {
                truncated = true;
                break 'files;
            }
            used += cost;
            picked.push(line);
        }
    }
    picked.reverse();
    Selection {
        lines: picked,
        truncated,
    }
}

/// daemon 日志文件名按新旧排序(新在前):`fluxdown_<日期>.log` 早于同日的 `fluxdown_<日期>.<序号>.log`。
/// 返回 `(文件名, 日期)`。
fn daemon_log_order(names: Vec<String>) -> Vec<(String, String)> {
    let mut parsed: Vec<(String, u32, String)> = names
        .into_iter()
        .filter_map(|name| {
            let stem = name.strip_prefix("fluxdown_")?.strip_suffix(".log")?;
            let (date, part) = match stem.split_once('.') {
                Some((date, part)) => (date, part.parse::<u32>().ok()?),
                None => (stem, 0),
            };
            if date.is_empty() {
                return None;
            }
            Some((date.to_owned(), part, name))
        })
        .collect();
    parsed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    parsed
        .into_iter()
        .map(|(date, _, name)| (name, date))
        .collect()
}

/// 一个组件要读的文件(新在前),以及是否要给行补日期。
struct SourceFiles {
    files: Vec<(PathBuf, Option<String>)>,
}

async fn daemon_files(dir: &Path) -> Result<SourceFiles, std::io::Error> {
    let mut names = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let files = daemon_log_order(names)
        .into_iter()
        .take(MAX_FILES_PER_SOURCE)
        .map(|(name, date)| (dir.join(name), Some(date)))
        .collect();
    Ok(SourceFiles { files })
}

fn stem_files(dir: &Path, stem: &str) -> SourceFiles {
    SourceFiles {
        files: vec![
            (dir.join(format!("{stem}.log")), None),
            (dir.join(format!("{stem}.log.1")), None),
        ],
    }
}

/// 读取并整理一个组件;目录或文件不存在时返回说明,不算失败。
async fn read_source(
    source: LogSource,
    paths: &LogPathsDto,
    level: LogLevel,
    query: Option<&str>,
    lines: usize,
    budget: usize,
) -> Value {
    let name = source.as_str();
    let dir = match source {
        LogSource::Agent | LogSource::Desktop => paths.agent_log_dir.as_str(),
        LogSource::Daemon => paths.daemon_log_dir.as_str(),
    };
    if dir.is_empty() {
        return json!({ "source": name, "available": false, "reason": "log directory is unknown" });
    }
    let dir = Path::new(dir);
    let files = match source {
        LogSource::Agent => Ok(stem_files(dir, "agent")),
        LogSource::Desktop => Ok(stem_files(dir, "desktop")),
        LogSource::Daemon => daemon_files(dir).await,
    };
    let files = match files {
        Ok(files) => files,
        Err(error) => {
            return json!({ "source": name, "available": false, "reason": format!("cannot list the log directory: {error}") });
        }
    };
    let mut parsed: Vec<Vec<Record>> = Vec::new();
    let mut read_names: Vec<String> = Vec::new();
    for (path, date) in &files.files {
        let Some(bytes) = crate::log_export::read_tail(path, TAIL_BYTES).await else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        parsed.push(parse_records(&text, date.as_deref()));
        if let Some(file_name) = path.file_name() {
            read_names.push(file_name.to_string_lossy().into_owned());
        }
    }
    if parsed.is_empty() {
        return json!({ "source": name, "available": false, "reason": "no log file found" });
    }
    let selection = select(&parsed, level.level(), query, lines, budget);
    json!({
        "source": name,
        "available": true,
        "files": read_names,
        "returned": selection.lines.len(),
        "olderMatchesOmitted": selection.truncated,
        "lines": selection.lines,
    })
}

/// 全部组件合计的行字节预算:回传上限扣掉骨架与 1/8 的脱敏 / 转义余量。
/// 预算按回传上限等比缩放,且永远小于它:被丢弃的只会是最旧的行(见 [`select`]),最新的行与
/// `olderMatchesOmitted` 标注原样保留,而不是被收尾阶段从尾部截掉。
fn total_budget(limit: usize, sources: usize) -> usize {
    limit
        .saturating_sub(limit / 8)
        .saturating_sub(ENVELOPE_OVERHEAD_BYTES)
        .saturating_sub(sources * SOURCE_OVERHEAD_BYTES)
}

pub(super) async fn logs(ctx: &ToolContext, args: Value, limit: usize) -> Result<Value, ToolError> {
    let LogArgs {
        source,
        level,
        query,
        lines,
    } = parse_args(args)?;
    let lines = lines.unwrap_or(DEFAULT_LINES);
    if !(1..=MAX_LINES).contains(&lines) {
        return Err(invalid(format!("lines must be between 1 and {MAX_LINES}")));
    }
    let lines = lines as usize;
    if let Some(query) = &query {
        check_max("query", query, MAX_QUERY_CHARS)?;
    }
    let query = query
        .map(|query| query.trim().to_ascii_lowercase())
        .filter(|query| !query.is_empty());
    let level = level.unwrap_or_default();
    let sources: Vec<LogSource> = match source {
        Some(source) => vec![source],
        None => LogSource::ALL.to_vec(),
    };
    let paths: LogPathsDto = ctx
        .call_as(method::AGENT_DIAGNOSTICS_LOG_PATHS, json!({}))
        .await?;
    let budget = total_budget(limit, sources.len()) / sources.len();
    let mut reports = Vec::with_capacity(sources.len());
    for source in sources {
        reports.push(read_source(source, &paths, level, query.as_deref(), lines, budget).await);
    }
    Ok(json!({
        "level": level.as_str(),
        "query": query,
        "linesRequested": lines,
        "order": "oldest to newest within each source",
        "sources": reports,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT_LOG: &str = "\
[truncated: first 12 bytes omitted]
====== 2026-11-27T01:00:00.000Z pid=10 reason=start ======
fluxdown-agent 1.0 session header
2026-11-27T01:00:01.100Z INFO  agent started
2026-11-27T01:00:02.200Z ERROR download failed
    caused by: connection reset
2026-11-27T01:00:03.300Z WARN  slow response
2026-11-27T01:00:04.400Z DEBUG verbose detail
====== 2026-11-27T02:00:00.000Z pid=11 reason=start ======
second session header line
2026-11-27T02:00:01.000Z INFO  agent restarted
";

    const DAEMON_LOG: &str = "\
09:00:00.001 INFO  daemon ready
09:00:01.002 task 42 queued
09:00:02.003 ERROR  engine error: timeout
";

    #[test]
    fn parses_records_with_continuations_and_banners() {
        let records = parse_records(AGENT_LOG, None);
        assert_eq!(records.len(), 5);
        assert_eq!(records[0].level, Level::Info);
        assert_eq!(records[1].level, Level::Error);
        assert!(
            records[1]
                .text
                .ends_with("download failed\n    caused by: connection reset")
        );
        assert_eq!(records[3].level, Level::Debug);
        // 重启横幅之后的会话头行不能并进上一条记录。
        assert!(!records[3].text.contains("second session"));
        assert!(!records[0].text.contains("session header"));
    }

    #[test]
    fn daemon_lines_get_the_date_and_default_level() {
        let records = parse_records(DAEMON_LOG, Some("2026-11-27"));
        assert_eq!(records.len(), 3);
        assert!(records[0].text.starts_with("2026-11-27 09:00:00.001"));
        assert_eq!(
            records[1].level,
            Level::Info,
            "a line without a level counts as info"
        );
        assert_eq!(records[2].level, Level::Error);
    }

    #[test]
    fn select_filters_by_level_and_query() {
        let files = vec![parse_records(AGENT_LOG, None)];
        let errors = select(&files, Level::Error, None, 50, 10_000);
        assert_eq!(errors.lines.len(), 1);
        assert!(errors.lines[0].contains("download failed"));
        let info = select(&files, Level::Info, None, 50, 10_000);
        assert_eq!(info.lines.len(), 4, "debug is below the minimum level");
        let debug = select(&files, Level::Debug, None, 50, 10_000);
        assert_eq!(debug.lines.len(), 5);
        let by_query = select(&files, Level::Debug, Some("restarted"), 50, 10_000);
        assert_eq!(by_query.lines.len(), 1);
        let by_continuation = select(&files, Level::Debug, Some("connection reset"), 50, 10_000);
        assert_eq!(by_continuation.lines.len(), 1);
    }

    #[test]
    fn select_keeps_the_newest_lines_in_order() {
        let files = vec![parse_records(AGENT_LOG, None)];
        let picked = select(&files, Level::Debug, None, 2, 10_000);
        assert!(picked.truncated);
        assert_eq!(picked.lines.len(), 2);
        assert!(picked.lines[0].contains("verbose detail"));
        assert!(picked.lines[1].contains("agent restarted"));
    }

    #[test]
    fn select_spans_files_newest_first() {
        let newest = parse_records("09:00:05.000 INFO  new\n", Some("2026-11-28"));
        let older = parse_records("23:00:00.000 INFO  old\n", Some("2026-11-27"));
        let picked = select(&[newest, older], Level::Info, None, 10, 10_000);
        assert_eq!(picked.lines.len(), 2);
        assert!(picked.lines[0].contains(" old"), "{:?}", picked.lines);
        assert!(picked.lines[1].contains(" new"));
        assert!(!picked.truncated);
    }

    #[test]
    fn select_respects_the_byte_budget_at_char_boundaries() {
        let long = "错".repeat(MAX_RECORD_CHARS * 2);
        let text = format!(
            "2026-11-27T01:00:00.000Z INFO  {long}\n2026-11-27T01:00:01.000Z INFO  {long}\n2026-11-27T01:00:02.000Z INFO  {long}\n"
        );
        let files = vec![parse_records(&text, None)];
        let picked = select(&files, Level::Info, None, 50, 2_000);
        assert!(picked.truncated);
        assert!(!picked.lines.is_empty());
        let total: usize = picked.lines.iter().map(|line| line.len()).sum();
        assert!(total <= 2_000, "{total}");
        for line in &picked.lines {
            assert!(
                line.ends_with('…'),
                "multi-byte text is cut at a char boundary"
            );
        }
    }

    #[test]
    fn total_budget_scales_with_the_limit_and_never_underflows() {
        let small = total_budget(24_000, 3);
        let large = total_budget(64 * 1024, 3);
        assert!(small < 24_000 && small > 12_000, "{small}");
        assert!(large > small && large < 64 * 1024, "{large}");
        assert_eq!(total_budget(512, 3), 0);
    }

    #[tokio::test]
    async fn big_logs_keep_the_newest_lines_and_the_marker_inside_the_limit() {
        let dir = std::env::temp_dir().join(format!("fluxdown-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut text = String::new();
        for index in 0..2_000 {
            text.push_str(&format!(
                "2026-11-27T01:{:02}:{:02}.000Z INFO  line-{index} {}\n",
                (index / 60) % 60,
                index % 60,
                "x".repeat(150)
            ));
        }
        std::fs::write(dir.join("agent.log"), text).expect("write log");
        let paths = LogPathsDto {
            agent_log_dir: dir.to_string_lossy().into_owned(),
            ..LogPathsDto::default()
        };
        for limit in [8_000_usize, 24_000, 64 * 1024] {
            let report = read_source(
                LogSource::Agent,
                &paths,
                LogLevel::Info,
                None,
                500,
                total_budget(limit, 1),
            )
            .await;
            let out = super::super::output::finalize_json(
                json!({ "level": "info", "sources": [report] }),
                limit,
            );
            assert!(out.len() <= limit, "{} > {limit}", out.len());
            let parsed: Value = serde_json::from_str(&out).expect("valid JSON");
            assert!(
                parsed.get("truncated").is_none(),
                "must not fall back to the envelope"
            );
            let source = &parsed["sources"][0];
            assert_eq!(source["olderMatchesOmitted"], json!(true));
            let lines = source["lines"].as_array().expect("lines");
            let last = lines.last().and_then(Value::as_str).unwrap_or_default();
            assert!(last.contains("line-1999 "), "newest line kept: {last}");
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn daemon_files_sort_newest_first() {
        let names = vec![
            "fluxdown_2026-11-26.log".to_owned(),
            "fluxdown_2026-11-27.log".to_owned(),
            "fluxdown_2026-11-27.2.log".to_owned(),
            "fluxdown_2026-11-27.10.log".to_owned(),
            "fluxdown_2026-11-27.x.log".to_owned(),
            "agent.log".to_owned(),
            "fluxdown_.log".to_owned(),
        ];
        let order: Vec<String> = daemon_log_order(names)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            order,
            [
                "fluxdown_2026-11-27.10.log",
                "fluxdown_2026-11-27.2.log",
                "fluxdown_2026-11-27.log",
                "fluxdown_2026-11-26.log",
            ]
        );
    }

    #[test]
    fn args_are_validated() {
        assert!(parse_args::<LogArgs>(json!({ "source": "kernel" })).is_err());
        assert!(parse_args::<LogArgs>(json!({ "level": "trace" })).is_err());
        assert!(parse_args::<LogArgs>(json!({ "since": "1h" })).is_err());
        let ok = parse_args::<LogArgs>(json!({ "source": "daemon", "level": "warn", "lines": 10 }))
            .expect("valid");
        assert_eq!(ok.source, Some(LogSource::Daemon));
        assert_eq!(ok.level, Some(LogLevel::Warn));
    }
}
