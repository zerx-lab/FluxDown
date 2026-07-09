//! aria2 JSON-RPC 兼容垫片（`POST /jsonrpc`）。
//!
//! 实现下载相关子集：`aria2.addUri`、`aria2.getVersion`、`aria2.getGlobalStat`、
//! `system.multicall`、`system.listMethods`。让既有「发送到 aria2」类油猴脚本
//! （如网盘直链下载助手 panlinker）与 AriaNg 类客户端把 FluxDown 当 aria2 用。
//!
//! 同时支持**单个请求对象**与**顶层 JSON 数组批量**（gofile-enhanced 等脚本
//! 一次 POST 多个 JSON-RPC 对象的实际行为）。
//!
//! 安全：不校验 `Content-Type`（与真实 aria2 一致，兼容不带 `application/json`
//! 头的 aria2 风格脚本），以「请求体能否解析为合法 JSON-RPC」为准入门槛；
//! 支持 aria2 约定的 `token:xxx`（params[0]）或 `X-FluxDown-Token` 头鉴权。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::auth::constant_time_eq;
use crate::service::ApiHost;
use crate::types::DownloadRequest;

/// 处理一个 `/jsonrpc` 请求体，返回 JSON-RPC 响应（始终 HTTP 200 包裹）。
///
/// `header_token_ok`：`X-FluxDown-Token` 头是否已通过校验（由 HTTP 层判定）；
/// `config_token`：服务端配置的 token（空 = 不鉴权）。
pub(crate) async fn handle_jsonrpc(
    host: &dyn ApiHost,
    config_token: &str,
    header_token_ok: bool,
    body: &[u8],
) -> Value {
    // 准入门槛：请求体须能解析为合法 JSON（解析失败即非 JSON-RPC 客户端，拒绝）。
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return rpc_err(&Value::Null, -32700, &format!("parse error: {e}")),
    };

    match parsed {
        // 顶层数组：逐个处理，返回等长结果数组。
        Value::Array(calls) => {
            let mut out = Vec::with_capacity(calls.len());
            for call in &calls {
                out.push(dispatch_rpc_call(call, host, config_token, header_token_ok).await);
            }
            Value::Array(out)
        }
        obj @ Value::Object(_) => {
            dispatch_rpc_call(&obj, host, config_token, header_token_ok).await
        }
        _ => rpc_err(&Value::Null, -32600, "invalid request"),
    }
}

/// 校验单个 JSON-RPC 调用的 token（服务端配置了 token 时）。
/// 接受 `X-FluxDown-Token` 头（已由 HTTP 层判定）或 aria2 约定的
/// `params[0] = "token:xxx"`。
fn jsonrpc_token_ok(params: &Value, config_token: &str, header_token_ok: bool) -> bool {
    if config_token.is_empty() || header_token_ok {
        return true;
    }
    params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix("token:"))
        .map(|t| constant_time_eq(t, config_token))
        .unwrap_or(false)
}

/// 处理单个 JSON-RPC 调用，返回完整响应对象（含 `id` + `result`/`error`）。
async fn dispatch_rpc_call(
    call: &Value,
    host: &dyn ApiHost,
    config_token: &str,
    header_token_ok: bool,
) -> Value {
    let id = call.get("id").cloned().unwrap_or(Value::Null);
    let method = call.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = call.get("params").cloned().unwrap_or(Value::Array(vec![]));

    if !jsonrpc_token_ok(&params, config_token, header_token_ok) {
        return rpc_err(&id, 1, "Unauthorized: invalid token");
    }

    if method == "system.multicall" {
        return system_multicall(&id, &params, host).await;
    }
    dispatch_method(method, &params, &id, host).await
}

