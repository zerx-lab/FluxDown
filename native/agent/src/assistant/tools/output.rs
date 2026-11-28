//! 工具输出管线与展示辅助:回传给云端的文本必须脱敏、去掉用户主目录并限制体积。
//!
//! 管线 = JSON 键名脱敏(密钥类键整值抹掉)→ 日志同款文本规则(URL 账号、令牌类 query、Cookie /
//! Authorization、代理账号、用户目录名)→ 主目录替换为 `~` → 在 UTF-8 字符边界截断并标注。
//! 任务 URL、路径等展示字段另有按字段的最小化函数,尽量在进入管线之前就只留必要信息。

use std::sync::LazyLock;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

/// 回传输出上限(云端上限 512 KiB,这里取更小的值以免撑爆模型上下文)。
pub(super) const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// 脱敏前的预截断上限:避免对异常大的文本跑整套正则。
const MAX_SANITIZE_BYTES: usize = 4 * MAX_OUTPUT_BYTES;
/// 截断标注的预留长度(标注最长约 80 字节)。
const TRUNCATION_RESERVE: usize = 96;
/// 合法 JSON 兜底外壳(固定字段 + 数字)的预留长度。
const ENVELOPE_RESERVE: usize = 256;
/// 列表预算在回传上限之外再留的固定余量:裁剪时追加的计数字段、脱敏改写。
const ITEMS_HEADROOM: usize = 160;
/// 展示用 URL 路径的字符上限。
const MAX_URL_PATH_CHARS: usize = 200;
/// 无法解析的 URL 原样展示时的字符上限。
const MAX_FALLBACK_URL_CHARS: usize = 200;

/// 当前用户主目录;太短(如 `/`)的路径不参与替换,免得把所有路径都改写。
fn home_dir() -> Option<&'static str> {
    static HOME: LazyLock<Option<String>> = LazyLock::new(|| {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().display().to_string())
            .filter(|home| home.len() >= 3)
    });
    HOME.as_deref()
}

