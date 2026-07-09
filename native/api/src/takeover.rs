//! 油猴脚本接管端点的请求体解析（`/download`、`/download/batch`）。
//!
//! 端点语义：请求进入宿主的「外部下载」流程（桌面端弹快速下载确认框），
//! 与浏览器扩展 NMH 走完全相同的处理路径。

use std::collections::HashMap;

use serde_json::Value;

use crate::types::DownloadRequest;

/// 解析批量下载请求体。
///
/// 支持两种形态：
/// - `{ "urls": ["u1","u2"], "saveDir": "", "referrer": "", "cookies": "", "headers": {} }`
/// - `{ "items": [ { ...DownloadRequest }, ... ] }`（取各项 url，共享首个非空 saveDir/cookies/referrer）
///
/// 统一合并为**单个** [`DownloadRequest`]，`url` 以换行符连接 —— 与 Dart 快速下载
/// 弹框「按换行拆分批量创建」的既有约定一致，用户只需确认一次。
pub(crate) fn parse_batch(body: &[u8]) -> Result<DownloadRequest, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;

    // 形态 A：urls 数组 + 共享字段
    if let Some(urls) = v.get("urls").and_then(|u| u.as_array()) {
        let joined = urls
            .iter()
            .filter_map(|u| u.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if joined.is_empty() {
            return Err("urls is empty".to_string());
        }
        let headers = v
            .get("headers")
            .and_then(|h| h.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect::<HashMap<String, String>>()
            })
            .filter(|m| !m.is_empty());
        return Ok(DownloadRequest {
            url: joined,
            filename: String::new(),
            save_dir: str_field(&v, "saveDir"),
            referrer: str_field(&v, "referrer"),
            cookies: str_field(&v, "cookies"),
            headers,
            file_size: v.get("fileSize").and_then(|f| f.as_i64()),
            mime_type: None,
            method: None,
            body: None,
            audio_url: None,
        });
    }

    // 形态 B：items 数组（每项是一个 DownloadRequest）
    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
        let parsed: Vec<DownloadRequest> = items
            .iter()
            .filter_map(|item| serde_json::from_value::<DownloadRequest>(item.clone()).ok())
            .collect();
        if parsed.is_empty() {
            return Err("items is empty or invalid".to_string());
        }
        let joined = parsed
            .iter()
            .map(|d| d.url.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if joined.is_empty() {
            return Err("no valid urls in items".to_string());
        }
        // 共享首个非空 cookies / referrer / headers。
        let cookies = parsed
            .iter()
            .map(|d| d.cookies.clone())
            .find(|c| !c.is_empty())
            .unwrap_or_default();
        let referrer = parsed
            .iter()
            .map(|d| d.referrer.clone())
            .find(|r| !r.is_empty())
            .unwrap_or_default();
        let save_dir = parsed
            .iter()
            .map(|d| d.save_dir.clone())
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        let headers = parsed
            .iter()
            .find_map(|d| d.headers.clone().filter(|h| !h.is_empty()));
        return Ok(DownloadRequest {
            url: joined,
            filename: String::new(),
            save_dir,
            referrer,
            cookies,
            headers,
            file_size: None,
            mime_type: None,
            method: None,
            body: None,
            audio_url: None,
        });
    }

    Err("expected `urls` array or `items` array".to_string())
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_batch_urls_form_joins_and_carries_shared_fields() {
        let body = br#"{"urls":["https://a.com/1.zip","https://b.com/2.zip"],"saveDir":"D:/dl","referrer":"https://p.com/","cookies":"s=1"}"#;
        let dl = parse_batch(body).unwrap();
        assert_eq!(dl.url, "https://a.com/1.zip\nhttps://b.com/2.zip");
        assert_eq!(dl.save_dir, "D:/dl");
        assert_eq!(dl.referrer, "https://p.com/");
        assert_eq!(dl.cookies, "s=1");
    }

    #[test]
    fn parse_batch_items_form_uses_first_non_empty_cookies() {
        let body = br#"{"items":[{"url":"https://a.com/1.zip","cookies":"s=1"},{"url":"https://b.com/2.zip","saveDir":"D:/dl"}]}"#;
        let dl = parse_batch(body).unwrap();
        assert_eq!(dl.url, "https://a.com/1.zip\nhttps://b.com/2.zip");
        assert_eq!(dl.cookies, "s=1");
        assert_eq!(dl.save_dir, "D:/dl");
    }

    #[test]
    fn parse_batch_rejects_invalid_input() {
        let cases: &[&[u8]] = &[br#"{"urls":[]}"#, br#"{}"#, b"not json"];
        for body in cases {
            assert!(parse_batch(body).is_err(), "expected error for {body:?}");
        }
    }
}
