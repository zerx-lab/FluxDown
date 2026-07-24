//! aria2 JSON-RPC 兼容垫片（`POST /jsonrpc`）。
//!
//! 覆盖 aria2 官方 36 个方法全集（见 [`crate::aria2::METHOD_NAMES`]）：
//! 能真实映射到 [`ApiHost`] 能力的方法（`addUri`/`addTorrent`/
//! `remove`/`pause`/`tellStatus`/`tellActive`/`tellWaiting`/`tellStopped`/
//! `getFiles`/`getUris`/`getOption`/`getGlobalOption`/`changeGlobalOption`/
//! `getGlobalStat`/`purgeDownloadResult`/`removeDownloadResult`/
//! `getVersion`/`getSessionInfo`/`system.*` 等）给出真实实现；引擎/DB
//! 层面确实没有对应能力的方法（`addMetalink`/`changePosition`/`changeUri`/
//! `shutdown`/`forceShutdown`）明确拒绝，`getPeers`/`getServers`/
//! `saveSession`/`changeOption` 则按 aria2 精神返回退化但合法的结果
//! （空数组 / `"OK"`）。详见 `local://aria2_compat_contract.md`。
//!
//! 纯函数映射层（GID 编解码、status 映射、选项翻译、响应字段拼装）收敛在
//! [`crate::aria2`]；本模块只做「解析 params → 调用映射层 → 调
//! `&dyn ApiHost` → 包装 JSON-RPC 响应」的胶水工作。
//!
//! 同时支持**单个请求对象**与**顶层 JSON 数组批量**（gofile-enhanced 等脚本
//! 一次 POST 多个 JSON-RPC 对象的实际行为）。
//!
//! 安全：不校验 `Content-Type`（与真实 aria2 一致，兼容不带 `application/json`
//! 头的 aria2 风格脚本），以「请求体能否解析为合法 JSON-RPC」为准入门槛；
//! 支持 aria2 约定的 `token:xxx`（params[0]）或 `X-FluxDown-Token` 头鉴权。
//! `system.listMethods`/`system.listNotifications` 不鉴权（对齐 aria2：
//! 这两个方法重写了 `execute()`，从不调用 `authorize()`）。
//!
//! 错误模型：协议层保留 `-32700`（parse error）/`-32600`（invalid request）；
//! 一旦请求被路由到具体方法，**所有业务失败统一 `code: 1`** + aria2 风格
//! 文案（未知方法/鉴权失败/参数错误/GID 不存在等），不使用
//! `-32601`/`-32602` 等标准 JSON-RPC 错误码——这是与旧实现的关键差异，
//! 也是与真实 aria2 行为对齐的核心决策。

use serde_json::{Value, json};

use crate::aria2;
use crate::auth::constant_time_eq;
use crate::service::{ApiError, ApiHost};
use crate::types::{CreateTaskRequest, TaskDto};

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

    // aria2 行为对齐：这两个方法重写了 execute()，从不调用 authorize()，
    // 即使 token 缺失/错误也照常返回名单；但 token 前缀（若存在）仍按约定剥离。
    if method == "system.listMethods" || method == "system.listNotifications" {
        let arr = strip_token_prefix(params.as_array().cloned().unwrap_or_default());
        return dispatch_method(method, &arr, &id, host).await;
    }

    // aria2 行为对齐：`system.multicall` 信封本身不鉴权（token 由每个
    // 子调用各自携带，见 aria2 文档「In multicall, specify the token in
    // each nested method's params」）；宽容起见仍剥离外层 token 前缀。
    if method == "system.multicall" {
        let arr = strip_token_prefix(params.as_array().cloned().unwrap_or_default());
        return system_multicall(&id, &arr, host, config_token, header_token_ok).await;
    }

    if !jsonrpc_token_ok(&params, config_token, header_token_ok) {
        return rpc_err(&id, 1, "Unauthorized");
    }
    let arr = match params.as_array() {
        Some(a) => strip_token_prefix(a.clone()),
        None => return rpc_err(&id, 1, "params must be an array"),
    };
    dispatch_method(method, &arr, &id, host).await
}

/// 剥离 aria2 约定的 `token:xxx` 前缀参数（若第 0 个元素是这样的字符串）。
/// 命中后从数组头部弹出，后续下标整体前移——所有方法的参数解析都按剥离
/// 后的下标进行（`RpcMethod::authorize` 的 `pop_front` 语义，对齐
/// aria2_rpc_methods.md §0.2）。无论是否配置 token 都会剥离。
fn strip_token_prefix(mut arr: Vec<Value>) -> Vec<Value> {
    if matches!(arr.first(), Some(Value::String(s)) if s.starts_with("token:")) {
        arr.remove(0);
    }
    arr
}

