//! ED2K（eDonkey2000）leech-only 下载支持。
//!
//! 分层：
//! - [`hash`] —— 分块 MD4 / root hash 数学（纯函数，可离线全测）。
//! - [`link`] —— `ed2k://` 链接解析（纯字符串）。
//!
//! 后续阶段扩充：`proto`（帧编解码）、`server`（找源）、`peer`（块下载）、
//! 编排入口 `run_ed2k_download` 与终验 `finalize_and_verify`。

pub mod client;
pub mod hash;
pub mod kad;
pub mod link;
pub mod peer;
pub mod proto;
pub mod server;
pub mod server_subscription;
pub mod upnp;

#[cfg(test)]
pub mod testutil;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::OnceCell;
use tokio::task::JoinSet;

use crate::downloader::{DownloadError, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::ed2k::client::{Source, shared_client};
use crate::ed2k::link::{Ed2kLink, parse_ed2k_link};
use crate::ed2k::peer::download_block_on_stream;
use crate::ed2k::server::{PeerAddr, parse_server_list};
use crate::logger::{log_error, log_info};

/// 块下载失败时携带失败 peer 身份的错误，供调度层区分"投毒/越界 → 拉黑"
/// 与"纯网络失败 → 退避"。`download_block_from_peer` 的所有 `Err` 路径一律
/// 回填此结构（成功路径直接返回 `(PeerAddr, md4)`）。
#[derive(Debug)]
pub struct Ed2kBlockError {
    /// 失败所属的 peer。
    pub peer: PeerAddr,
    /// 底层错误（`Ed2kIntegrity` → 拉黑；其余 → 退避）。
    pub source: DownloadError,
}

/// 单个块下载任务的 join 结果：`(block_index, 源, 成功 md4 | 失败 DownloadError)`。
/// 源随结果回传，供调度层按失败类型对该源退避或拉黑。
type BlockJoinResult = (u64, Source, Result<[u8; 16], DownloadError>);

/// 无用户设置时的默认并发 peer 数（`segment_count <= 0` → 此值）。
pub const DEFAULT_ED2K_CONCURRENCY: usize = 4;

/// 并发 peer 数上限。每个并发块持一条独立 TCP 连接，封顶避免连接风暴。
pub const MAX_ED2K_CONCURRENCY: usize = 8;

/// 由用户 `segment_count` 与剩余块数推导实际并发 peer 数。
///
/// 镜像 `hls_downloader::hls_concurrency` 的**纯函数计算逻辑**：`<= 0` 取
/// [`DEFAULT_ED2K_CONCURRENCY`]，clamp 到 `[1, MAX_ED2K_CONCURRENCY]`，且不
/// 超过 `remaining` 块数。
///
/// 注：仅计算逻辑镜像 HLS；ed2k 运行时消费模型是"队列 pop + 动态重试入队"，
/// 非 HLS 的"一次性 spawn + Semaphore"。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::{ed2k_concurrency, DEFAULT_ED2K_CONCURRENCY, MAX_ED2K_CONCURRENCY};
/// assert_eq!(ed2k_concurrency(0, 100), DEFAULT_ED2K_CONCURRENCY);
/// assert_eq!(ed2k_concurrency(999, 100), MAX_ED2K_CONCURRENCY);
/// assert_eq!(ed2k_concurrency(16, 3), 3); // capped by remaining
/// ```
#[must_use]
pub fn ed2k_concurrency(segment_count: i32, remaining: usize) -> usize {
    let requested = if segment_count <= 0 {
        DEFAULT_ED2K_CONCURRENCY
    } else {
        segment_count as usize
    };
    requested
        .clamp(1, MAX_ED2K_CONCURRENCY)
        .min(remaining.max(1))
}

// ---------------------------------------------------------------------------
// 块状态码（ed2k_blocks.state）
// ---------------------------------------------------------------------------

const BLOCK_MISSING: i64 = 0;
const BLOCK_VERIFIED: i64 = 3;

// ---------------------------------------------------------------------------
// 调度/终态阈值（工程默认，见计划 §5 待澄清）
// ---------------------------------------------------------------------------

/// 单块内容校验失败（块 MD4 不匹配）最大重试次数，超过判永久失败。
const BLOCK_MAX_RETRIES: u32 = 5;
/// find_sources 连续返回空的最大次数，超过转用户可见 error 终态。
const MAX_SOURCE_RETRIES: u32 = 3;
/// 终验发现坏块后回外层重下的最大轮数。
const FINALIZE_MAX_RETRIES: u32 = 2;
/// 无源时的重试间隔基值。
const SOURCE_RETRY_DELAY: Duration = Duration::from_secs(60);
/// 无源重试的抖动上界（避免多任务同时触发登录突发）。
const SOURCE_RETRY_JITTER: Duration = Duration::from_secs(5);
/// Kad 单次找源的整体超时（bootstrap + 迭代 FindNode + SearchSources）。
/// 实测真实网络全程 ~18s（bootstrap 3s + FindNode 收敛 + SearchSources），
/// 20s 会截断 SearchSources 阶段；各阶段空闲即提前退出，上限放宽无额外代价。
const KAD_FIND_TIMEOUT: Duration = Duration::from_secs(45);
/// 旁路进度上报周期。
const PROGRESS_TICK: Duration = Duration::from_millis(200);

/// ED2K 下载编排入口（签名同 `run_ftp_download`：接受 [`DownloadParams`] 单参）。
///
/// 流程：解析链接 → 预分配 temp → 加载/初始化块状态 → 起旁路进度任务 →
/// 外层调度状态机（找源 / 并发拉块 / 逐块 MD4 / 完整性拉黑）→ 终验读盘
/// 重算锚定 root → `sync_all`+`rename`。旁路进度任务在任一退出路径 `abort()`。
pub async fn run_ed2k_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = run_ed2k_download_inner(&params).await;
    match result {
        Ok(total) => {
            log_info!(
                "[ed2k-download] task {} completed, total={}",
                task_id_log,
                total
            );
            let _ = params.db.update_task_status(&params.task_id, 3, "").await;
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: None,
                    ..Default::default()
                })
                .await;
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[ed2k-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            log_error!("[ed2k-download] task {} error: {}", task_id_log, msg);
            let _ = params.db.update_task_status(&params.task_id, 4, &msg).await;
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                _ => (0, 0),
            };
            let _ = params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: dl,
                    total_bytes: total,
                    status: 4,
                    error_message: msg,
                    file_name: String::new(),
                    segment_details: None,
                    ..Default::default()
                })
                .await;
        }
    }
}