/// 把 `text` 里的 `home` 前缀(其后须是路径分隔符或串尾)替换为 `~`。
fn replace_home(text: &str, home: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find(home) {
        let after = &rest[position + home.len()..];
        let at_boundary = match after.chars().next() {
            None => true,
            Some(next) => matches!(next, '/' | '\\'),
        };
        out.push_str(&rest[..position]);
        if at_boundary {
            out.push('~');
        } else {
            out.push_str(home);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// 展示用路径:用户主目录替换为 `~`。
pub(super) fn display_path(path: &str) -> String {
    match home_dir() {
        Some(home) => replace_home(path, home),
        None => path.to_owned(),
    }
}

fn scrub_value(value: &mut Value, home: Option<&str>) {
    match value {
        Value::String(text) => {
            if let Some(home) = home
                && text.contains(home)
            {
                *text = replace_home(text, home);
            }
            // 逐个字符串套用日志文本规则:不把整段 JSON 当文本处理,规则吃掉的引号 / 括号不会改坏结构。
            let cleaned = crate::log_export::sanitize_text(text.as_bytes());
            if cleaned != text.as_bytes() {
                *text = String::from_utf8_lossy(&cleaned).into_owned();
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_value(item, home);
            }
        }
        Value::Object(map) => {
            for child in map.values_mut() {
                scrub_value(child, home);
            }
        }
        _ => {}
    }
}

/// 在 UTF-8 字符边界截断到 `max` 字节(含标注),被截断时在末尾标注保留 / 总字节数。
pub(super) fn truncate_utf8(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let total = text.len();
    let mut end = max.saturating_sub(TRUNCATION_RESERVE);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(&format!(
        "\n[output truncated: kept {end} of {total} bytes]"
    ));
    text
}

/// 在字符边界截断到 `max` 个字符,被截断时以 `…` 结尾。
pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// 纯文本输出(错误消息、日志文本)的收尾:预截断 → 文本脱敏 → 主目录 → 截断到 `max` 字节。
pub(super) fn finalize_text(text: &str, max: usize) -> String {
    finalize_text_with(text, home_dir(), max)
}

fn finalize_text_with(text: &str, home: Option<&str>, max: usize) -> String {
    let mut end = text.len().min(MAX_SANITIZE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cleaned = crate::log_export::sanitize_text(&text.as_bytes()[..end]);
    let cleaned = String::from_utf8_lossy(&cleaned).into_owned();
    let cleaned = match home {
        Some(home) => replace_home(&cleaned, home),
        None => cleaned,
    };
    truncate_utf8(cleaned, max)
}

/// JSON 结果的收尾:键名脱敏 → 逐串文本脱敏 + 主目录 → 紧凑序列化 → 超过 `max` 字节时换成合法 JSON 外壳。
pub(super) fn finalize_json(value: Value, max: usize) -> String {
    finalize_json_with(value, home_dir(), max)
}

fn finalize_json_with(mut value: Value, home: Option<&str>, max: usize) -> String {
    // 先按键名抹掉密钥类值(与日志导出同一份判定),再逐串脱敏与替换主目录,最后紧凑序列化。
    crate::log_export::redact_secret_values(&mut value);
    scrub_value(&mut value, home);
    let text = value.to_string();
    if text.len() <= max {
        text
    } else {
        truncated_envelope(&text, max)
    }
}

/// 序列化后仍超出上限的最后兜底:按字节切坏的 JSON 模型读不了,所以把前缀放进一个合法的 JSON 外壳,
/// 带上保留 / 总字节数。大列表应在更早处按条目数裁剪(见 [`fit_items`]),不该走到这里。
fn truncated_envelope(text: &str, max: usize) -> String {
    let total = text.len();
    let mut end = max.saturating_sub(ENVELOPE_RESERVE).min(total);
    loop {
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = serde_json::json!({
            "truncated": true,
            "note": "output exceeded the size limit; partialJson is only the beginning of the result and is not complete JSON",
            "keptBytes": end,
            "totalBytes": total,
            "partialJson": &text[..end],
        })
        .to_string();
        if candidate.len() <= max || end == 0 {
            return candidate;
        }
        // 每少一个源字节,转义后至少少一个字节:按超出量收缩一次必然更接近上限。
        end = end.saturating_sub(candidate.len() - max);
    }
}

/// 只数字节数的 writer:预算判断不必为每个条目分配字符串。
#[derive(Default)]
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `value` 紧凑序列化后的字节数。
pub(super) fn json_len(value: &Value) -> usize {
    let mut count = ByteCount::default();
    // 计数 writer 不会失败;万一序列化失败按「放不下」处理,而不是当成 0 字节。
    serde_json::to_writer(&mut count, value).map_or(usize::MAX, |()| count.0)
}

/// 列表条目可用的字节预算:回传上限扣掉列表之外的骨架,再留出被裁剪时附加的计数字段与脱敏改写余量。
pub(super) fn items_budget(limit: usize, skeleton: &Value) -> usize {
    limit
        .saturating_sub(json_len(skeleton))
        .saturating_sub(limit / 20 + ITEMS_HEADROOM)
}

/// 按预算保留最长的条目前缀(保序,被丢弃的是排在后面的条目),返回保留的条目与被丢弃的条数。
/// 输出因此永远是完整的 JSON 数组,调用方把丢弃条数写进结果。
pub(super) fn fit_items(mut items: Vec<Value>, budget: usize) -> (Vec<Value>, usize) {
    let mut used = 0_usize;
    let mut keep = items.len();
    for (index, item) in items.iter().enumerate() {
        // 条目自身 + 数组里的分隔逗号。
        used = used.saturating_add(json_len(item)).saturating_add(1);
        if used > budget {
            keep = index;
            break;
        }
    }
    let omitted = items.len() - keep;
    items.truncate(keep);
    (items, omitted)
}

/// 展示用 URL:去 userinfo、fragment、query(只标注有 query),路径过长截断。
///
/// magnet 只留 `xt`(info hash)与 `dn`(名称),tracker 地址可能带 passkey 因此丢弃;
/// 解析不了的串(如 `ed2k://|file|…`)截断后原样返回,最终仍会过一遍文本脱敏。
pub(super) fn display_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let Ok(url) = reqwest::Url::parse(raw) else {
        return truncate_chars(raw, MAX_FALLBACK_URL_CHARS);
    };
    if url.scheme() == "magnet" {
        let mut parts = Vec::new();
        for (key, value) in url.query_pairs() {
            if matches!(key.as_ref(), "xt" | "dn") {
                parts.push(format!("{key}={}", truncate_chars(&value, 120)));
            }
        }
        return format!("magnet:?{}", parts.join("&"));
    }
    let Some(host) = url.host_str() else {
        return format!(
            "{}:{}",
            url.scheme(),
            truncate_chars(url.path(), MAX_URL_PATH_CHARS)
        );
    };
    let mut out = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(&truncate_chars(url.path(), MAX_URL_PATH_CHARS));
    if url.query().is_some() {
        out.push_str("?…");
    }
    out
}

/// 只保留 `scheme://host[:port]`:tracker 的路径里常带 passkey。
pub(super) fn origin_only(raw: &str) -> String {
    let raw = raw.trim();
    match reqwest::Url::parse(raw) {
        Ok(url) => match url.host_str() {
            Some(host) => match url.port() {
                Some(port) => format!("{}://{host}:{port}", url.scheme()),
                None => format!("{}://{host}", url.scheme()),
            },
            None => format!("{}:", url.scheme()),
        },
        Err(_) => "(unparseable address)".to_owned(),
    }
}

/// URL 的主机名;magnet / ed2k 等没有主机的返回 `None`。
pub(super) fn url_host(raw: &str) -> Option<String> {
    reqwest::Url::parse(raw.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}

/// Unix 秒 → RFC 3339 UTC 文本;非正数(未设置)为 `Value::Null`。
pub(super) fn iso_secs(secs: i64) -> Value {
    match u64::try_from(secs) {
        Ok(secs) if secs > 0 => Value::String(fluxdown_logfile::utc_timestamp(
            UNIX_EPOCH + Duration::from_secs(secs),
        )),
        _ => Value::Null,
    }
}

/// Unix 毫秒 → RFC 3339 UTC 文本;非正数为 `Value::Null`。
pub(super) fn iso_millis(millis: i64) -> Value {
    match u64::try_from(millis) {
        Ok(millis) if millis > 0 => Value::String(fluxdown_logfile::utc_timestamp(
            UNIX_EPOCH + Duration::from_millis(millis),
        )),
        _ => Value::Null,
    }
}

/// 已下 / 总量的百分比(一位小数);总量未知为 `None`。
pub(super) fn percent(downloaded: i64, total: i64) -> Option<f64> {
    if total <= 0 {
        return None;
    }
    let ratio = downloaded.clamp(0, total) as f64 / total as f64 * 100.0;
    Some((ratio * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn truncation_stops_on_a_char_boundary_and_stays_within_the_limit() {
        // 每个字符 3 字节:任何预留长度的截断点都可能落在字符中间。
        for max in [200, 201, 202, 203, 1000] {
            let text = "错".repeat(2000);
            let out = truncate_utf8(text, max);
            assert!(out.len() <= max, "len {} > {max}", out.len());
            assert!(out.contains("[output truncated: kept "), "{out}");
            assert!(out.is_char_boundary(out.len()));
        }
    }

    #[test]
    fn short_text_is_returned_untouched() {
        assert_eq!(truncate_utf8("ok".to_owned(), 100), "ok");
        let exact = "x".repeat(100);
        assert_eq!(truncate_utf8(exact.clone(), 100), exact);
    }

    #[test]
    fn multibyte_output_over_the_real_limit_never_panics() {
        let value = json!({ "text": "日志".repeat(MAX_OUTPUT_BYTES) });
        let out = finalize_json_with(value, None, MAX_OUTPUT_BYTES);
        assert!(out.len() <= MAX_OUTPUT_BYTES);
        let parsed: Value = serde_json::from_str(&out).expect("the fallback is valid JSON");
        assert_eq!(parsed["truncated"], true);
        let text = finalize_text_with(&"🙂".repeat(MAX_OUTPUT_BYTES), None, MAX_OUTPUT_BYTES);
        assert!(text.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn oversized_json_always_comes_back_as_valid_json_within_the_limit() {
        // 引号 / 反斜杠 / 控制字符转义后会变长:外壳必须把转义膨胀算进去。
        for max in [600, 1_000, 4_096, MAX_OUTPUT_BYTES] {
            let value = json!({ "lines": vec!["a\"b\\c\n\u{1}日志"; 5_000] });
            let out = finalize_json_with(value, None, max);
            assert!(out.len() <= max, "len {} > {max}", out.len());
            let parsed: Value = serde_json::from_str(&out).expect("valid JSON");
            assert_eq!(parsed["truncated"], true);
            let kept = parsed["keptBytes"].as_u64().expect("kept bytes");
            let total = parsed["totalBytes"].as_u64().expect("total bytes");
            assert!(kept < total);
            assert_eq!(
                parsed["partialJson"].as_str().expect("prefix").len() as u64,
                kept
            );
        }
        let small = finalize_json_with(json!({ "a": 1 }), None, 600);
        assert_eq!(small, r#"{"a":1}"#);
    }

    #[test]
    fn list_items_are_kept_as_a_prefix_within_the_budget() {
        let items: Vec<Value> = (0..50).map(|index| json!({ "n": index })).collect();
        let each = json_len(&items[0]) + 1;
        let (kept, omitted) = fit_items(items.clone(), each * 10);
        assert_eq!((kept.len(), omitted), (10, 40));
        assert_eq!(kept[0], items[0], "the leading items survive");
        assert_eq!(kept[9], items[9]);
        let (all, none) = fit_items(items.clone(), usize::MAX);
        assert_eq!((all.len(), none), (50, 0));
        let (empty, dropped) = fit_items(items, 0);
        assert_eq!((empty.len(), dropped), (0, 50));
    }

    #[test]
    fn items_budget_subtracts_the_skeleton_and_the_headroom() {
        let skeleton = json!({ "matched": 3, "tasks": [] });
        let limit = 24_000;
        let budget = items_budget(limit, &skeleton);
        assert!(budget < limit - json_len(&skeleton));
        assert!(budget >= limit - json_len(&skeleton) - limit / 20 - ITEMS_HEADROOM);
        assert_eq!(items_budget(100, &skeleton), 0, "never underflows");
    }

    #[test]
    fn sanitizer_strips_url_credentials_and_token_queries() {
        let text = finalize_text_with(
            "GET https://u:p@h/x?token=abc failed",
            None,
            MAX_OUTPUT_BYTES,
        );
        assert!(!text.contains("u:p"), "{text}");
        assert!(!text.contains("abc"), "{text}");
        assert!(text.contains("h/x"), "{text}");

        let json = finalize_json_with(
            json!({ "url": "https://u:p@h/x?token=abc", "proxyPassword": "hunter2" }),
            None,
            MAX_OUTPUT_BYTES,
        );
        for leaked in ["u:p", "abc", "hunter2"] {
            assert!(!json.contains(leaked), "{leaked} leaked: {json}");
        }
        assert!(!json.contains('\n'), "output must be compact: {json}");
    }

    #[test]
    fn home_directory_becomes_tilde_only_at_a_path_boundary() {
        assert_eq!(
            replace_home("/Users/alice/Downloads/a.zip", "/Users/alice"),
            "~/Downloads/a.zip"
        );
        assert_eq!(replace_home("/Users/alice", "/Users/alice"), "~");
        assert_eq!(
            replace_home("/Users/alice2/x", "/Users/alice"),
            "/Users/alice2/x"
        );
        assert_eq!(
            replace_home(r"C:\Users\bob\Downloads", r"C:\Users\bob"),
            r"~\Downloads"
        );
        let out = finalize_json_with(
            json!({ "saveDir": "/Users/alice/Downloads", "nested": ["/Users/alice/x"] }),
            Some("/Users/alice"),
            MAX_OUTPUT_BYTES,
        );
        assert!(!out.contains("alice"), "{out}");
        assert!(out.contains("~/Downloads"), "{out}");
    }

    #[test]
    fn display_url_keeps_only_scheme_host_and_path() {
        assert_eq!(
            display_url("https://u:p@h.example/x/y?token=abc#frag"),
            "https://h.example/x/y?…"
        );
        assert_eq!(
            display_url("ftp://user:pw@ftp.example:2121/a.bin"),
            "ftp://ftp.example:2121/a.bin"
        );
        assert_eq!(
            display_url("https://h.example/plain"),
            "https://h.example/plain"
        );
        assert_eq!(display_url(""), "");
    }

    #[test]
    fn display_url_drops_magnet_trackers() {
        let out = display_url(
            "magnet:?xt=urn:btih:abcdef&dn=Some%20Name&tr=https%3A%2F%2Ftracker.example%2Fannounce%2FPASSKEY123",
        );
        assert_eq!(out, "magnet:?xt=urn:btih:abcdef&dn=Some Name");
        assert!(!out.contains("PASSKEY123"));
    }

    #[test]
    fn unparseable_urls_fall_back_to_truncated_text() {
        let out = display_url(&format!("ed2k://|file|{}|1|abc|/", "n".repeat(500)));
        assert!(
            out.chars().count() <= MAX_FALLBACK_URL_CHARS + 1,
            "{}",
            out.len()
        );
    }

    #[test]
    fn tracker_origin_drops_path_and_query() {
        assert_eq!(
            origin_only("https://tracker.example:8443/announce/PASSKEY?x=1"),
            "https://tracker.example:8443"
        );
        assert_eq!(
            origin_only("udp://tracker.example:6969/announce"),
            "udp://tracker.example:6969"
        );
    }

    #[test]
    fn timestamps_and_percentages() {
        assert_eq!(iso_secs(0), Value::Null);
        assert_eq!(iso_secs(-5), Value::Null);
        assert_eq!(iso_secs(86_400), json!("1970-01-02T00:00:00.000Z"));
        assert_eq!(iso_millis(1_500), json!("1970-01-01T00:00:01.500Z"));
        assert_eq!(percent(50, 200), Some(25.0));
        assert_eq!(percent(1, 3), Some(33.3));
        assert_eq!(percent(5, 0), None);
        assert_eq!(percent(500, 100), Some(100.0));
    }

    #[test]
    fn truncate_chars_counts_characters() {
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        assert_eq!(truncate_chars("错误错误", 2), "错误…");
        assert_eq!(truncate_chars("abc", 3), "abc");
    }
}
