//! 匿名统计（server / NAS / Docker 部署形态）。
//!
//! 隐私边界（硬性约束，与桌面端 `analytics_service.dart` 完全一致）：
//! - 只发送两类事件：`app_installed`（首次部署一次性）与 `app_active`（每日一次）。
//! - **禁止**采集任何与下载任务相关的信息（URL、文件名、大小、协议、速度等）。
//! - `sessionId` 是持久匿名设备 ID（存 config 表，随数据目录/挂载卷同生命周期：
//!   升级镜像保留，删卷重装 = 新 ID = 统计为新安装）。
//! - `app_installed` 不受开关控制；`app_active` 受 Web 设置页「匿名使用统计」
//!   开关（config 键 `analytics_enabled`）控制，每个 tick 实时读库。
//! - 部署级硬关闭：环境变量 `FLUXDOWN_ANALYTICS=off`（NAS/隐私敏感环境一刀切）。
//!
//! 本模块只依赖 `Db` 的通用 config KV 与 reqwest —— 下载引擎 crate 零感知。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxdown_engine::db::Db;
use fluxdown_engine::{log_error, log_info};

/// 编译期注入的 App-Key（CI：`FLUXDOWN_ANALYTICS_APP_KEY`）。空 = 未配置。
const BAKED_APP_KEY: &str = match option_env!("FLUXDOWN_ANALYTICS_APP_KEY") {
    Some(v) => v,
    None => "",
};

/// 统计上报端点。
const ENDPOINT: &str = "https://ops.zerx.dev/api/zerx.v1.AnalyticsIngestService/TrackEvents";

/// config 键：持久匿名设备 ID（UUID v4）。
const K_DEVICE_ID: &str = "analytics_device_id";
/// config 键：首次部署事件是否已上报。
const K_INSTALL_REPORTED: &str = "analytics_install_reported";
/// config 键：上次 `app_active` 上报的 UTC 天序号（days since epoch）。
const K_LAST_ACTIVE_DAY: &str = "analytics_last_active_day";
/// config 键：用户开关（Web 设置页 → PUT /api/v1/config）。
const K_ENABLED: &str = "analytics_enabled";

/// 运行期 App-Key：环境变量覆盖编译期烘焙值（自建部署可自带 key 或留空禁用）。
fn app_key() -> String {
    match std::env::var("FLUXDOWN_ANALYTICS_APP_KEY") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => BAKED_APP_KEY.trim().to_string(),
    }
}

/// 部署级硬关闭：`FLUXDOWN_ANALYTICS=off|0|false`。
fn disabled_by_env() -> bool {
    matches!(
        std::env::var("FLUXDOWN_ANALYTICS").as_deref(),
        Ok("off") | Ok("0") | Ok("false")
    )
}

/// 后台上报循环：启动延迟 10s 后首次评估，此后每小时 tick 一次
/// （跨天时补发 `app_active`，长驻 NAS 进程不依赖重启）。
pub async fn run(db: Db, server_version: &'static str) {
    if disabled_by_env() {
        log_info!("[analytics] disabled by FLUXDOWN_ANALYTICS env");
        return;
    }
    let key = app_key();
    if key.is_empty() {
        log_info!("[analytics] no app key configured, analytics disabled");
        return;
    }

    tokio::time::sleep(Duration::from_secs(10)).await;
    loop {
        report_once(&db, &key, server_version).await;
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// 单轮评估：首装事件（不受开关控制，失败下轮重试）+ 每日活跃（受开关控制）。
async fn report_once(db: &Db, key: &str, server_version: &'static str) {
    let device_id = match ensure_device_id(db).await {
        Some(id) => id,
        None => return, // DB 故障，下轮重试
    };

    // 首次部署事件：成功后永久标记。
    let installed = matches!(
        db.get_config(K_INSTALL_REPORTED).await,
        Ok(Some(v)) if v == "true"
    );
    if !installed
        && track(key, "app_installed", &device_id, server_version).await
        && let Err(e) = db.set_config(K_INSTALL_REPORTED, "true").await
    {
        log_error!("[analytics] persist install flag failed: {}", e);
    }

    // 用户开关（缺省 = 开启，与 init_default_config 种子一致）。
    let enabled = !matches!(
        db.get_config(K_ENABLED).await,
        Ok(Some(v)) if v == "false"
    );
    if !enabled {
        return;
    }

    // 每日活跃：UTC 天序号去重。
    let today = epoch_days().to_string();
    let last = db.get_config(K_LAST_ACTIVE_DAY).await.ok().flatten();
    if last.as_deref() == Some(today.as_str()) {
        return;
    }
    if track(key, "app_active", &device_id, server_version).await
        && let Err(e) = db.set_config(K_LAST_ACTIVE_DAY, &today).await
    {
        log_error!("[analytics] persist active day failed: {}", e);
    }
}

/// 读取或生成持久设备 ID。
async fn ensure_device_id(db: &Db) -> Option<String> {
    match db.get_config(K_DEVICE_ID).await {
        Ok(Some(id)) if !id.is_empty() => return Some(id),
        Ok(_) => {}
        Err(e) => {
            log_error!("[analytics] read device id failed: {}", e);
            return None;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = db.set_config(K_DEVICE_ID, &id).await {
        log_error!("[analytics] persist device id failed: {}", e);
        return None;
    }
    Some(id)
}

/// 发送单个事件。只携带系统级匿名属性与部署形态维度，恒不 panic。
async fn track(key: &str, event_name: &str, device_id: &str, server_version: &'static str) -> bool {
    let payload = serde_json::json!({
        "events": [{
            "sessionId": device_id,
            "eventName": event_name,
            "systemProps": {
                "osName": os_name(),
                "osVersion": std::env::consts::ARCH,
                "appVersion": server_version,
                "locale": "",
                // dev 构建走 ops 的 debug 分流，不污染正式统计
                "isDebug": server_version == "dev",
            },
            "props": {
                "edition": "server",
                "container": in_container(),
            },
        }]
    });

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            log_error!("[analytics] http client build failed: {}", e);
            return false;
        }
    };
    match client
        .post(ENDPOINT)
        .header("Content-Type", "application/json")
        .header("App-Key", key)
        .json(&payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            log_info!("[analytics] {} sent", event_name);
            true
        }
        Ok(resp) => {
            log_info!(
                "[analytics] {} rejected: HTTP {}",
                event_name,
                resp.status()
            );
            false
        }
        Err(e) => {
            // 网络失败静默降级 —— 统计缺失优于影响服务。
            log_info!("[analytics] {} failed: {}", event_name, e);
            false
        }
    }
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

/// Docker/容器检测（NAS 部署形态维度，仅 bool）。
fn in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
}

/// UTC days since Unix epoch（每日去重用，无需日历库）。
fn epoch_days() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}