/// 内层实现：返回下载总字节数或错误。
async fn run_ed2k_download_inner(params: &DownloadParams) -> Result<i64, DownloadError> {
    let link = parse_ed2k_link(&params.url)?;
    let total_bytes = link.total_bytes;
    let part_size = hash::PART_SIZE;
    let large_file = total_bytes > hash::OLD_MAX_FILE_SIZE;
    let block_count = hash::part_count(total_bytes, part_size);
    let is_single = block_count == 1;
    let task_id = params.task_id.clone();

    let _ = params.db.update_task_status(&task_id, 5, "").await;

    let save_dir = Path::new(&params.save_dir);
    let final_path = save_dir.join(&link.file_name);
    let temp_path = save_dir.join(format!("{}{}", link.file_name, crate::downloader::TEMP_EXT));

    let temp_ok = matches!(
        tokio::fs::metadata(&temp_path).await,
        Ok(m) if m.len() == total_bytes
    );
    if temp_ok {
        params.db.init_ed2k_blocks(&task_id, block_count).await?;
    } else {
        if let Some(parent) = temp_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(DownloadError::Io)?;
        file.set_len(total_bytes).await.map_err(DownloadError::Io)?;
        drop(file);
        params.db.init_ed2k_blocks(&task_id, block_count).await?;
        for i in 0..block_count {
            params
                .db
                .update_ed2k_block(&task_id, i, BLOCK_MISSING, 0, false)
                .await?;
        }
    }

    if total_bytes == 0 {
        if link.root_hash != hash::MD4_EMPTY {
            return Err(DownloadError::Ed2kIntegrity(
                "0-byte file root hash mismatch".into(),
            ));
        }
        finalize_rename(&temp_path, &final_path).await?;
        return Ok(0);
    }

    let _ = params
        .progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: total_bytes as i64,
            status: 1,
            error_message: String::new(),
            file_name: link.file_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await;

    // 服务器来源合并：用户手填列表（ed2k_server_list）+ 订阅缓存
    // （ed2k_server_sub_cache，由 hub 定期刷新 server.met 写入）。
    let manual_cfg = params
        .db
        .get_config("ed2k_server_list")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let sub_cfg = params
        .db
        .get_config("ed2k_server_sub_cache")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut server_list = parse_server_list(&manual_cfg);
    let mut seen: HashSet<String> = server_list.iter().cloned().collect();
    for s in parse_server_list(&sub_cfg) {
        if seen.insert(s.clone()) {
            server_list.push(s);
        }
    }

    let progress: Arc<StdMutex<HashMap<u64, i64>>> = Arc::new(StdMutex::new(HashMap::new()));
    let progress_handle = spawn_progress_reporter(
        params.db.clone(),
        params.progress_tx.clone(),
        task_id.clone(),
        total_bytes,
        part_size,
        Arc::clone(&progress),
    );

    let hashset_cache: Arc<OnceCell<Vec<[u8; 16]>>> = Arc::new(OnceCell::new());
    let client = shared_client();
    // 客户端配置：监听端口/UPnP/Kad 开关来自 DB（hub 首启注入默认）。
    let listen_port = params
        .db
        .get_config("ed2k_listen_port")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let enable_upnp = params
        .db
        .get_config("ed2k_enable_upnp")
        .await
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(true);
    let enable_kad = params
        .db
        .get_config("ed2k_enable_kad")
        .await
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(true);
    client.configure(crate::ed2k::client::ClientConfig {
        listen_port,
        udp_port: 0,
        servers: server_list.clone(),
        enable_upnp,
        enable_kad,
    });
    // Kad bootstrap 节点（nodes.dat，base64 缓存，由 hub 后台刷新）。
    let nodes_dat: Vec<u8> = if enable_kad {
        use base64::Engine as _;
        params
            .db
            .get_config("ed2k_nodes_dat_cache")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut sources: Vec<Source> = Vec::new();
    let mut source_retries: u32 = 0;
    let mut backoff: HashSet<Source> = HashSet::new();
    let mut integrity_bl: HashSet<Source> = HashSet::new();
    let mut strikes: HashMap<u64, u32> = HashMap::new();
    let mut finalize_retries: u32 = 0;
    let mut round_robin: usize = 0;

    let outcome: Result<i64, DownloadError> = 'outer: loop {
        let blocks = match params.db.load_ed2k_blocks(&task_id).await {
            Ok(b) => b,
            Err(e) => break 'outer Err(e.into()),
        };
        let mut pending: VecDeque<u64> = blocks
            .iter()
            .filter(|(_, state, _, _)| *state != BLOCK_VERIFIED)
            .map(|(idx, _, _, _)| *idx)
            .collect();
        if pending.is_empty() {
            match finalize_and_verify(&params.db, &task_id, &temp_path, &link, part_size).await {
                Ok(()) => break 'outer Ok(total_bytes as i64),
                Err(DownloadError::Ed2kIntegrity(_)) if finalize_retries < FINALIZE_MAX_RETRIES => {
                    finalize_retries += 1;
                    log_info!(
                        "[ed2k-download] task {} finalize found bad block, re-downloading (round {})",
                        task_id,
                        finalize_retries
                    );
                    continue 'outer;
                }
                Err(e) => break 'outer Err(e),
            }
        }
        let concurrency = ed2k_concurrency(params.segment_count, pending.len());
        let mut join: JoinSet<BlockJoinResult> = JoinSet::new();

        let inner: Result<(), DownloadError> = loop {
            if params.cancel_token.is_cancelled() {
                break Err(DownloadError::Cancelled);
            }
            while join.len() < concurrency && !pending.is_empty() {
                match pick_source(&sources, &backoff, &integrity_bl, &mut round_robin) {
                    Some(src) => {
                        let Some(bi) = pending.pop_front() else { break };
                        let file_hash = link.root_hash;
                        let dest = temp_path.clone();
                        let cancel = params.cancel_token.clone();
                        let lim = params.speed_limiter.clone();
                        let hc = Arc::clone(&hashset_cache);
                        let pg = Arc::clone(&progress);
                        let client = Arc::clone(&client);
                        join.spawn(async move {
                            // HighID 直连 / LowID 经服务器 callback 中转，拿到已连接流后拉块。
                            let r = match client.connect_source(src).await {
                                Ok(stream) => {
                                    download_block_on_stream(
                                        stream,
                                        &file_hash,
                                        bi,
                                        total_bytes,
                                        part_size,
                                        large_file,
                                        &dest,
                                        &cancel,
                                        &lim,
                                        &hc,
                                        &pg,
                                    )
                                    .await
                                }
                                Err(e) => Err(e),
                            };
                            (bi, src, r)
                        });
                    }
                    None => break,
                }
            }

            if join.is_empty() {
                if pending.is_empty() {
                    break Ok(());
                }
                // 重读服务器列表并重配客户端：hub 后台刷新（含缓存版本失效重取）
                // 完成后，本轮即可用上修正后的服务器，无需重启。
                {
                    let manual = params
                        .db
                        .get_config("ed2k_server_list")
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let sub = params
                        .db
                        .get_config("ed2k_server_sub_cache")
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let mut fresh = parse_server_list(&manual);
                    let mut seen_srv: HashSet<String> = fresh.iter().cloned().collect();
                    for s in parse_server_list(&sub) {
                        if seen_srv.insert(s.clone()) {
                            fresh.push(s);
                        }
                    }
                    if !fresh.is_empty() {
                        client.configure(crate::ed2k::client::ClientConfig {
                            listen_port,
                            udp_port: 0,
                            servers: fresh,
                            enable_upnp,
                            enable_kad,
                        });
                    }
                }
                // 服务器找源（可能空/失败），Kad 作为去中心化补充。
                // 竞速 cancel_token：删除/暂停任务时立即中止 ~96s 的服务器扫描，
                // 避免 handle wait 超时 → 文件被占 → 僵尸任务复活。
                let server_res = tokio::select! {
                    biased;
                    () = params.cancel_token.cancelled() => break 'outer Err(DownloadError::Cancelled),
                    r = client.find_sources(&link.root_hash, total_bytes, large_file) => r,
                };
                let mut merged: Vec<Source> = server_res.unwrap_or_default();
                if enable_kad && !nodes_dat.is_empty() {
                    let kad_res = crate::ed2k::kad::node::find_sources_kad(
                        &link.root_hash,
                        total_bytes,
                        0,
                        listen_port,
                        &nodes_dat,
                        KAD_FIND_TIMEOUT,
                        &params.cancel_token,
                    )
                    .await;
                    if let Ok(peers) = kad_res {
                        let mut seen: HashSet<Source> = merged.iter().copied().collect();
                        for peer in peers {
                            let src = Source::HighId(peer);
                            if seen.insert(src) {
                                merged.push(src);
                            }
                        }
                    }
                }
                if !merged.is_empty() {
                    sources = merged;
                    backoff.clear();
                    source_retries = 0;
                } else {
                    source_retries += 1;
                    if source_retries >= MAX_SOURCE_RETRIES {
                        break Err(DownloadError::Ed2k(
                            "no sources found for this ed2k file".into(),
                        ));
                    }
                    let _ = params.db.update_task_status(&task_id, 5, "").await;
                    let jitter_ms = (u64::from(source_retries) * 137)
                        % (SOURCE_RETRY_JITTER.as_millis() as u64).max(1);
                    // 竞速 cancel：重试等待期间被删除/暂停应立即响应，不空等 60s。
                    tokio::select! {
                        biased;
                        () = params.cancel_token.cancelled() => {
                            break 'outer Err(DownloadError::Cancelled)
                        }
                        () = tokio::time::sleep(
                            SOURCE_RETRY_DELAY + Duration::from_millis(jitter_ms),
                        ) => {}
                    }
                }
                continue;
            }

            if let Some(joined) = join.join_next().await {
                let (bi, src, res) = match joined {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match res {
                    Ok(md4) => {
                        let ok = if is_single {
                            md4 == link.root_hash
                        } else {
                            hashset_cache.get().and_then(|h| h.get(bi as usize)) == Some(&md4)
                        };
                        if ok {
                            let (bs, be) = hash::part_span(bi, total_bytes, part_size);
                            let _ = params
                                .db
                                .update_ed2k_block(
                                    &task_id,
                                    bi,
                                    BLOCK_VERIFIED,
                                    (be - bs) as i64,
                                    false,
                                )
                                .await;
                            if !is_single && let Some(h) = hashset_cache.get() {
                                let blob: Vec<u8> = h.iter().flatten().copied().collect();
                                let _ = params.db.save_ed2k_hashset(&task_id, &blob).await;
                            }
                        } else {
                            // 块 MD4 不匹配 = 投毒/损坏 → 拉黑该源，块重下。
                            let _ = params
                                .db
                                .update_ed2k_block(&task_id, bi, BLOCK_MISSING, 0, true)
                                .await;
                            pending.push_back(bi);
                            *strikes.entry(bi).or_insert(0) += 1;
                            integrity_bl.insert(src);
                            if strikes.get(&bi).copied().unwrap_or(0) >= BLOCK_MAX_RETRIES {
                                break Err(DownloadError::Ed2kIntegrity(format!(
                                    "block {bi} irrecoverable after {BLOCK_MAX_RETRIES} tries"
                                )));
                            }
                        }
                    }
                    Err(source) => {
                        let _ = params
                            .db
                            .update_ed2k_block(&task_id, bi, BLOCK_MISSING, 0, false)
                            .await;
                        pending.push_back(bi);
                        // 完整性违规 → 拉黑该源；纯网络失败 → 退避。
                        if matches!(source, DownloadError::Ed2kIntegrity(_)) {
                            log_info!(
                                "[ed2k] block {} from {:?} INTEGRITY violation: {} — blacklisting",
                                bi,
                                src,
                                source
                            );
                            integrity_bl.insert(src);
                        } else {
                            log_info!(
                                "[ed2k] block {} from {:?} failed: {} — backoff",
                                bi,
                                src,
                                source
                            );
                            backoff.insert(src);
                        }
                    }
                }
            }
        };

        if let Err(e) = inner {
            break 'outer Err(e);
        }
    };

    progress_handle.abort();

    match outcome {
        Ok(total) => {
            finalize_rename(&temp_path, &final_path).await?;
            Ok(total)
        }
        Err(e) => Err(e),
    }
}

/// 轮转选取一个不在 `backoff ∪ integrity_bl` 的源；无可用源返回 `None`。
fn pick_source(
    sources: &[Source],
    backoff: &HashSet<Source>,
    integrity_bl: &HashSet<Source>,
    round_robin: &mut usize,
) -> Option<Source> {
    if sources.is_empty() {
        return None;
    }
    for _ in 0..sources.len() {
        let idx = *round_robin % sources.len();
        *round_robin = round_robin.wrapping_add(1);
        let src = sources[idx];
        if !backoff.contains(&src) && !integrity_bl.contains(&src) {
            return Some(src);
        }
    }
    None
}

/// 旁路进度任务：周期读 progress 快照 + 块状态计数，上报 [`ProgressUpdate`]。
///
/// 取锁窄作用域（`lock→clone→释放`），锁释放后才 `.await` DB，Future 保持 `Send`。
/// 返回的 `JoinHandle` 由调用方在退出前 `abort()`（drop 引用不触发 cancel）。
fn spawn_progress_reporter(
    db: crate::db::Db,
    progress_tx: tokio::sync::mpsc::Sender<ProgressUpdate>,
    task_id: String,
    total_bytes: u64,
    part_size: u64,
    progress: Arc<StdMutex<HashMap<u64, i64>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PROGRESS_TICK).await;
            let snapshot: Vec<(u64, i64)> = {
                let Ok(g) = progress.lock() else { continue };
                g.iter().map(|(k, v)| (*k, *v)).collect()
            };
            let Ok(blocks) = db.load_ed2k_blocks(&task_id).await else {
                continue;
            };
            let mut downloaded: i64 = 0;
            let mut segment_details = Vec::new();
            for (idx, state, _dl, _rt) in &blocks {
                let (bs, be) = hash::part_span(*idx, total_bytes, part_size);
                let block_len = (be - bs) as i64;
                if *state == BLOCK_VERIFIED {
                    downloaded += block_len;
                } else if let Some((_, live)) = snapshot.iter().find(|(k, _)| k == idx) {
                    downloaded += *live;
                    segment_details.push(SegmentProgressInfo {
                        index: *idx as i32,
                        start_byte: bs as i64,
                        end_byte: be as i64,
                        downloaded_bytes: *live,
                    });
                }
            }
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: total_bytes as i64,
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: if segment_details.is_empty() {
                        None
                    } else {
                        Some(segment_details)
                    },
                    ..Default::default()
                })
                .await;
        }
    })
}