/// 派发单个 aria2 方法（不含 `system.multicall`——由 [`dispatch_rpc_call`]
/// 提前拦截，避免异步递归；也避免嵌套 multicall 误入本函数）。
async fn dispatch_method(method: &str, arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    match method {
        // ---- 真实实现：经 &dyn ApiHost -------------------------------
        "aria2.addUri" => add_uri(arr, id, host).await,
        "aria2.addTorrent" => add_torrent(arr, id, host).await,
        "aria2.remove" | "aria2.forceRemove" => remove_download(arr, id, host).await,
        "aria2.pause" | "aria2.forcePause" => pause_download(arr, id, host).await,
        "aria2.unpause" => unpause_download(arr, id, host).await,
        "aria2.pauseAll" | "aria2.forcePauseAll" => unit_result(id, host.pause_all().await),
        "aria2.unpauseAll" => unit_result(id, host.continue_all().await),
        "aria2.tellStatus" => tell_status(arr, id, host).await,
        "aria2.tellActive" => tell_active(arr, id, host).await,
        "aria2.tellWaiting" => tell_waiting_or_stopped(arr, id, host, true).await,
        "aria2.tellStopped" => tell_waiting_or_stopped(arr, id, host, false).await,
        "aria2.getUris" => get_uris(arr, id, host).await,
        "aria2.getFiles" => get_files(arr, id, host).await,
        "aria2.getOption" => get_option(arr, id, host).await,
        "aria2.getGlobalOption" => get_global_option(id, host).await,
        "aria2.changeGlobalOption" => change_global_option(arr, id, host).await,
        "aria2.getGlobalStat" => get_global_stat(id, host).await,
        "aria2.purgeDownloadResult" => purge_download_result(id, host).await,
        "aria2.removeDownloadResult" => remove_download_result(arr, id, host).await,
        "aria2.getVersion" => rpc_ok(
            id,
            json!({ "version": aria2::ARIA2_VERSION, "enabledFeatures": aria2::ENABLED_FEATURES }),
        ),
        "aria2.getSessionInfo" => rpc_ok(id, json!({ "sessionId": aria2::session_id() })),
        "system.listMethods" => rpc_ok(id, json!(aria2::METHOD_NAMES)),
        "system.listNotifications" => rpc_ok(id, json!(aria2::NOTIFICATION_NAMES)),

        // ---- 降级：引擎有近似能力，返回退化但合法的结果 -----------------
        "aria2.getPeers" | "aria2.getServers" => rpc_ok(id, Value::Array(vec![])),
        "aria2.saveSession" | "aria2.changeOption" => rpc_ok(id, Value::String("OK".to_string())),

        // ---- 降级：引擎/DB 层面确无对应能力，明确拒绝 -------------------
        "aria2.addMetalink"
        | "aria2.changePosition"
        | "aria2.changeUri"
        | "aria2.shutdown"
        | "aria2.forceShutdown" => rpc_err(id, 1, &aria2::err_unsupported(method)),

        other => rpc_err(id, 1, &format!("No such method: {other}")),
    }
}

/// 实现 `system.multicall`：`params = [ [ {methodName, params}, ... ] ]`。
/// 每个子调用独立鉴权（token 从各自 params 头部取，aria2 语义）；
/// 成功结果按 aria2 约定包裹成单元素数组；禁止嵌套 multicall。
async fn system_multicall(
    id: &Value,
    params: &[Value],
    host: &dyn ApiHost,
    config_token: &str,
    header_token_ok: bool,
) -> Value {
    let calls = match params.first() {
        None => return rpc_err(id, 1, &aria2::err_missing_param(0)),
        Some(v) => match v.as_array() {
            Some(c) => c,
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(0)),
        },
    };

    let mut results = Vec::with_capacity(calls.len());
    for c in calls {
        let method = c.get("methodName").and_then(|m| m.as_str()).unwrap_or("");
        if method.is_empty() {
            results.push(json!({ "code": 1, "message": "Missing methodName." }));
            continue;
        }
        if method == "system.multicall" {
            results.push(json!({ "code": 1, "message": "Recursive system.multicall forbidden." }));
            continue;
        }
        let inner = c.get("params").cloned().unwrap_or(Value::Array(vec![]));
        // listMethods/listNotifications 与顶层一致免鉴权，其余子调用逐个校验。
        let exempt = method == "system.listMethods" || method == "system.listNotifications";
        if !exempt && !jsonrpc_token_ok(&inner, config_token, header_token_ok) {
            results.push(json!({ "code": 1, "message": "Unauthorized" }));
            continue;
        }
        let inner_params = strip_token_prefix(inner.as_array().cloned().unwrap_or_default());
        let resp = dispatch_method(method, &inner_params, &Value::Null, host).await;
        if let Some(result) = resp.get("result") {
            results.push(json!([result]));
        } else {
            results.push(resp.get("error").cloned().unwrap_or(Value::Null));
        }
    }
    rpc_ok(id, Value::Array(results))
}

// ---------------------------------------------------------------------------
// 每方法实现
// ---------------------------------------------------------------------------

/// `aria2.addUri`：`params = [uris, options?, position?]`。`uris` 取首个
/// 非空条目建任务（其余镜像 URI 被忽略——引擎任务模型只有单一 `url`）；
/// `position`（插入等待队列指定位置）被忽略——等待队列只有 FIFO。
async fn add_uri(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let uris = match arr.first() {
        None => return rpc_err(id, 1, &aria2::err_missing_param(0)),
        Some(v) => match v.as_array() {
            Some(a) => a,
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(0)),
        },
    };
    let url = uris
        .iter()
        .filter_map(|u| u.as_str())
        .map(str::trim)
        .find(|s| !s.is_empty());
    let Some(url) = url else {
        return rpc_err(id, 1, "URI is not provided.");
    };

    let options = arr.get(1).and_then(|v| v.as_object());
    let opts = match aria2::parse_request_options(options) {
        Ok(o) => o,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let req = aria2::build_create_task_request(url.to_string(), None, opts);
    create_task_and_respond(id, host, req).await
}

/// `aria2.addTorrent`：`params = [torrent(base64), uris?, options?, position?]`。
/// `uris`（web seed 列表）与 `position` 被忽略（引擎不支持 web seed /
/// 队列插入位置）；`torrent` 原样透传给宿主（宿主负责 base64 解码）。
async fn add_torrent(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let torrent = match arr.first() {
        None => return rpc_err(id, 1, &aria2::err_missing_param(0)),
        Some(v) => match v.as_str() {
            Some(s) => s,
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(0)),
        },
    };
    if torrent.trim().is_empty() {
        return rpc_err(id, 1, "No Torrent to download.");
    }

    let options = arr.get(2).and_then(|v| v.as_object());
    let opts = match aria2::parse_request_options(options) {
        Ok(o) => o,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let req = aria2::build_create_task_request(String::new(), Some(torrent.to_string()), opts);
    create_task_and_respond(id, host, req).await
}

