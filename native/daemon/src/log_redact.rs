//! 诊断日志导出用的快照脱敏：保留结构与排障所需字段，置换凭据类取值。

use fluxdown_protocol::DaemonSnapshot;

const REDACTED: &str = "<redacted>";

/// 原地脱敏 daemon 快照（调用方持有的是克隆，不影响运行中的投影）。
pub fn redact_snapshot(snapshot: &mut DaemonSnapshot) {
    for key in ["proxy_username", "proxy_password"] {
        if let Some(value) = snapshot.config.values.get_mut(key)
            && !value.is_empty()
        {
            *value = REDACTED.to_owned();
        }
    }
    if let Some(value) = snapshot.config.values.get_mut("proxy_rules") {
        *value = redact_proxy_rules(value);
    }
    if let Some(value) = snapshot.config.values.get_mut("webhook.endpoints") {
        *value = redact_webhook_endpoints(value);
    }
    for task in &mut snapshot.tasks {
        task.url = strip_url(&task.url, true);
        task.referrer = strip_url(&task.referrer, true);
        task.origin_url = strip_url(&task.origin_url, true);
        task.proxy_url = strip_url(&task.proxy_url, false);
    }
    for source in &mut snapshot.rss_sources {
        // RSS 源地址常内嵌 passkey（query），与任务 URL 同样处理。
        source.url = strip_url(&source.url, true);
        if !source.cookies.is_empty() {
            source.cookies = REDACTED.to_owned();
        }
        if !source.provider_config.is_empty() {
            source.provider_config = REDACTED.to_owned();
        }
        source.proxy_url = strip_url(&source.proxy_url, false);
    }
    for delivery in &mut snapshot.webhook_deliveries {
        // Telegram 等预设把 token 放在路径里，只保留 scheme 与 host。
        delivery.url = strip_url(&delivery.url, false);
        delivery.request_body.clear();
        delivery.response_body.clear();
    }
}

/// 站点代理规则：名称与匹配式可能暴露访问习惯，自定义代理地址只保留 `scheme://host:port`。
fn redact_proxy_rules(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return REDACTED.to_owned();
    };
    if let Some(items) = value.as_array_mut() {
        for item in items {
            let Some(map) = item.as_object_mut() else {
                continue;
            };
            for key in ["name", "pattern"] {
                if let Some(entry) = map.get_mut(key)
                    && entry.as_str().is_some_and(|text| !text.is_empty())
                {
                    *entry = serde_json::Value::String(REDACTED.to_owned());
                }
            }
            if let Some(entry) = map.get_mut("proxy")
                && let Some(text) = entry.as_str()
            {
                *entry = serde_json::Value::String(strip_url(text, false));
            }
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| REDACTED.to_owned())
}

/// 去掉 userinfo、query 与 fragment；`keep_path` 为假时路径也一并丢弃（仅留 scheme://host[:port]）。
fn strip_url(url: &str, keep_path: bool) -> String {
    if url.is_empty() {
        return String::new();
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        let end = url.find(['?', '#']).unwrap_or(url.len());
        return url[..end].to_owned();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let mut out = format!("{scheme}://{host}");
    if keep_path {
        let tail = &rest[authority_end..];
        let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
        out.push_str(&tail[..path_end]);
    }
    out
}

/// `webhook.endpoints` 是端点数组 JSON：请求头取值与签名密钥置换，URL 仅留 scheme+host。
/// 无法解析时整体替换，避免泄露未知结构。
fn redact_webhook_endpoints(raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return REDACTED.to_owned();
    };
    redact_endpoint_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| REDACTED.to_owned())
}

fn redact_endpoint_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_endpoint_value),
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                match key.as_str() {
                    "headers" => redact_header_values(entry),
                    "sign_secret" | "signSecret" => {
                        if entry.as_str().is_some_and(|text| !text.is_empty()) {
                            *entry = serde_json::Value::String(REDACTED.to_owned());
                        }
                    }
                    "url" => {
                        if let Some(text) = entry.as_str() {
                            *entry = serde_json::Value::String(strip_url(text, false));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn redact_header_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for entry in map.values_mut() {
                *entry = serde_json::Value::String(REDACTED.to_owned());
            }
        }
        serde_json::Value::Array(items) => {
            // 兼容 `[[name, value], ...]` 或 `[{name, value}]` 形式：只保留头名。
            for item in items {
                match item {
                    serde_json::Value::Array(pair) if pair.len() == 2 => {
                        pair[1] = serde_json::Value::String(REDACTED.to_owned());
                    }
                    serde_json::Value::Object(map) => {
                        if let Some(entry) = map.get_mut("value") {
                            *entry = serde_json::Value::String(REDACTED.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::String(_) => *value = serde_json::Value::String(REDACTED.to_owned()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{redact_proxy_rules, redact_webhook_endpoints, strip_url};

    #[test]
    fn proxy_rules_are_redacted_but_keep_structure() {
        assert_eq!(redact_proxy_rules(""), "");
        assert_eq!(redact_proxy_rules("not json"), "<redacted>");
        let out = redact_proxy_rules(
            r#"[{"id":"a","enabled":true,"name":"GH","match":"domain","pattern":"github.com","action":"custom","proxy":"socks5://u:p@127.0.0.1:7891"},{"id":"b","name":"","pattern":"","action":"direct","proxy":""},3]"#,
        );
        let value: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(value[0]["name"], "<redacted>");
        assert_eq!(value[0]["pattern"], "<redacted>");
        assert_eq!(value[0]["proxy"], "socks5://127.0.0.1:7891");
        assert_eq!(value[0]["id"], "a");
        assert_eq!(value[0]["action"], "custom");
        assert_eq!(value[1]["name"], "");
        assert_eq!(value[1]["proxy"], "");
        assert!(!out.contains("u:p"));
    }

    #[test]
    fn strip_url_removes_userinfo_and_query() {
        assert_eq!(
            strip_url("https://u:p@host:8080/a/b?sig=1#f", true),
            "https://host:8080/a/b"
        );
        assert_eq!(
            strip_url("https://api.telegram.org/bot123:ABC/sendMessage?x=1", false),
            "https://api.telegram.org"
        );
        assert_eq!(strip_url("magnet:?xt=urn:btih:abc", true), "magnet:");
        assert_eq!(
            strip_url("socks5://user:pw@127.0.0.1:1080", false),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(strip_url("", true), "");
    }

    #[test]
    fn webhook_endpoints_keep_structure_but_drop_secrets() {
        let raw = r#"[{"id":"a","url":"https://h.example/bot1:tok/send?k=v","headers":{"Authorization":"Bearer s"},"sign_secret":"hmac","enabled":true}]"#;
        let redacted = redact_webhook_endpoints(raw);
        for secret in ["tok", "Bearer s", "hmac", "k=v"] {
            assert!(!redacted.contains(secret), "{secret} leaked: {redacted}");
        }
        let value: serde_json::Value = serde_json::from_str(&redacted).unwrap_or_default();
        assert_eq!(value[0]["id"], "a");
        assert_eq!(value[0]["enabled"], true);
        assert_eq!(redact_webhook_endpoints("not json"), "<redacted>");
    }
}