/// 派发单个 aria2 方法（不含 `system.multicall`，避免异步递归）。
async fn dispatch_method(method: &str, params: &Value, id: &Value, host: &dyn ApiHost) -> Value {
    match method {
        "aria2.addUri" => match aria2_add_uri_to_download_request(params) {
            Ok(dl) => {
                let gid = pseudo_gid(&dl.url);
                match host.submit_external(dl).await {
                    Ok(()) => rpc_ok(id, Value::String(gid)),
                    Err(e) => rpc_err(id, 1, &e.to_string()),
                }
            }
            Err(e) => rpc_err(id, -32602, &e),
        },
        // 返回一个真实存在的 aria2 版本号以通过各客户端的连通性/版本检测。
        "aria2.getVersion" => rpc_ok(
            id,
            json!({
                "version": "1.37.0",
                "enabledFeatures": [
                    "Async DNS", "BitTorrent", "Firefox3 Cookie", "GZip",
                    "HTTPS", "Message Digest", "Metalink", "XML-RPC"
                ],
            }),
        ),
        // 不暴露真实统计；返回占位以满足客户端的「连通性探测」。
        "aria2.getGlobalStat" => rpc_ok(
            id,
            json!({
                "downloadSpeed": "0", "uploadSpeed": "0",
                "numActive": "0", "numWaiting": "0", "numStopped": "0",
            }),
        ),
        "system.listMethods" => rpc_ok(
            id,
            json!([
                "aria2.addUri",
                "aria2.getVersion",
                "aria2.getGlobalStat",
                "system.multicall",
                "system.listMethods"
            ]),
        ),
        other => rpc_err(id, -32601, &format!("Method not found: {other}")),
    }
}