/// `addUri`/`addTorrent` 共用尾段：建任务、返回 GID。aria2 的 `pause`
/// 选项已映射为 `CreateTaskRequest.start_paused`（建时即暂停原语），
/// 不再有「建后补暂停」的竞态窗口。
async fn create_task_and_respond(id: &Value, host: &dyn ApiHost, req: CreateTaskRequest) -> Value {
    match host.create_task(req).await {
        Ok(task_id) => rpc_ok(id, Value::String(aria2::task_id_to_gid(&task_id))),
        Err(e) => rpc_err(id, 1, &e.to_string()),
    }
}

/// `aria2.remove`/`aria2.forceRemove`：FluxDown 不区分「优雅/强制」停止，
/// 两者等价为 `delete_task(task_id, delete_files=false)`。
async fn remove_download(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let task = match find_task_by_gid(host, gid).await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let canonical = aria2::task_id_to_gid(&task.task_id);
    let result = host.delete_task(&task.task_id, false).await;
    gid_result(id, &canonical, result)
}

/// `aria2.pause`/`aria2.forcePause`：FluxDown 不区分「优雅/强制」暂停。
async fn pause_download(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let task = match find_task_by_gid(host, gid).await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let canonical = aria2::task_id_to_gid(&task.task_id);
    let result = host.pause_task(&task.task_id).await;
    gid_result(id, &canonical, result)
}

/// `aria2.unpause`：单任务恢复。
async fn unpause_download(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let task = match find_task_by_gid(host, gid).await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let canonical = aria2::task_id_to_gid(&task.task_id);
    let result = host.continue_task(&task.task_id).await;
    gid_result(id, &canonical, result)
}

/// `aria2.tellStatus`：`params = [gid, keys?]`。
async fn tell_status(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let keys = aria2::parse_keys(arr.get(1));
    let task = match find_task_by_gid(host, gid).await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e),
    };
    let speed = host
        .live_speeds()
        .await
        .unwrap_or_default()
        .get(&task.task_id)
        .copied()
        .unwrap_or_default();
    let obj = aria2::build_status_object(&task, speed);
    rpc_ok(id, aria2::filter_keys(obj, &keys))
}

/// `aria2.tellActive`：`params = [keys?]`（无 gid、无分页——直接返回全部
/// active 组）。
async fn tell_active(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let keys = aria2::parse_keys(arr.first());
    let tasks = match host.list_tasks().await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e.to_string()),
    };
    let speeds = host.live_speeds().await.unwrap_or_default();
    let out: Vec<Value> = tasks
        .iter()
        .filter(|t| aria2::is_active_status(t.status))
        .map(|t| {
            let speed = speeds.get(&t.task_id).copied().unwrap_or_default();
            aria2::filter_keys(aria2::build_status_object(t, speed), &keys)
        })
        .collect();
    rpc_ok(id, Value::Array(out))
}

/// `aria2.tellWaiting`（`waiting=true`）/`aria2.tellStopped`
/// （`waiting=false`）共用实现：`params = [offset, num, keys?]`。
async fn tell_waiting_or_stopped(
    arr: &[Value],
    id: &Value,
    host: &dyn ApiHost,
    waiting: bool,
) -> Value {
    let offset = match arr.first() {
        None => return rpc_err(id, 1, &aria2::err_missing_param(0)),
        Some(v) => match v.as_i64() {
            Some(n) => n,
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(0)),
        },
    };
    let num = match arr.get(1) {
        None => return rpc_err(id, 1, &aria2::err_missing_param(1)),
        Some(v) => match v.as_i64() {
            Some(n) if n >= 0 => n,
            Some(_) => return rpc_err(id, 1, &aria2::err_integer_ge(1, 0)),
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(1)),
        },
    };
    let keys = aria2::parse_keys(arr.get(2));

    let tasks = match host.list_tasks().await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e.to_string()),
    };
    let bucket: Vec<TaskDto> = tasks
        .into_iter()
        .filter(|t| {
            if waiting {
                aria2::is_waiting_status(t.status)
            } else {
                aria2::is_stopped_status(t.status)
            }
        })
        .collect();
    let speeds = host.live_speeds().await.unwrap_or_default();
    let out: Vec<Value> = aria2::paginate(&bucket, offset, num)
        .into_iter()
        .map(|t| {
            let speed = speeds.get(&t.task_id).copied().unwrap_or_default();
            aria2::filter_keys(aria2::build_status_object(t, speed), &keys)
        })
        .collect();
    rpc_ok(id, Value::Array(out))
}

/// `aria2.getUris`：`params = [gid]`。
async fn get_uris(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    match find_task_by_gid(host, gid).await {
        Ok(task) => rpc_ok(id, aria2::build_uris_array(&task)),
        Err(e) => rpc_err(id, 1, &e),
    }
}

/// `aria2.getFiles`：`params = [gid]`。
async fn get_files(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    match find_task_by_gid(host, gid).await {
        Ok(task) => rpc_ok(id, Value::Array(vec![aria2::build_file_entry(&task)])),
        Err(e) => rpc_err(id, 1, &e),
    }
}

/// `aria2.getOption`：`params = [gid]`（单任务选项快照）。
async fn get_option(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    match find_task_by_gid(host, gid).await {
        Ok(task) => rpc_ok(id, aria2::build_get_option(&task)),
        Err(e) => rpc_err(id, 1, &e),
    }
}