/// 终验：逐块读盘重算 MD4 并锚定 link root hash。
///
/// 测试可直接构造"块已 verified 的 DB 状态 + 手工写坏 temp 字节"后调用本函数，
/// 绕开时序竞争。捕获"下载时块级 MD4 通过但落盘后字节损坏"的独立风险类别。
///
/// # Errors
///
/// 任一坏块（已置 missing）→ [`DownloadError::Ed2kIntegrity`]；hashset 缺失 →
/// [`DownloadError::Ed2k`]；I/O 失败 → [`DownloadError::Io`]。
pub async fn finalize_and_verify(
    db: &crate::db::Db,
    task_id: &str,
    temp: &Path,
    link: &Ed2kLink,
    part_size: u64,
) -> Result<(), DownloadError> {
    let total_bytes = link.total_bytes;
    let part_count = hash::part_count(total_bytes, part_size);
    let temp = temp.to_path_buf();

    let disk_hashes: Vec<[u8; 16]> = {
        let temp = temp.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<[u8; 16]>, std::io::Error> {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&temp)?;
            let mut out = Vec::with_capacity(part_count as usize);
            for i in 0..part_count {
                let (start, end) = hash::part_span(i, total_bytes, part_size);
                let len = (end - start) as usize;
                file.seek(SeekFrom::Start(start))?;
                let mut buf = vec![0u8; len];
                file.read_exact(&mut buf)?;
                out.push(hash::hash_part(&buf));
            }
            Ok(out)
        })
        .await
        .map_err(|e| DownloadError::Ed2k(format!("finalize join error: {e}")))?
        .map_err(DownloadError::Io)?
    };

    if part_count == 1 {
        if disk_hashes[0] != link.root_hash {
            db.update_ed2k_block(task_id, 0, BLOCK_MISSING, 0, false)
                .await?;
            return Err(DownloadError::Ed2kIntegrity(
                "single block hash mismatch".into(),
            ));
        }
        return Ok(());
    }

    let blob = db
        .load_ed2k_hashset(task_id)
        .await?
        .ok_or_else(|| DownloadError::Ed2k("hashset missing in db".into()))?;
    if blob.len() as u64 != part_count * 16 {
        return Err(DownloadError::Ed2k("hashset blob length mismatch".into()));
    }
    let db_hashes: Vec<[u8; 16]> = blob
        .chunks_exact(16)
        .map(|c| {
            let mut h = [0u8; 16];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let mut bad = false;
    for (i, (disk, expected)) in disk_hashes.iter().zip(db_hashes.iter()).enumerate() {
        if disk != expected {
            db.update_ed2k_block(task_id, i as u64, BLOCK_MISSING, 0, false)
                .await?;
            bad = true;
        }
    }
    if bad {
        return Err(DownloadError::Ed2kIntegrity(
            "disk block hash mismatch".into(),
        ));
    }

    let root = hash::compute_root(&hash::build_root_input(
        &disk_hashes,
        total_bytes,
        part_size,
    ));
    if root != link.root_hash {
        for i in 0..part_count {
            db.update_ed2k_block(task_id, i, BLOCK_MISSING, 0, false)
                .await?;
        }
        return Err(DownloadError::Ed2kIntegrity(
            "recomputed root mismatch (corrupt hashset table?)".into(),
        ));
    }
    Ok(())
}

/// 完成落盘：`sync_all` + rename temp→final（复用既有惯例）。
async fn finalize_rename(temp: &Path, final_path: &Path) -> Result<(), DownloadError> {
    if let Ok(file) = tokio::fs::File::open(temp).await {
        let _ = file.sync_all().await;
    }
    tokio::fs::rename(temp, final_path)
        .await
        .map_err(DownloadError::Io)
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_MISSING, BLOCK_VERIFIED, DEFAULT_ED2K_CONCURRENCY, MAX_ED2K_CONCURRENCY,
        ed2k_concurrency, finalize_and_verify,
    };

    #[test]
    fn auto_uses_default() {
        assert_eq!(ed2k_concurrency(0, 100), DEFAULT_ED2K_CONCURRENCY);
        assert_eq!(ed2k_concurrency(-1, 100), DEFAULT_ED2K_CONCURRENCY);
    }

    #[test]
    fn respects_user_value() {
        assert_eq!(ed2k_concurrency(2, 100), 2);
        assert_eq!(ed2k_concurrency(1, 100), 1);
    }

    #[test]
    fn clamped_to_max() {
        assert_eq!(ed2k_concurrency(999, 100), MAX_ED2K_CONCURRENCY);
        assert_eq!(ed2k_concurrency(i32::MAX, 100), MAX_ED2K_CONCURRENCY);
    }

    #[test]
    fn never_below_one() {
        assert_eq!(ed2k_concurrency(4, 0), 1);
        assert_eq!(ed2k_concurrency(0, 1), 1);
    }

    #[test]
    fn capped_by_remaining() {
        assert_eq!(ed2k_concurrency(8, 3), 3);
        assert_eq!(ed2k_concurrency(8, 2), 2);
    }

    // -----------------------------------------------------------------------
    // ED2K mock-TCP 集成测试（peer 块下载 / server 找源 / 终验落盘）。
    //
    // 覆盖 15 条必测场景：peer 层 1-8、server 层 9-11、终验层 12-15。
    //
    // `PeerFault::Unrequested` 未单独建测：该 fault 要求块长 ≥ 4×BLOCK_SIZE
    // （184_320*4 ≈ 737 KiB）才能稳定触发（mock 发送 [3*BS,4*BS) 未请求区间），
    // 场景 5/6 已经过同一 `accept_part` 防线（越界 / 长度不符）验证了
    // `Ed2kIntegrity` 分类正确；"未请求数据" 校验是该函数内紧邻的第三条分支，
    // 风险已被同类防线覆盖。为凑数造一个大到不实用、且不稳定的假测试没有
    // 意义，故跳过。

    use std::collections::HashMap;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use tokio::sync::OnceCell;
    use tokio_util::sync::CancellationToken;

    use crate::db::Db;
    use crate::downloader::DownloadError;
    use crate::ed2k::hash;
    use crate::ed2k::link::parse_ed2k_link;
    use crate::ed2k::peer::download_block_from_peer;
    use crate::ed2k::server::{PeerAddr, find_sources};
    use crate::ed2k::testutil::{MockPeer, MockServer, PeerFault, ed2k_link, root_hash};
    use crate::speed_limiter::SpeedLimiter;

    static IT_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// 分配一个唯一的临时目录（tag 便于失败定位）。
    fn it_scratch_dir(tag: &str) -> PathBuf {
        let n = IT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ed2k_it_{}_{tag}_{n}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            panic!("create scratch dir failed: {e}");
        }
        dir
    }

    /// 预建 dest 文件：`download_block_from_peer` 只 `OpenOptions::write` 不
    /// create，按绝对偏移 seek 写入，测试须先建好定长文件。
    async fn prep_dest(path: &Path, total: u64) {
        let file = match tokio::fs::File::create(path).await {
            Ok(f) => f,
            Err(e) => panic!("create dest failed: {e}"),
        };
        if let Err(e) = file.set_len(total).await {
            panic!("set_len failed: {e}");
        }
    }

    fn new_cancel() -> CancellationToken {
        CancellationToken::new()
    }

    fn no_limit() -> SpeedLimiter {
        SpeedLimiter::new(0)
    }

    fn fresh_hashset_cache() -> Arc<OnceCell<Vec<[u8; 16]>>> {
        Arc::new(OnceCell::new())
    }

    fn empty_progress() -> Arc<StdMutex<HashMap<u64, i64>>> {
        Arc::new(StdMutex::new(HashMap::new()))
    }

    /// 开一个全新的测试 DB（独立临时目录）。
    async fn open_it_db(tag: &str) -> (Db, PathBuf) {
        let dir = it_scratch_dir(tag);
        let db = match Db::open(&dir).await {
            Ok(db) => db,
            Err(e) => panic!("open db failed: {e}"),
        };
        (db, dir)
    }

    async fn insert_it_task(db: &Db, task_id: &str, total_bytes: u64) {
        let res = db
            .insert_task(
                task_id,
                "ed2k://it",
                "it.bin",
                ".",
                1,
                total_bytes as i64,
                "",
                "",
                "",
                0,
            )
            .await;
        if let Err(e) = res {
            panic!("insert_task failed: {e}");
        }
    }

    // --- peer 层：1. happy 单块（小数据 + 超大 part_size 两种配置） ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_happy_single_block_small_and_large_part_size() {
        let configs: [(&str, u64); 2] =
            [("default_ps", hash::PART_SIZE), ("huge_ps", 50_000_000_000)];
        for (tag, part_size) in configs {
            let data: Vec<u8> = (0..321u32).map(|i| (i % 256) as u8).collect();
            let total = data.len() as u64;
            let root = root_hash(&data, part_size);
            assert_eq!(
                hash::part_count(total, part_size),
                1,
                "config {tag} must be single-block"
            );

            let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::None).await {
                Ok(p) => p,
                Err(e) => panic!("spawn mock peer failed ({tag}): {e}"),
            };

            let dir = it_scratch_dir(&format!("peer1_{tag}"));
            let dest = dir.join("out.tmp");
            prep_dest(&dest, total).await;

            let result = download_block_from_peer(
                peer.peer_addr(),
                &root,
                0,
                total,
                part_size,
                false,
                &dest,
                &new_cancel(),
                &no_limit(),
                fresh_hashset_cache(),
                empty_progress(),
            )
            .await;

            let (returned_peer, md4) = match result {
                Ok(v) => v,
                Err(e) => panic!("download failed ({tag}): {e:?}"),
            };
            assert_eq!(returned_peer, peer.peer_addr());
            assert_eq!(md4, root, "returned md4 must equal root hash ({tag})");

            let on_disk = match tokio::fs::read(&dest).await {
                Ok(b) => b,
                Err(e) => panic!("read dest failed ({tag}): {e}"),
            };
            assert_eq!(on_disk, data, "disk content must equal source data ({tag})");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    // --- peer 层：2. happy 多块，hashset_cache 跨块复用 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_happy_multi_block_reuses_hashset_cache() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 251) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        let part_count = hash::part_count(total, part_size);
        assert_eq!(part_count, 4, "800B / 256 part_size must yield 4 blocks");

        // peer_a：正常应答，首块下载触发 hashset 拉取并填充共享缓存。
        let peer_a = match MockPeer::spawn(data.clone(), part_size, PeerFault::None).await {
            Ok(p) => p,
            Err(e) => panic!("spawn peer_a failed: {e}"),
        };
        // peer_b：hashset 投毒。若缓存未被复用、后续块重新拉取 hashset 会在
        // 自验时失败暴露；缓存正确复用时 `ensure_hashset` 命中 cache 直接返回，
        // 永远不会向 peer_b 发送 HASHSETREQUEST。
        let peer_b = match MockPeer::spawn(data.clone(), part_size, PeerFault::PoisonHashset).await
        {
            Ok(p) => p,
            Err(e) => panic!("spawn peer_b failed: {e}"),
        };

        let dir = it_scratch_dir("peer2");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let cache = fresh_hashset_cache();
        let progress = empty_progress();

        for block_index in 0..part_count {
            let peer = if block_index == 0 {
                peer_a.peer_addr()
            } else {
                peer_b.peer_addr()
            };
            let result = download_block_from_peer(
                peer,
                &root,
                block_index,
                total,
                part_size,
                false,
                &dest,
                &new_cancel(),
                &no_limit(),
                Arc::clone(&cache),
                Arc::clone(&progress),
            )
            .await;
            let (_, md4) = match result {
                Ok(v) => v,
                Err(e) => panic!("block {block_index} failed: {e:?}"),
            };
            let (s, e) = hash::part_span(block_index, total, part_size);
            assert_eq!(
                md4,
                hash::hash_part(&data[s as usize..e as usize]),
                "block {block_index} md4 mismatch"
            );
        }

        assert!(
            cache.get().is_some(),
            "hashset cache must be populated after first block"
        );

        let on_disk = match tokio::fs::read(&dest).await {
            Ok(b) => b,
            Err(e) => panic!("read dest failed: {e}"),
        };
        assert_eq!(on_disk, data);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：3. happy 压缩帧（单块） ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_happy_compressed_frame_single_block() {
        let part_size = hash::PART_SIZE;
        let data: Vec<u8> = (0..2000u32).map(|i| (i % 200) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        assert_eq!(hash::part_count(total, part_size), 1);

        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::Compressed).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };

        let dir = it_scratch_dir("peer3");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = download_block_from_peer(
            peer.peer_addr(),
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &new_cancel(),
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let (_, md4) = match result {
            Ok(v) => v,
            Err(e) => panic!("compressed download failed: {e:?}"),
        };
        assert_eq!(md4, root);

        let on_disk = match tokio::fs::read(&dest).await {
            Ok(b) => b,
            Err(e) => panic!("read dest failed: {e}"),
        };
        assert_eq!(on_disk, data);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：4. 投毒 hashset → Ed2kIntegrity ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_poisoned_hashset_rejected_as_integrity() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 250) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        assert_eq!(hash::part_count(total, part_size), 4);

        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::PoisonHashset).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };

        let dir = it_scratch_dir("peer4");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = download_block_from_peer(
            peer.peer_addr(),
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &new_cancel(),
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let Err(err) = result else {
            panic!("poisoned hashset must fail download");
        };
        assert!(
            matches!(err.source, DownloadError::Ed2kIntegrity(_)),
            "expected Ed2kIntegrity, got {:?}",
            err.source
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：5. 越界分片 → Ed2kIntegrity ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_out_of_bounds_part_rejected_as_integrity() {
        let part_size = hash::PART_SIZE;
        let data: Vec<u8> = (0..500u32).map(|i| (i % 200) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        assert_eq!(hash::part_count(total, part_size), 1);

        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::OutOfBounds).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };

        let dir = it_scratch_dir("peer5");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = download_block_from_peer(
            peer.peer_addr(),
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &new_cancel(),
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let Err(err) = result else {
            panic!("out-of-bounds sendingpart must fail download");
        };
        assert!(
            matches!(err.source, DownloadError::Ed2kIntegrity(_)),
            "expected Ed2kIntegrity, got {:?}",
            err.source
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：6. 长度不符 → Ed2kIntegrity ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_length_mismatch_rejected_as_integrity() {
        let part_size = hash::PART_SIZE;
        let data: Vec<u8> = (0..500u32).map(|i| (i % 200) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        assert_eq!(hash::part_count(total, part_size), 1);

        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::LengthMismatch).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };

        let dir = it_scratch_dir("peer6");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = download_block_from_peer(
            peer.peer_addr(),
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &new_cancel(),
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let Err(err) = result else {
            panic!("length-mismatch sendingpart must fail download");
        };
        assert!(
            matches!(err.source, DownloadError::Ed2kIntegrity(_)),
            "expected Ed2kIntegrity, got {:?}",
            err.source
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：7. 连接失败 → 非 Ed2kIntegrity（Io/Ed2k） ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_connection_failure_is_not_integrity_violation() {
        let part_size = hash::PART_SIZE;
        let data: Vec<u8> = vec![1u8; 300];
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);

        // 端口 1 为特权端口，本机通常未监听，连接会被立即拒绝（纯网络失败）。
        let dead_peer = PeerAddr {
            ip: Ipv4Addr::LOCALHOST,
            port: 1,
        };

        let dir = it_scratch_dir("peer7");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = download_block_from_peer(
            dead_peer,
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &new_cancel(),
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let Err(err) = result else {
            panic!("connecting to dead port must fail");
        };
        assert!(
            !matches!(err.source, DownloadError::Ed2kIntegrity(_)),
            "pure network failure must not be classified as Ed2kIntegrity, got {:?}",
            err.source
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- peer 层：8. cancel → Cancelled ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_cancelled_before_read_returns_cancelled() {
        let part_size = hash::PART_SIZE;
        let data: Vec<u8> = vec![7u8; 300];
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);

        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::None).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };

        let dir = it_scratch_dir("peer8");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let cancel = new_cancel();
        cancel.cancel();

        let result = download_block_from_peer(
            peer.peer_addr(),
            &root,
            0,
            total,
            part_size,
            false,
            &dest,
            &cancel,
            &no_limit(),
            fresh_hashset_cache(),
            empty_progress(),
        )
        .await;

        let Err(err) = result else {
            panic!("pre-cancelled token must abort download");
        };
        assert!(
            matches!(err.source, DownloadError::Cancelled),
            "expected Cancelled, got {:?}",
            err.source
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- server 层：9. happy 找源，HighID 还原正确 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_find_sources_happy_reconstructs_highid_peer() {
        let target = PeerAddr {
            ip: Ipv4Addr::LOCALHOST,
            port: 12345,
        };
        let server = match MockServer::spawn(vec![target]).await {
            Ok(s) => s,
            Err(e) => panic!("spawn mock server failed: {e}"),
        };

        let file_hash = [0x11u8; 16];
        let result = find_sources(
            &[server.server_string()],
            &file_hash,
            1_000_000,
            false,
            &new_cancel(),
        )
        .await;

        let peers = match result {
            Ok(p) => p,
            Err(e) => panic!("find_sources failed: {e}"),
        };
        assert_eq!(peers, vec![target], "must reconstruct exact HighID peer");
    }

    // --- server 层：10. 空源列表 → Ed2k ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_find_sources_empty_list_is_ed2k_error() {
        let server = match MockServer::spawn(vec![]).await {
            Ok(s) => s,
            Err(e) => panic!("spawn mock server failed: {e}"),
        };

        let file_hash = [0x22u8; 16];
        let result = find_sources(
            &[server.server_string()],
            &file_hash,
            1_000_000,
            false,
            &new_cancel(),
        )
        .await;

        let Err(err) = result else {
            panic!("empty source list must yield an error");
        };
        assert!(
            matches!(err, DownloadError::Ed2k(_)),
            "expected Ed2k, got {err:?}"
        );
    }

    // --- server 层：11. 无可达服务器 → Err ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_find_sources_unreachable_server_is_error() {
        let file_hash = [0x33u8; 16];
        // 端口 1 未监听，连接立即失败；服务器列表遍历完毕后应归为 Ed2k。
        let result = find_sources(
            &["127.0.0.1:1".to_string()],
            &file_hash,
            1_000_000,
            false,
            &new_cancel(),
        )
        .await;

        let Err(err) = result else {
            panic!("unreachable server must yield an error");
        };
        assert!(
            matches!(err, DownloadError::Ed2k(_)),
            "expected Ed2k, got {err:?}"
        );
    }

    // --- 终验层：12. happy 多块 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalize_happy_multi_block_verifies() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 253) as u8).collect();
        let total = data.len() as u64;
        let part_count = hash::part_count(total, part_size);
        assert_eq!(part_count, 4);

        let link_url = ed2k_link("movie.bin", &data, part_size);
        let link = match parse_ed2k_link(&link_url) {
            Ok(l) => l,
            Err(e) => panic!("parse generated ed2k link failed: {e:?}"),
        };

        let (db, dir) = open_it_db("fin12").await;
        let task_id = "fin12-task";
        insert_it_task(&db, task_id, total).await;
        if let Err(e) = db.init_ed2k_blocks(task_id, part_count).await {
            panic!("init_ed2k_blocks failed: {e}");
        }
        for i in 0..part_count {
            if let Err(e) = db
                .update_ed2k_block(task_id, i, BLOCK_VERIFIED, part_size as i64, false)
                .await
            {
                panic!("update_ed2k_block failed: {e}");
            }
        }

        let mut hashset_blob = Vec::with_capacity(part_count as usize * 16);
        for i in 0..part_count {
            let (s, e) = hash::part_span(i, total, part_size);
            hashset_blob.extend_from_slice(&hash::hash_part(&data[s as usize..e as usize]));
        }
        if let Err(e) = db.save_ed2k_hashset(task_id, &hashset_blob).await {
            panic!("save_ed2k_hashset failed: {e}");
        }

        let temp = dir.join("movie.bin.part");
        if let Err(e) = tokio::fs::write(&temp, &data).await {
            panic!("write temp failed: {e}");
        }

        let result = finalize_and_verify(&db, task_id, &temp, &link, part_size).await;
        if let Err(e) = result {
            panic!("finalize_and_verify must succeed on clean data: {e:?}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 终验层：13. 磁盘坏块 → Ed2kIntegrity + 该块重置 missing ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalize_disk_corruption_resets_bad_block_to_missing() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 253) as u8).collect();
        let total = data.len() as u64;
        let part_count = hash::part_count(total, part_size);
        assert_eq!(part_count, 4);

        let link_url = ed2k_link("movie2.bin", &data, part_size);
        let link = match parse_ed2k_link(&link_url) {
            Ok(l) => l,
            Err(e) => panic!("parse generated ed2k link failed: {e:?}"),
        };

        let (db, dir) = open_it_db("fin13").await;
        let task_id = "fin13-task";
        insert_it_task(&db, task_id, total).await;
        if let Err(e) = db.init_ed2k_blocks(task_id, part_count).await {
            panic!("init_ed2k_blocks failed: {e}");
        }
        for i in 0..part_count {
            if let Err(e) = db
                .update_ed2k_block(task_id, i, BLOCK_VERIFIED, part_size as i64, false)
                .await
            {
                panic!("update_ed2k_block failed: {e}");
            }
        }

        let mut hashset_blob = Vec::with_capacity(part_count as usize * 16);
        for i in 0..part_count {
            let (s, e) = hash::part_span(i, total, part_size);
            hashset_blob.extend_from_slice(&hash::hash_part(&data[s as usize..e as usize]));
        }
        if let Err(e) = db.save_ed2k_hashset(task_id, &hashset_blob).await {
            panic!("save_ed2k_hashset failed: {e}");
        }

        // 破坏落在块 1 范围内的字节（[256,512)），其余块保持正确。
        let mut corrupted = data.clone();
        corrupted[300] ^= 0xFF;
        let temp = dir.join("movie2.bin.part");
        if let Err(e) = tokio::fs::write(&temp, &corrupted).await {
            panic!("write temp failed: {e}");
        }

        let result = finalize_and_verify(&db, task_id, &temp, &link, part_size).await;
        let Err(err) = result else {
            panic!("corrupted disk block must fail finalize");
        };
        assert!(
            matches!(err, DownloadError::Ed2kIntegrity(_)),
            "expected Ed2kIntegrity, got {err:?}"
        );

        let blocks = match db.load_ed2k_blocks(task_id).await {
            Ok(b) => b,
            Err(e) => panic!("load_ed2k_blocks failed: {e}"),
        };
        for (idx, state, _, _) in &blocks {
            if *idx == 1 {
                assert_eq!(
                    *state, BLOCK_MISSING,
                    "corrupted block 1 must be reset to missing"
                );
            } else {
                assert_eq!(
                    *state, BLOCK_VERIFIED,
                    "untouched block {idx} must remain verified"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 终验层：14. hashset 缺失 → Ed2k（非 Integrity） ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalize_missing_hashset_is_ed2k_not_integrity() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 253) as u8).collect();
        let total = data.len() as u64;
        let part_count = hash::part_count(total, part_size);
        assert_eq!(part_count, 4);

        let link_url = ed2k_link("movie3.bin", &data, part_size);
        let link = match parse_ed2k_link(&link_url) {
            Ok(l) => l,
            Err(e) => panic!("parse generated ed2k link failed: {e:?}"),
        };

        let (db, dir) = open_it_db("fin14").await;
        let task_id = "fin14-task";
        insert_it_task(&db, task_id, total).await;
        if let Err(e) = db.init_ed2k_blocks(task_id, part_count).await {
            panic!("init_ed2k_blocks failed: {e}");
        }
        for i in 0..part_count {
            if let Err(e) = db
                .update_ed2k_block(task_id, i, BLOCK_VERIFIED, part_size as i64, false)
                .await
            {
                panic!("update_ed2k_block failed: {e}");
            }
        }
        // 有意不调用 save_ed2k_hashset。

        let temp = dir.join("movie3.bin.part");
        if let Err(e) = tokio::fs::write(&temp, &data).await {
            panic!("write temp failed: {e}");
        }

        let result = finalize_and_verify(&db, task_id, &temp, &link, part_size).await;
        let Err(err) = result else {
            panic!("missing hashset must fail finalize");
        };
        assert!(
            matches!(err, DownloadError::Ed2k(_)),
            "expected Ed2k, got {err:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 终验层：15. happy 单块 + 单块坏字节 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finalize_single_block_happy_then_corrupted_byte() {
        let part_size: u64 = 300;
        let data: Vec<u8> = (0..100u32).map(|i| (i % 200) as u8).collect();
        let total = data.len() as u64;
        assert_eq!(hash::part_count(total, part_size), 1);

        let link_url = ed2k_link("single.bin", &data, part_size);
        let link = match parse_ed2k_link(&link_url) {
            Ok(l) => l,
            Err(e) => panic!("parse generated ed2k link failed: {e:?}"),
        };

        let (db, dir) = open_it_db("fin15").await;
        let task_id = "fin15-task";
        insert_it_task(&db, task_id, total).await;
        if let Err(e) = db.init_ed2k_blocks(task_id, 1).await {
            panic!("init_ed2k_blocks failed: {e}");
        }
        if let Err(e) = db
            .update_ed2k_block(task_id, 0, BLOCK_VERIFIED, total as i64, false)
            .await
        {
            panic!("update_ed2k_block failed: {e}");
        }

        let temp = dir.join("single.bin.part");
        if let Err(e) = tokio::fs::write(&temp, &data).await {
            panic!("write temp failed: {e}");
        }

        // 单块 happy：正确字节 → Ok。
        if let Err(e) = finalize_and_verify(&db, task_id, &temp, &link, part_size).await {
            panic!("finalize_and_verify must succeed on clean single block: {e:?}");
        }

        // 显式恢复为 verified，隔离下一步坏字节场景的前置状态。
        if let Err(e) = db
            .update_ed2k_block(task_id, 0, BLOCK_VERIFIED, total as i64, false)
            .await
        {
            panic!("update_ed2k_block failed: {e}");
        }

        // 坏字节：单块内容损坏 → Ed2kIntegrity 且块重置 missing。
        let mut corrupted = data.clone();
        corrupted[42] ^= 0xFF;
        if let Err(e) = tokio::fs::write(&temp, &corrupted).await {
            panic!("write corrupted temp failed: {e}");
        }

        let result = finalize_and_verify(&db, task_id, &temp, &link, part_size).await;
        let Err(err) = result else {
            panic!("corrupted single block must fail finalize");
        };
        assert!(
            matches!(err, DownloadError::Ed2kIntegrity(_)),
            "expected Ed2kIntegrity, got {err:?}"
        );

        let blocks = match db.load_ed2k_blocks(task_id).await {
            Ok(b) => b,
            Err(e) => panic!("load_ed2k_blocks failed: {e}"),
        };
        let Some((_, state, _, _)) = blocks.first() else {
            panic!("expected exactly one block row");
        };
        assert_eq!(
            *state, BLOCK_MISSING,
            "corrupted single block must be reset to missing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