/// 实现 `system.multicall`：`params = [ [ {methodName, params}, ... ] ]`。
/// 每个子调用的成功结果按 aria2 约定包裹成单元素数组。
async fn system_multicall(id: &Value, params: &Value, host: &dyn ApiHost) -> Value {
    let calls = match params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_array())
    {
        Some(c) => c,
        None => return rpc_err(id, -32602, "system.multicall expects an array of calls"),
    };

    let mut results = Vec::with_capacity(calls.len());
    for c in calls {
        let method = c.get("methodName").and_then(|m| m.as_str()).unwrap_or("");
        // 禁止嵌套 multicall（aria2 行为一致）。
        if method == "system.multicall" {
            results.push(json!({ "code": -32600, "message": "nested multicall not allowed" }));
            continue;
        }
        let inner_params = c.get("params").cloned().unwrap_or(Value::Array(vec![]));
        let resp = dispatch_method(method, &inner_params, &Value::Null, host).await;
        if let Some(result) = resp.get("result") {
            results.push(json!([result]));
        } else {
            results.push(resp.get("error").cloned().unwrap_or(Value::Null));
        }
    }
    rpc_ok(id, Value::Array(results))
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 由稳定输入派生一个 16 字符十六进制占位 GID（无需随机源）。
///
/// FNV-1a 64-bit hash → 16 hex chars，格式上贴近 aria2 的 GID。
pub(crate) fn pseudo_gid(seed: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in seed.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// 把 `aria2.addUri` 的 params 翻译为 [`DownloadRequest`]。
///
/// `params = [ "token:xxx"?, [uris...], { options }? ]`
///
/// 支持的 options：`dir`→save_dir、`out`→filename、
/// `referer`/`referrer`→referrer、`header`（字符串数组）→Cookie/Referer/其它头。
pub(crate) fn aria2_add_uri_to_download_request(params: &Value) -> Result<DownloadRequest, String> {
    let arr = params.as_array().ok_or("params must be an array")?;

    // 跳过可能存在的 "token:xxx" 前缀参数。
    let mut idx = 0;
    if let Some(first) = arr.first().and_then(|v| v.as_str())
        && first.starts_with("token:")
    {
        idx = 1;
    }

    let uris = arr
        .get(idx)
        .and_then(|v| v.as_array())
        .ok_or("first param (after optional token) must be a uris array")?;
    let joined = uris
        .iter()
        .filter_map(|u| u.as_str())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        return Err("at least one URI is required".to_string());
    }

    let options = arr.get(idx + 1).and_then(|v| v.as_object());

    let mut filename = String::new();
    let mut save_dir = String::new();
    let mut referrer = String::new();
    let mut cookies = String::new();
    let mut extra_headers: HashMap<String, String> = HashMap::new();

    if let Some(opts) = options {
        if let Some(out) = opts.get("out").and_then(|v| v.as_str()) {
            filename = out.to_string();
        }
        if let Some(dir) = opts.get("dir").and_then(|v| v.as_str()) {
            save_dir = dir.to_string();
        }
        if let Some(r) = opts
            .get("referer")
            .or_else(|| opts.get("referrer"))
            .and_then(|v| v.as_str())
        {
            referrer = r.to_string();
        }
        // aria2 的 header 是字符串数组，每项形如 "Name: value"。
        if let Some(headers) = opts.get("header").and_then(|v| v.as_array()) {
            for h in headers.iter().filter_map(|x| x.as_str()) {
                if let Some((name, value)) = h.split_once(':') {
                    let name = name.trim();
                    let value = value.trim();
                    match name.to_ascii_lowercase().as_str() {
                        "cookie" => cookies = value.to_string(),
                        "referer" | "referrer" => {
                            if referrer.is_empty() {
                                referrer = value.to_string();
                            }
                        }
                        _ => {
                            extra_headers.insert(name.to_string(), value.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(DownloadRequest {
        url: joined,
        filename,
        save_dir,
        referrer,
        cookies,
        headers: if extra_headers.is_empty() {
            None
        } else {
            Some(extra_headers)
        },
        file_size: None,
        mime_type: None,
        method: None,
        body: None,
        audio_url: None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn aria2_add_uri_extracts_out_referer_and_cookie_header() {
        let params = serde_json::json!([
            ["https://example.com/file.zip"],
            {
                "out": "renamed.zip",
                "dir": "D:/Downloads/sub",
                "header": ["Cookie: a=b", "Referer: https://example.com/", "User-Agent: UA/1.0"]
            }
        ]);
        let dl = aria2_add_uri_to_download_request(&params).unwrap();
        assert_eq!(dl.url, "https://example.com/file.zip");
        assert_eq!(dl.filename, "renamed.zip");
        assert_eq!(dl.save_dir, "D:/Downloads/sub");
        assert_eq!(dl.cookies, "a=b");
        assert_eq!(dl.referrer, "https://example.com/");
        assert_eq!(dl.headers.unwrap().get("User-Agent").unwrap(), "UA/1.0");
    }

    #[test]
    fn aria2_add_uri_skips_leading_token_param() {
        let params = serde_json::json!(["token:mysecret", ["https://example.com/file.zip"]]);
        let dl = aria2_add_uri_to_download_request(&params).unwrap();
        assert_eq!(dl.url, "https://example.com/file.zip");
    }

    #[test]
    fn aria2_add_uri_referrer_alias_used_when_referer_absent() {
        let params = serde_json::json!([
            ["https://example.com/f.zip"],
            { "referrer": "https://ref.example/" }
        ]);
        let dl = aria2_add_uri_to_download_request(&params).unwrap();
        assert_eq!(dl.referrer, "https://ref.example/");
    }

    #[test]
    fn aria2_add_uri_requires_at_least_one_uri() {
        let params = serde_json::json!([[]]);
        assert!(aria2_add_uri_to_download_request(&params).is_err());
    }

    #[test]
    fn pseudo_gid_is_stable_16_char_hex() {
        let g = pseudo_gid("https://example.com/file.zip");
        assert_eq!(g.len(), 16);
        assert!(g.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(g, pseudo_gid("https://example.com/file.zip"));
        assert_ne!(g, pseudo_gid("https://example.com/other.zip"));
    }
}