/// `aria2.getGlobalOption`：无参数。
async fn get_global_option(id: &Value, host: &dyn ApiHost) -> Value {
    let config = host.get_config().await.unwrap_or_default();
    rpc_ok(id, aria2::build_global_option(&config))
}

/// `aria2.changeGlobalOption`：`params = [options]`。映射表之外的键
/// 静默忽略；映射结果为空时直接返回 `"OK"`（不打扰宿主）。
async fn change_global_option(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let options = match arr.first() {
        None => return rpc_err(id, 1, &aria2::err_missing_param(0)),
        Some(v) => match v.as_object() {
            Some(o) => o,
            None => return rpc_err(id, 1, &aria2::err_wrong_type_param(0)),
        },
    };
    let changes = match aria2::map_change_global_options(options) {
        Ok(c) => c,
        Err(e) => return rpc_err(id, 1, &e),
    };
    if changes.is_empty() {
        return rpc_ok(id, Value::String("OK".to_string()));
    }
    unit_result(id, host.apply_config(changes).await)
}

/// `aria2.getGlobalStat`：无参数。聚合 `list_tasks` 的状态桶计数与
/// `live_speeds` 的瞬时速率之和。
async fn get_global_stat(id: &Value, host: &dyn ApiHost) -> Value {
    let tasks = match host.list_tasks().await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e.to_string()),
    };
    let speeds = host.live_speeds().await.unwrap_or_default();
    let num_active = tasks
        .iter()
        .filter(|t| aria2::is_active_status(t.status))
        .count();
    let num_waiting = tasks
        .iter()
        .filter(|t| aria2::is_waiting_status(t.status))
        .count();
    let num_stopped = tasks
        .iter()
        .filter(|t| aria2::is_stopped_status(t.status))
        .count();
    let download_speed: i64 = speeds.values().map(|s| s.download_bps).sum();
    let upload_speed: i64 = speeds.values().map(|s| s.upload_bps).sum();
    rpc_ok(
        id,
        json!({
            "downloadSpeed": download_speed.to_string(),
            "uploadSpeed": upload_speed.to_string(),
            "numActive": num_active.to_string(),
            "numWaiting": num_waiting.to_string(),
            "numStopped": num_stopped.to_string(),
            "numStoppedTotal": num_stopped.to_string(),
        }),
    )
}

/// `aria2.purgeDownloadResult`：无参数，清除全部已停止（complete/error）
/// 任务的结果记录（`delete_files=false`，语义等价 SQLite 常驻持久化下的
/// 「清空历史列表项」）。逐条删除尽力而为，恒返回 `"OK"`（对齐 aria2：
/// 该操作在内存态实现里不会失败）。
async fn purge_download_result(id: &Value, host: &dyn ApiHost) -> Value {
    if let Ok(tasks) = host.list_tasks().await {
        for t in tasks.iter().filter(|t| aria2::is_stopped_status(t.status)) {
            let _ = host.delete_task(&t.task_id, false).await;
        }
    }
    rpc_ok(id, Value::String("OK".to_string()))
}

/// `aria2.removeDownloadResult`：`params = [gid]`，仅允许已停止任务。
async fn remove_download_result(arr: &[Value], id: &Value, host: &dyn ApiHost) -> Value {
    let gid = match require_gid(arr, id) {
        Ok(g) => g,
        Err(resp) => return resp,
    };
    let task = match find_task_by_gid(host, gid).await {
        Ok(t) => t,
        Err(e) => return rpc_err(id, 1, &e),
    };
    if !aria2::is_stopped_status(task.status) {
        let canonical = aria2::task_id_to_gid(&task.task_id);
        return rpc_err(
            id,
            1,
            &format!("Could not remove download result of GID#{canonical}"),
        );
    }
    unit_result(id, host.delete_task(&task.task_id, false).await)
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 提取 `arr[0]` 作为必填 gid 字符串参数；缺失/类型错误时返回对应的
/// aria2 风格错误响应（调用方 `match ... { Err(resp) => return resp }`）。
fn require_gid<'a>(arr: &'a [Value], id: &Value) -> Result<&'a str, Value> {
    match arr.first() {
        None => Err(rpc_err(id, 1, &aria2::err_missing_param(0))),
        Some(v) => match v.as_str() {
            Some(s) => Ok(s),
            None => Err(rpc_err(id, 1, &aria2::err_wrong_type_param(0))),
        },
    }
}

/// GID 必填参数校验 + 反查为完整 [`TaskDto`] 的公共逻辑。
async fn find_task_by_gid(host: &dyn ApiHost, gid: &str) -> Result<TaskDto, String> {
    let tasks = host.list_tasks().await.map_err(|e| e.to_string())?;
    aria2::resolve_gid(&tasks, gid).cloned()
}

/// `Result<(), ApiError>` → `"OK"`/`code:1` 响应。
fn unit_result(id: &Value, result: Result<(), ApiError>) -> Value {
    match result {
        Ok(()) => rpc_ok(id, Value::String("OK".to_string())),
        Err(e) => rpc_err(id, 1, &e.to_string()),
    }
}

/// `Result<(), ApiError>` → GID 字符串/`code:1` 响应（`remove`/`pause`/
/// `unpause` 等按 aria2 约定返回被操作任务的 GID）。
fn gid_result(id: &Value, gid: &str, result: Result<(), ApiError>) -> Value {
    match result {
        Ok(()) => rpc_ok(id, Value::String(gid.to_string())),
        Err(e) => rpc_err(id, 1, &e.to_string()),
    }
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::service::{ApiError, LiveSpeed};
    use crate::types::{CreateTaskRequest, QueueDto};

    /// 本模块专用的轻量 `ApiHost`：记录调用、按需注入任务/配置/速率快照。
    /// 与 `tests.rs` 的黑盒 HTTP `MockHost`相互独立（镜像 `mcp.rs` 的
    /// `FakeHost` 风格），用于直接驱动 `dispatch_method`/`handle_jsonrpc`
    /// 做白盒断言，不经过真实 TCP。
    #[derive(Default)]
    struct TestHost {
        tasks: Vec<TaskDto>,
        config: HashMap<String, String>,
        speeds: HashMap<String, LiveSpeed>,
        next_task_id: String,
        created: Mutex<Vec<CreateTaskRequest>>,
        applied_config: Mutex<Vec<HashMap<String, String>>>,
        paused: Mutex<Vec<String>>,
        continued: Mutex<Vec<String>>,
        deleted: Mutex<Vec<(String, bool)>>,
    }

    impl TestHost {
        fn new(next_task_id: &str) -> Self {
            Self {
                next_task_id: next_task_id.to_string(),
                ..Default::default()
            }
        }

        fn with_tasks(mut self, tasks: Vec<TaskDto>) -> Self {
            self.tasks = tasks;
            self
        }

        fn with_config(mut self, config: HashMap<String, String>) -> Self {
            self.config = config;
            self
        }

        fn with_speeds(mut self, speeds: HashMap<String, LiveSpeed>) -> Self {
            self.speeds = speeds;
            self
        }
    }

    #[async_trait]
    impl ApiHost for TestHost {
        async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError> {
            Ok(self.tasks.clone())
        }
        async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError> {
            Ok(self.tasks.iter().find(|t| t.task_id == task_id).cloned())
        }
        async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError> {
            self.created.lock().unwrap().push(req);
            Ok(self.next_task_id.clone())
        }
        async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError> {
            self.deleted
                .lock()
                .unwrap()
                .push((task_id.to_string(), delete_files));
            Ok(())
        }
        async fn pause_task(&self, task_id: &str) -> Result<(), ApiError> {
            self.paused.lock().unwrap().push(task_id.to_string());
            Ok(())
        }
        async fn continue_task(&self, task_id: &str) -> Result<(), ApiError> {
            self.continued.lock().unwrap().push(task_id.to_string());
            Ok(())
        }
        async fn pause_all(&self) -> Result<(), ApiError> {
            Ok(())
        }
        async fn continue_all(&self) -> Result<(), ApiError> {
            Ok(())
        }
        async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError> {
            Ok(vec![])
        }
        async fn submit_external(
            &self,
            _req: crate::types::DownloadRequest,
        ) -> Result<(), ApiError> {
            Ok(())
        }
        async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
            Ok(self.config.clone())
        }
        async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
            self.applied_config.lock().unwrap().push(changes);
            Ok(())
        }
        async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
            Ok(self.speeds.clone())
        }
    }

    fn task(id: &str, status: i32) -> TaskDto {
        TaskDto {
            task_id: id.to_string(),
            url: format!("https://example.com/{id}.zip"),
            file_name: format!("{id}.zip"),
            save_dir: "/dl".to_string(),
            status,
            downloaded_bytes: 0,
            total_bytes: 100,
            error_message: String::new(),
            created_at: "1700000000".to_string(),
            proxy_url: String::new(),
            queue_id: String::new(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            referrer: String::new(),
        }
    }

    async fn call(host: &dyn ApiHost, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        handle_jsonrpc(host, "", false, body.to_string().as_bytes()).await
    }

    // -- addUri / addTorrent -------------------------------------------------

    #[tokio::test]
    async fn add_uri_creates_task_with_mapped_options_and_returns_gid() {
        let host = TestHost::new("550e8400-e29b-41d4-a716-446655440000");
        let resp = call(
            &host,
            "aria2.addUri",
            json!([
                ["", "https://a.com/v.mp4"],
                {
                    "out": "v.mp4", "dir": "D:/dl", "split": "4",
                    "all-proxy": "http://p:1", "user-agent": "UA/1",
                    "checksum": "sha-1=abc", "header": ["Cookie: s=1"]
                }
            ]),
        )
        .await;
        assert_eq!(resp["result"], "550e8400e29b41d4");
        let created = host.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].url, "https://a.com/v.mp4");
        assert_eq!(created[0].file_name, "v.mp4");
        assert_eq!(created[0].save_dir, "D:/dl");
        assert_eq!(created[0].segments, 4);
        assert_eq!(created[0].proxy_url, "http://p:1");
        assert_eq!(created[0].user_agent, "UA/1");
        assert_eq!(created[0].checksum, "sha-1=abc");
        assert_eq!(created[0].cookies, "s=1");
    }

    #[tokio::test]
    async fn add_uri_pause_option_creates_paused_task() {
        let host = TestHost::new("task-xyz");
        let resp = call(
            &host,
            "aria2.addUri",
            json!([["https://a.com/f"], { "pause": "true" }]),
        )
        .await;
        assert!(resp["result"].is_string());
        // `pause` 映射为建时即暂停（start_paused），不再有「建后补暂停」
        // 的第二次调用。
        let created = host.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].start_paused);
        assert!(host.paused.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_uri_empty_uris_is_code_1_uri_not_provided() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.addUri", json!([[]])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "URI is not provided.");
    }

    #[tokio::test]
    async fn add_uri_gid_option_is_rejected() {
        let host = TestHost::default();
        let resp = call(
            &host,
            "aria2.addUri",
            json!([["https://a.com/f"], { "gid": "abc" }]),
        )
        .await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "GID reservation is not supported");
        assert!(host.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn add_torrent_threads_base64_into_torrent_b64() {
        let host = TestHost::new("task-bt");
        let resp = call(
            &host,
            "aria2.addTorrent",
            json!(["YmFzZTY0Ym9keQ==", [], {}]),
        )
        .await;
        assert!(resp["result"].is_string());
        let created = host.created.lock().unwrap();
        assert_eq!(created[0].url, "");
        assert_eq!(created[0].torrent_b64.as_deref(), Some("YmFzZTY0Ym9keQ=="));
    }

    #[tokio::test]
    async fn add_torrent_missing_param_is_code_1() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.addTorrent", json!([])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(
            resp["error"]["message"],
            "The parameter at 0 is required but missing."
        );
    }

    // -- gid 反查 / remove / pause / unpause ----------------------------------

    #[tokio::test]
    async fn remove_pause_unpause_resolve_gid_prefix_and_call_task_id() {
        let host =
            TestHost::default().with_tasks(vec![task("550e8400-e29b-41d4-a716-446655440000", 1)]);
        let resp = call(&host, "aria2.pause", json!(["550e84"])).await;
        assert_eq!(resp["result"], "550e8400e29b41d4");
        assert_eq!(
            host.paused.lock().unwrap().as_slice(),
            ["550e8400-e29b-41d4-a716-446655440000"]
        );

        let resp = call(&host, "aria2.unpause", json!(["550e84"])).await;
        assert_eq!(resp["result"], "550e8400e29b41d4");
        assert_eq!(host.continued.lock().unwrap().len(), 1);

        let resp = call(&host, "aria2.forceRemove", json!(["550e84"])).await;
        assert_eq!(resp["result"], "550e8400e29b41d4");
        assert_eq!(
            host.deleted.lock().unwrap()[0],
            ("550e8400-e29b-41d4-a716-446655440000".to_string(), false)
        );
    }

    #[tokio::test]
    async fn gid_not_found_and_not_unique_error_text() {
        let host = TestHost::default().with_tasks(vec![task("aaaa1111", 1), task("aaaa2222", 1)]);
        let resp = call(&host, "aria2.pause", json!(["zzzz"])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "GID zzzz is not found");

        let resp = call(&host, "aria2.pause", json!(["aaaa"])).await;
        assert_eq!(resp["error"]["message"], "GID aaaa is not unique");
    }

    // -- tellStatus / tellActive / tellWaiting / tellStopped ------------------

    #[tokio::test]
    async fn tell_status_filters_by_keys() {
        let host = TestHost::default().with_tasks(vec![task("1a1a", 1)]);
        let resp = call(
            &host,
            "aria2.tellStatus",
            json!(["1a1a", ["status", "gid"]]),
        )
        .await;
        let result = resp["result"].as_object().unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result["status"], "active");
        assert_eq!(result["gid"], "1a1a");
    }

    #[tokio::test]
    async fn tell_status_uses_live_speed_for_matching_task() {
        let mut speeds = HashMap::new();
        speeds.insert(
            "1a1a".to_string(),
            LiveSpeed {
                download_bps: 555,
                upload_bps: 0,
            },
        );
        let host = TestHost::default()
            .with_tasks(vec![task("1a1a", 1)])
            .with_speeds(speeds);
        let resp = call(&host, "aria2.tellStatus", json!(["1a1a"])).await;
        assert_eq!(resp["result"]["downloadSpeed"], "555");
    }

    #[tokio::test]
    async fn tell_active_returns_only_downloading_and_preparing() {
        let host = TestHost::default().with_tasks(vec![
            task("a", 1),
            task("b", 0),
            task("c", 5),
            task("d", 3),
        ]);
        let resp = call(&host, "aria2.tellActive", json!([])).await;
        let arr = resp["result"].as_array().unwrap();
        let gids: Vec<&str> = arr.iter().map(|v| v["gid"].as_str().unwrap()).collect();
        assert_eq!(gids, ["a", "c"]);
    }

    #[tokio::test]
    async fn tell_waiting_negative_offset_returns_most_recent_first() {
        let tasks = vec![task("w0", 0), task("w1", 0), task("w2", 2), task("w3", 0)];
        let host = TestHost::default().with_tasks(tasks);
        let resp = call(&host, "aria2.tellWaiting", json!([-1, 2])).await;
        let arr = resp["result"].as_array().unwrap();
        let gids: Vec<&str> = arr.iter().map(|v| v["gid"].as_str().unwrap()).collect();
        assert_eq!(gids, ["w3", "w2"]);
        assert_eq!(arr[0]["status"], "waiting");
        assert_eq!(arr[1]["status"], "paused");
    }

    #[tokio::test]
    async fn tell_stopped_positive_offset_paginates_forward() {
        let tasks = vec![task("s0", 3), task("s1", 4), task("s2", 3)];
        let host = TestHost::default().with_tasks(tasks);
        let resp = call(&host, "aria2.tellStopped", json!([1, 10])).await;
        let arr = resp["result"].as_array().unwrap();
        let gids: Vec<&str> = arr.iter().map(|v| v["gid"].as_str().unwrap()).collect();
        assert_eq!(gids, ["s1", "s2"]);
    }

    #[tokio::test]
    async fn tell_waiting_num_zero_returns_empty() {
        let host = TestHost::default().with_tasks(vec![task("w0", 0)]);
        let resp = call(&host, "aria2.tellWaiting", json!([0, 0])).await;
        assert_eq!(resp["result"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn tell_waiting_negative_num_is_integer_ge_error() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.tellWaiting", json!([0, -1])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("greater than or equal to 0")
        );
    }

    // -- getGlobalStat ---------------------------------------------------------

    #[tokio::test]
    async fn get_global_stat_aggregates_counts_and_speeds() {
        let mut speeds = HashMap::new();
        speeds.insert(
            "a".to_string(),
            LiveSpeed {
                download_bps: 100,
                upload_bps: 10,
            },
        );
        speeds.insert(
            "c".to_string(),
            LiveSpeed {
                download_bps: 50,
                upload_bps: 0,
            },
        );
        let host = TestHost::default()
            .with_tasks(vec![
                task("a", 1),
                task("b", 0),
                task("c", 5),
                task("d", 3),
                task("e", 4),
            ])
            .with_speeds(speeds);
        let resp = call(&host, "aria2.getGlobalStat", json!([])).await;
        let r = &resp["result"];
        assert_eq!(r["numActive"], "2");
        assert_eq!(r["numWaiting"], "1");
        assert_eq!(r["numStopped"], "2");
        assert_eq!(r["numStoppedTotal"], "2");
        assert_eq!(r["downloadSpeed"], "150");
        assert_eq!(r["uploadSpeed"], "10");
    }

    // -- changeGlobalOption / getGlobalOption ----------------------------------

    #[tokio::test]
    async fn change_global_option_calls_apply_config_with_mapped_keys() {
        let host = TestHost::default();
        let resp = call(
            &host,
            "aria2.changeGlobalOption",
            json!([{ "max-overall-download-limit": "2M", "split": "8" }]),
        )
        .await;
        assert_eq!(resp["result"], "OK");
        let applied = host.applied_config.lock().unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(
            applied[0].get("speed_limit_bytes").unwrap(),
            &(2 * 1024 * 1024).to_string()
        );
        assert_eq!(applied[0].get("default_segments").unwrap(), "8");
    }

    #[tokio::test]
    async fn change_global_option_unknown_keys_skip_apply_config_call() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.changeGlobalOption", json!([{ "nope": "1" }])).await;
        assert_eq!(resp["result"], "OK");
        assert!(host.applied_config.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_global_option_reports_config_and_static_defaults() {
        let mut config = HashMap::new();
        config.insert("default_save_dir".to_string(), "/dl".to_string());
        let host = TestHost::default().with_config(config);
        let resp = call(&host, "aria2.getGlobalOption", json!([])).await;
        assert_eq!(resp["result"]["dir"], "/dl");
        assert_eq!(resp["result"]["max-connection-per-server"], "1");
    }

    // -- purge / removeDownloadResult ------------------------------------------

    #[tokio::test]
    async fn remove_download_result_rejects_non_stopped_task() {
        let host = TestHost::default().with_tasks(vec![task("1a1a", 1)]);
        let resp = call(&host, "aria2.removeDownloadResult", json!(["1a1a"])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(
            resp["error"]["message"],
            "Could not remove download result of GID#1a1a"
        );
    }

    #[tokio::test]
    async fn remove_download_result_deletes_stopped_task() {
        let host = TestHost::default().with_tasks(vec![task("1a1a", 3)]);
        let resp = call(&host, "aria2.removeDownloadResult", json!(["1a1a"])).await;
        assert_eq!(resp["result"], "OK");
        assert_eq!(host.deleted.lock().unwrap()[0], ("1a1a".to_string(), false));
    }

    #[tokio::test]
    async fn purge_download_result_deletes_all_stopped_tasks_only() {
        let host = TestHost::default().with_tasks(vec![task("a", 3), task("b", 1), task("c", 4)]);
        let resp = call(&host, "aria2.purgeDownloadResult", json!([])).await;
        assert_eq!(resp["result"], "OK");
        let mut deleted: Vec<String> = host
            .deleted
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        deleted.sort();
        assert_eq!(deleted, ["a", "c"]);
    }

    // -- 降级方法 ----------------------------------------------------------------

    #[tokio::test]
    async fn get_peers_and_get_servers_return_empty_array() {
        let host = TestHost::default();
        assert_eq!(
            call(&host, "aria2.getPeers", json!(["t1"])).await["result"],
            json!([])
        );
        assert_eq!(
            call(&host, "aria2.getServers", json!(["t1"])).await["result"],
            json!([])
        );
    }

    #[tokio::test]
    async fn save_session_and_change_option_return_ok() {
        let host = TestHost::default();
        assert_eq!(
            call(&host, "aria2.saveSession", json!([])).await["result"],
            "OK"
        );
        assert_eq!(
            call(&host, "aria2.changeOption", json!(["t1", {}])).await["result"],
            "OK"
        );
    }

    #[tokio::test]
    async fn unsupported_methods_return_code_1_with_clear_message() {
        let host = TestHost::default();
        for method in [
            "aria2.addMetalink",
            "aria2.changePosition",
            "aria2.changeUri",
            "aria2.shutdown",
            "aria2.forceShutdown",
        ] {
            let resp = call(&host, method, json!([])).await;
            assert_eq!(resp["error"]["code"], 1, "{method}");
            assert_eq!(
                resp["error"]["message"],
                format!("{method} is not supported by FluxDown."),
                "{method}"
            );
        }
    }

    // -- getVersion / getSessionInfo / listMethods / listNotifications --------

    #[tokio::test]
    async fn get_version_reports_honest_feature_list() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.getVersion", json!([])).await;
        assert_eq!(resp["result"]["version"], "1.37.0");
        let features = resp["result"]["enabledFeatures"].as_array().unwrap();
        assert!(features.iter().any(|f| f == "BitTorrent"));
        assert!(!features.iter().any(|f| f == "Metalink"));
        assert!(!features.iter().any(|f| f == "XML-RPC"));
    }

    #[tokio::test]
    async fn get_session_info_returns_40_hex_char_id() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.getSessionInfo", json!([])).await;
        let sid = resp["result"]["sessionId"].as_str().unwrap();
        assert_eq!(sid.len(), 40);
        assert!(sid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn list_methods_and_list_notifications_bypass_token_auth() {
        let host = TestHost::default();
        let body =
            json!({ "jsonrpc": "2.0", "id": 1, "method": "system.listMethods", "params": [] });
        // 配置了 token 但请求既无 header 也无 params token —— 正常 aria2
        // 方法本应 Unauthorized，但 listMethods 例外，不受影响。
        let resp = handle_jsonrpc(&host, "secret", false, body.to_string().as_bytes()).await;
        let methods = resp["result"].as_array().unwrap();
        assert_eq!(methods.len(), 36);

        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "system.listNotifications", "params": [] });
        let resp = handle_jsonrpc(&host, "secret", false, body.to_string().as_bytes()).await;
        assert_eq!(resp["result"].as_array().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn every_listed_method_name_has_a_dispatch_branch() {
        // 验收要求：listMethods 自陈与实际分支一致——逐个调用，任何一个
        // 都不应命中 catch-all 的 "No such method"。
        let host = TestHost::default().with_tasks(vec![task("t1", 3)]);
        for method in aria2::METHOD_NAMES {
            if *method == "system.multicall" {
                continue; // multicall 需要 params[0] 是数组，单独覆盖。
            }
            let params = match *method {
                "aria2.addUri" => json!([["https://x/y"]]),
                "aria2.addTorrent" => json!(["dGVzdA=="]),
                "aria2.tellWaiting" | "aria2.tellStopped" => json!([0, 10]),
                "aria2.changeGlobalOption" | "aria2.changeOption" => json!([{}]),
                m if m.starts_with("aria2.")
                    && m != "aria2.getVersion"
                    && m != "aria2.getSessionInfo"
                    && m != "aria2.getGlobalOption"
                    && m != "aria2.getGlobalStat"
                    && m != "aria2.purgeDownloadResult"
                    && m != "aria2.pauseAll"
                    && m != "aria2.forcePauseAll"
                    && m != "aria2.unpauseAll"
                    && m != "aria2.tellActive" =>
                {
                    json!(["t1"])
                }
                _ => json!([]),
            };
            let resp = call(&host, method, params).await;
            let message = resp["error"]["message"].as_str().unwrap_or_default();
            assert!(
                !message.starts_with("No such method"),
                "{method} fell through to catch-all: {message}"
            );
        }
    }

    // -- 未知方法 / 鉴权 / multicall ---------------------------------------------

    #[tokio::test]
    async fn unknown_method_reports_code_1_with_aria2_wording() {
        let host = TestHost::default();
        let resp = call(&host, "aria2.removeXyz", json!([])).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "No such method: aria2.removeXyz");
    }

    #[tokio::test]
    async fn wrong_token_reports_unauthorized_exact_text() {
        let host = TestHost::default();
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "aria2.getVersion",
            "params": ["token:WRONG"]
        });
        let resp = handle_jsonrpc(&host, "S", false, body.to_string().as_bytes()).await;
        assert_eq!(resp["error"]["code"], 1);
        assert_eq!(resp["error"]["message"], "Unauthorized");
    }

    #[tokio::test]
    async fn multicall_wraps_success_and_rejects_nesting_and_missing_name() {
        let host = TestHost::new("task-mc");
        let resp = call(
            &host,
            "system.multicall",
            json!([[
                { "methodName": "aria2.addUri", "params": [["https://a/1.zip"]] },
                { "methodName": "aria2.getVersion", "params": [] },
                { "methodName": "system.multicall", "params": [[]] },
                { "params": [] }
            ]]),
        )
        .await;
        let results = resp["result"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        assert!(results[0].is_array());
        assert!(results[0][0].is_string());
        assert!(results[1][0]["version"].is_string());
        assert_eq!(
            results[2]["message"],
            "Recursive system.multicall forbidden."
        );
        assert_eq!(results[3]["message"], "Missing methodName.");
    }

    /// aria2 语义：multicall 信封本身不鉴权，token 由每个子调用的
    /// params 头部各自携带；缺 token 的子调用单独失败，不拖累整体。
    #[tokio::test]
    async fn multicall_authorizes_each_sub_call_individually() {
        let host = TestHost::new("task-mc-auth");
        let body = json!({
            "jsonrpc": "2.0", "id": 7, "method": "system.multicall",
            "params": [[
                { "methodName": "aria2.getVersion", "params": ["token:S"] },
                { "methodName": "aria2.getVersion", "params": [] },
                { "methodName": "system.listMethods", "params": [] }
            ]]
        });
        let resp = handle_jsonrpc(&host, "S", false, body.to_string().as_bytes()).await;
        let results = resp["result"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert!(
            results[0][0]["version"].is_string(),
            "带 token 的子调用应成功"
        );
        assert_eq!(results[1]["message"], "Unauthorized");
        assert_eq!(
            results[2][0].as_array().unwrap().len(),
            36,
            "listMethods 免鉴权"
        );
    }

    #[tokio::test]
    async fn parse_error_and_invalid_request_keep_protocol_codes() {
        let host = TestHost::default();
        let resp = handle_jsonrpc(&host, "", false, b"not json").await;
        assert_eq!(resp["error"]["code"], -32700);

        let resp = handle_jsonrpc(&host, "", false, b"42").await;
        assert_eq!(resp["error"]["code"], -32600);
    }
}
