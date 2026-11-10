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
pub(crate) mod identity;
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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::OnceCell;
use tokio::task::JoinSet;

use crate::downloader::{DownloadError, DownloadParams, ProgressUpdate, SegmentProgressInfo};
use crate::ed2k::client::{Source, shared_client};
use crate::ed2k::link::{Ed2kLink, parse_ed2k_link};
use crate::ed2k::peer::PeerSession;
use crate::ed2k::server::{PeerAddr, parse_server_list};
use crate::logger::log_info;
use crate::output;
use crate::transfer_activity::{TaskRuntime, TaskSegment, TransferTracker};

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

/// 单个块下载任务的 join 结果：`(block_index, 源, 仍可复用的连接, 本次是否向对端
/// 申请过上传槽, 成功 md4 | 失败 DownloadError)`。源随结果回传，供调度层按失败类型
/// 对该源退避或拉黑。
type BlockJoinResult = (
    u64,
    Source,
    Option<PeerSession>,
    bool,
    Result<[u8; 16], DownloadError>,
);

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
/// 对同一源两次申请上传槽（`OP_STARTUPLOADREQ`）的最小间隔。eMule 把 10 分钟内
/// 的重复申请记为坏请求，累计 4 次封 IP 2 小时；留 1 分钟余量。
const REASK_INTERVAL: Duration = Duration::from_secs(11 * 60);
/// 旁路进度上报周期。
const PROGRESS_TICK: Duration = Duration::from_millis(200);

/// 块完成标记在数据同步并提交数据库之后发布给内存快照。
struct BlockSnapshot {
    verified: Vec<bool>,
    verified_bytes: i64,
}

impl BlockSnapshot {
    fn from_rows(total: u64, part_size: u64, rows: &[(u64, i64, i64, i64)]) -> Self {
        let mut snapshot = Self {
            verified: vec![false; hash::part_count(total, part_size) as usize],
            verified_bytes: 0,
        };
        for &(index, state, _, _) in rows {
            if state == BLOCK_VERIFIED {
                snapshot.set_verified(index, true, total, part_size);
            }
        }
        snapshot
    }

    fn set_verified(&mut self, index: u64, verified: bool, total: u64, part_size: u64) {
        if let Some(old) = self.verified.get_mut(index as usize)
            && *old != verified
        {
            let (start, end) = hash::part_span(index, total, part_size);
            let bytes = (end - start) as i64;
            self.verified_bytes += if verified { bytes } else { -bytes };
            *old = verified;
        }
    }

    fn pending(&self) -> VecDeque<u64> {
        self.verified
            .iter()
            .enumerate()
            .filter_map(|(index, verified)| (!*verified).then_some(index as u64))
            .collect()
    }
}

fn check_download_space(available: Option<u64>, required: u64) -> Result<(), DownloadError> {
    if required != 0
        && let Some(available) = available
        && available < required.saturating_add(crate::disk_space::PRECHECK_MARGIN)
    {
        return Err(DownloadError::Ed2k(format!(
            "insufficient disk space for ED2K download: need {required}B + {}B safety margin, have {available}B",
            crate::disk_space::PRECHECK_MARGIN
        )));
    }
    Ok(())
}

async fn persist_verified_blocks(
    db: &crate::db::Db,
    task_id: &str,
    temp: &Path,
    blocks: &[(u64, i64, i64, bool)],
    hashset: Option<&[[u8; 16]]>,
) -> Result<(), DownloadError> {
    // 数据先持久化，恢复时才能信任随后提交的 verified 标记。
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temp)
        .await
        .map_err(DownloadError::Io)?;
    file.sync_data().await.map_err(DownloadError::Io)?;
    if let Some(hashset) = hashset {
        let blob: Vec<u8> = hashset.iter().flatten().copied().collect();
        db.save_ed2k_hashset(task_id, &blob).await?;
    }
    db.update_ed2k_blocks(task_id, blocks).await?;
    Ok(())
}

fn remaining_disk_bytes(total: u64, verified: u64, allocated: Option<u64>) -> u64 {
    total.saturating_sub(verified.max(allocated.unwrap_or(0)))
}

async fn prepare_temp_and_blocks(
    db: &crate::db::Db,
    task_id: &str,
    temp: &Path,
    total_bytes: u64,
    part_size: u64,
    available: Option<u64>,
) -> Result<BlockSnapshot, DownloadError> {
    let temp_ok = matches!(tokio::fs::metadata(temp).await, Ok(m) if m.len() == total_bytes);
    let resume_rows = if temp_ok {
        db.load_ed2k_blocks(task_id).await?
    } else {
        Vec::new()
    };
    let snapshot = BlockSnapshot::from_rows(total_bytes, part_size, &resume_rows);
    // 已分配空间可以原地复用；查询失败时只抵扣可靠的已校验块。
    let allocated = if temp_ok
        && available.is_some()
        && snapshot.verified_bytes as u64 != total_bytes
    {
        let temp = temp.to_path_buf();
        let query = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(temp)?;
            fs2::FileExt::allocated_size(&file)
        });
        match tokio::time::timeout(Duration::from_secs(3), query).await {
            Ok(Ok(Ok(bytes))) => Some(bytes),
            Ok(Ok(Err(error))) => {
                crate::log_warn!(
                    "[ed2k] allocation query failed; reserving unverified bytes: {}",
                    error
                );
                None
            }
            Ok(Err(error)) => {
                if error.is_cancelled() {
                    tracing::debug!("ED2K allocation query cancelled during shutdown");
                } else {
                    crate::log_error!("[ed2k] allocation query task panicked: {}", error);
                }
                None
            }
            Err(error) => {
                tracing::debug!(%error, "ED2K allocation query timed out; reserving unverified bytes");
                None
            }
        }
    } else {
        None
    };
    check_download_space(
        available,
        remaining_disk_bytes(total_bytes, snapshot.verified_bytes as u64, allocated),
    )?;
    if !temp_ok {
        // 先清除旧标记，再重建文件，避免中途退出留下指向空洞的 verified 状态。
        db.reset_ed2k_blocks(task_id).await?;
        let file = tokio::fs::File::create(temp)
            .await
            .map_err(DownloadError::Io)?;
        file.set_len(total_bytes).await.map_err(DownloadError::Io)?;
    }
    db.init_ed2k_blocks(task_id, hash::part_count(total_bytes, part_size))
        .await?;
    Ok(snapshot)
}

/// ED2K 下载编排入口（签名同 `run_ftp_download`：接受 [`DownloadParams`] 单参）。
///
/// 流程：解析链接 → 磁盘余量预检 → 恢复/初始化块快照 → 起旁路进度任务 →
/// 外层调度状态机（找源 / 并发拉块 / 逐块 MD4 / 完整性拉黑）→ 终验读盘
/// 重算锚定 root → `sync_all`+`rename`。旁路进度任务在任一退出路径 `abort()`。
pub async fn run_ed2k_download(params: DownloadParams) {
    let task_id_log = params.task_id.clone();
    let result = async {
        let outcome = run_ed2k_download_inner(&params).await?;
        params.db.update_task_status(&params.task_id, 3, "").await?;
        Ok::<_, DownloadError>(outcome)
    }
    .await;
    match result {
        Ok((total, final_name)) => {
            log_info!(
                "[ed2k-download] task {} completed, total={}",
                task_id_log,
                total
            );

            if params
                .progress_tx
                .send(ProgressUpdate {
                    task_id: params.task_id,
                    downloaded_bytes: total,
                    total_bytes: total,
                    status: 3,
                    error_message: String::new(),
                    file_name: final_name,
                    segment_details: None,
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("ED2K completion receiver closed during shutdown");
            }
        }
        Err(DownloadError::Cancelled) => {
            log_info!("[ed2k-download] task {} cancelled", task_id_log);
        }
        Err(e) => {
            let msg = e.to_string();
            crate::logger::report_error("ed2k-download", "run task", &e);
            if let Err(db_error) = params.db.update_task_status(&params.task_id, 4, &msg).await {
                crate::logger::report_error(
                    "ed2k-download",
                    "persist terminal error status",
                    &db_error,
                );
            }
            let (dl, total) = match params.db.load_task_by_id(&params.task_id).await {
                Ok(Some(t)) => (t.downloaded_bytes, t.total_bytes),
                _ => (0, 0),
            };
            if params
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
                .await
                .is_err()
            {
                tracing::debug!("ED2K error receiver closed during shutdown");
            }
        }
    }
}

/// 内层实现：返回下载总字节数或错误。
async fn run_ed2k_download_inner(params: &DownloadParams) -> Result<(i64, String), DownloadError> {
    let link = parse_ed2k_link(&params.url)?;
    let total_bytes = link.total_bytes;
    let part_size = hash::PART_SIZE;
    let large_file = total_bytes > hash::OLD_MAX_FILE_SIZE;
    let is_single = hash::is_single_block(total_bytes, part_size);
    let task_id = params.task_id.clone();

    params.db.update_task_status(&task_id, 5, "").await?;

    let save_dir = Path::new(&params.save_dir);
    // manager 传入的名字已做 dedup / 用户自定义 / 预订临时路径，必须沿用，
    // 否则同名文件会被覆盖且 DB 名与落盘名不一致。
    let file_name = resolve_file_name(&params.file_name, &link.file_name);
    if file_name != link.file_name {
        let siblings: HashSet<String> = params
            .db
            .list_active_sibling_file_names(&params.save_dir, &task_id)
            .await?
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
        adopt_legacy_temp(
            save_dir,
            &link.file_name,
            &file_name,
            total_bytes,
            &siblings,
        )
        .await?;
    }
    let temp_path = save_dir.join(format!("{file_name}{}", crate::downloader::TEMP_EXT));

    output::ensure_parent(&temp_path).await?;
    let available = crate::disk_space::available_space_checked(save_dir.to_path_buf()).await;
    let blocks = Arc::new(StdMutex::new(
        prepare_temp_and_blocks(
            &params.db,
            &task_id,
            &temp_path,
            total_bytes,
            part_size,
            available,
        )
        .await?,
    ));

    if total_bytes == 0 {
        if link.root_hash != hash::MD4_EMPTY {
            return Err(DownloadError::Ed2kIntegrity(
                "0-byte file root hash mismatch".into(),
            ));
        }
        let avoid: HashSet<String> = params
            .db
            .list_active_sibling_file_names(&params.save_dir, &task_id)
            .await?
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
        let final_name =
            finalize_rename(&temp_path, save_dir, &file_name, &params.overwrite, &avoid).await?;
        persist_final_name(params, &task_id, &file_name, &final_name, 0).await?;
        return Ok((0, final_name));
    }

    if params
        .progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: total_bytes as i64,
            status: 1,
            error_message: String::new(),
            file_name: file_name.clone(),
            segment_details: None,
            ..Default::default()
        })
        .await
        .is_err()
    {
        tracing::debug!("ED2K progress receiver closed during shutdown");
        return Err(DownloadError::Cancelled);
    }

    // 服务器来源合并：用户手填列表（ed2k_server_list）+ 订阅缓存
    // （ed2k_server_sub_cache，由宿主定期刷新 server.met 写入）。
    let manual_cfg = params
        .db
        .get_config("ed2k_server_list")
        .await?
        .unwrap_or_default();
    let sub_cfg = params
        .db
        .get_config("ed2k_server_sub_cache")
        .await?
        .unwrap_or_default();
    let mut server_list = parse_server_list(&manual_cfg);
    let mut seen: HashSet<String> = server_list.iter().cloned().collect();
    for s in parse_server_list(&sub_cfg) {
        if seen.insert(s.clone()) {
            server_list.push(s);
        }
    }

    let progress: Arc<StdMutex<HashMap<u64, i64>>> = Arc::new(StdMutex::new(HashMap::new()));
    let tracker = TransferTracker::new();
    let connected = TransferTracker::new();
    let concurrency_limit = Arc::new(AtomicU32::new(0));
    let progress_guard = AbortOnDrop(spawn_progress_reporter(ProgressReporterContext {
        blocks: Arc::clone(&blocks),
        progress_tx: params.progress_tx.clone(),
        task_id: task_id.clone(),
        total_bytes,
        part_size,
        progress: Arc::clone(&progress),
        tracker: tracker.clone(),
        connected: connected.clone(),
        concurrency_limit: Arc::clone(&concurrency_limit),
    }));

    let hashset_cache: Arc<OnceCell<Vec<[u8; 16]>>> = Arc::new(OnceCell::new());
    crate::ed2k::identity::ensure_user_hash(&params.db).await?;
    let client = shared_client();
    // 客户端配置：监听端口/UPnP/Kad 开关来自 DB。
    let listen_port = params
        .db
        .get_config("ed2k_listen_port")
        .await?
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let enable_upnp = params
        .db
        .get_config("ed2k_enable_upnp")
        .await?
        .map(|v| v == "true")
        .unwrap_or(true);
    let enable_kad = params
        .db
        .get_config("ed2k_enable_kad")
        .await?
        .map(|v| v == "true")
        .unwrap_or(true);
    client.configure(crate::ed2k::client::ClientConfig {
        listen_port,
        udp_port: 0,
        servers: server_list.clone(),
        enable_upnp,
        enable_kad,
    });
    let _active_task = client.begin_task();
    // Kad bootstrap 节点（nodes.dat，base64 缓存，由宿主后台刷新）。
    let nodes_dat: Vec<u8> = if enable_kad {
        use base64::Engine as _;
        params
            .db
            .get_config("ed2k_nodes_dat_cache")
            .await?
            .filter(|s| !s.is_empty())
            .and_then(
                |s| match base64::engine::general_purpose::STANDARD.decode(&s) {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        crate::log_warn!("[ed2k] invalid cached Kad bootstrap data: {}", error);
                        None
                    }
                },
            )
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut sources: Vec<Source> = Vec::new();
    let mut source_retries: u32 = 0;
    let mut backoff: HashSet<Source> = HashSet::new();
    let mut lacks: HashSet<(Source, u64)> = HashSet::new();
    let mut integrity_bl: HashSet<Source> = HashSet::new();
    let mut strikes: HashMap<u64, u32> = HashMap::new();
    let mut finalize_retries: u32 = 0;
    let mut round_robin: usize = 0;
    let mut hashset_persisted = false;
    // 各源空闲中的已握手连接：一个块下完后放回，下一个块直接复用，不重新申请槽位。
    let mut idle: HashMap<Source, PeerSession> = HashMap::new();
    // 各源最近一次申请上传槽的时刻（见 [`REASK_INTERVAL`]）。
    let mut asked_at: HashMap<Source, tokio::time::Instant> = HashMap::new();

    let outcome: Result<i64, DownloadError> = 'outer: loop {
        let mut pending = match blocks.lock() {
            Ok(snapshot) => snapshot.pending(),
            Err(_) => break 'outer Err(DownloadError::Ed2k("block snapshot lock poisoned".into())),
        };
        if pending.is_empty() {
            match finalize_and_verify(&params.db, &task_id, &temp_path, &link, part_size).await {
                Ok(()) => break 'outer Ok(total_bytes as i64),
                Err(DownloadError::Ed2kIntegrity(_)) if finalize_retries < FINALIZE_MAX_RETRIES => {
                    let rows = params.db.load_ed2k_blocks(&task_id).await?;
                    *blocks.lock().map_err(|_| {
                        DownloadError::Ed2k("block snapshot lock poisoned".into())
                    })? = BlockSnapshot::from_rows(total_bytes, part_size, &rows);
                    finalize_retries += 1;
                    if let Err(journal_error) = crate::task_activity::record(&params.db, params.sink.as_ref(), &task_id, "retry", format!("ED2K 完整性校验失败，重下坏块（第 {finalize_retries}/{FINALIZE_MAX_RETRIES} 轮）"), None).await {
                        crate::log_error!("[task-activity] failed to persist retry: {}", journal_error);
                    }
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
        concurrency_limit.store(concurrency as u32, Ordering::Relaxed);
        let mut join: JoinSet<BlockJoinResult> = JoinSet::new();
        let mut ready = Vec::with_capacity(concurrency);
        let mut updates = Vec::with_capacity(concurrency);
        // 正在连接 / 传输中的源：每个源同时只占一条连接。eMule 按 user hash + IP
        // 认客户端，同一客户端的第二条连接会顶掉前一条，并发多连只会全部被断开。
        let mut busy: HashSet<Source> = HashSet::new();

        let inner: Result<(), DownloadError> = 'download: loop {
            if params.cancel_token.is_cancelled() {
                break Err(DownloadError::Cancelled);
            }
            // 不可选的源 = 在途 ∪（无空闲连接且仍在重询冷却期内）。
            let now = tokio::time::Instant::now();
            let mut unavailable = busy.clone();
            unavailable.extend(
                asked_at
                    .iter()
                    .filter(|(src, at)| {
                        !idle.contains_key(*src) && now.duration_since(**at) < REASK_INTERVAL
                    })
                    .map(|(src, _)| *src),
            );
            while join.len() < concurrency && !pending.is_empty() {
                // 在队列头部窗口内找第一个存在可用源的块：队首块可能只有被
                // 声明缺该块的源，不应阻塞后面的块。
                let picked =
                    pending
                        .iter()
                        .take(PICK_WINDOW)
                        .enumerate()
                        .find_map(|(pos, &block)| {
                            pick_source(
                                &sources,
                                &backoff,
                                &integrity_bl,
                                &unavailable,
                                &lacks,
                                block,
                                &mut round_robin,
                            )
                            .map(|src| (pos, src))
                        });
                let Some((pos, src)) = picked else { break };
                let Some(bi) = pending.remove(pos) else { break };
                busy.insert(src);
                unavailable.insert(src);
                let session = idle.remove(&src);
                let file_hash = link.root_hash;
                let dest = temp_path.clone();
                let cancel = params.cancel_token.clone();
                let lim = params.speed_limiter.clone();
                let hc = Arc::clone(&hashset_cache);
                let pg = Arc::clone(&progress);
                let client = Arc::clone(&client);
                let tracker = tracker.clone();
                let connected = connected.clone();
                join.spawn(async move {
                    // 连接/握手/排队可能阻塞数十秒且不看 cancel，整体与 cancel 竞速，
                    // 使暂停/删除立即生效（drop 会关闭 socket）。
                    let work = async {
                        let (mut session, asked) = match session {
                            Some(session) => (session, false),
                            None => {
                                // HighID 直连 / LowID 经服务器 callback 中转。
                                let stream = match client.connect_source(src).await {
                                    Ok(stream) => stream,
                                    Err(error) => return (None, false, Err(error)),
                                };
                                match PeerSession::open(stream, &file_hash, &cancel).await {
                                    Ok(session) => (session, true),
                                    Err(error) => return (None, error.asked, Err(error.source)),
                                }
                            }
                        };
                        let _connection = connected.start(bi as i32);
                        let result = session
                            .download_block(
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
                                &tracker,
                            )
                            .await;
                        (Some(session), asked, result)
                    };
                    let (session, asked, r) = tokio::select! {
                        biased;
                        () = cancel.cancelled() => (None, false, Err(DownloadError::Cancelled)),
                        out = work => out,
                    };
                    (bi, src, session, asked, r)
                });
            }

            if join.is_empty() {
                if pending.is_empty() {
                    break Ok(());
                }
                // 重读服务器列表并重配客户端：宿主后台刷新（含缓存版本失效重取）
                // 完成后，本轮即可用上修正后的服务器，无需重启。
                {
                    let manual = params
                        .db
                        .get_config("ed2k_server_list")
                        .await?
                        .unwrap_or_default();
                    let sub = params
                        .db
                        .get_config("ed2k_server_sub_cache")
                        .await?
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
                let mut merged: Vec<Source> = match server_res {
                    Ok(sources) => sources,
                    Err(error) => {
                        log_info!("[ed2k] server source query failed; trying Kad: {}", error);
                        Vec::new()
                    }
                };
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
                    match kad_res {
                        Ok(peers) => {
                            let mut seen: HashSet<Source> = merged.iter().copied().collect();
                            for peer in peers {
                                let src = Source::HighId(peer);
                                if seen.insert(src) {
                                    merged.push(src);
                                }
                            }
                        }
                        Err(DownloadError::Cancelled) => {
                            break 'outer Err(DownloadError::Cancelled);
                        }
                        Err(error) => log_info!("[ed2k] Kad source query failed: {}", error),
                    }
                }
                // 已被 integrity 拉黑的源不算可用源：否则每轮找源都会拿回同一批
                // 坏源、重置重试计数并立刻再找，任务永不收敛也无任何错误提示。
                let before = merged.len();
                merged.retain(|s| !integrity_bl.contains(s));
                let had_blacklisted = merged.len() < before;
                if !merged.is_empty() {
                    sources = merged;
                    backoff.clear();
                    lacks.clear();
                    source_retries = 0;
                    // 所有源都在重询冷却期内：等到最早一个到期再继续，不反复
                    // 登录服务器 / 查 Kad 空转。
                    let now = tokio::time::Instant::now();
                    let wake = sources
                        .iter()
                        .map(|src| match asked_at.get(src) {
                            Some(at) if !idle.contains_key(src) => *at + REASK_INTERVAL,
                            _ => now,
                        })
                        .min();
                    if let Some(wake) = wake
                        && wake > now
                    {
                        log_info!(
                            "[ed2k] task {} all {} sources asked recently; next slot request in {}s",
                            task_id,
                            sources.len(),
                            (wake - now).as_secs()
                        );
                        tokio::select! {
                            biased;
                            () = params.cancel_token.cancelled() => {
                                break 'outer Err(DownloadError::Cancelled)
                            }
                            () = tokio::time::sleep_until(wake) => {}
                        }
                    }
                } else {
                    source_retries += 1;
                    if source_retries < MAX_SOURCE_RETRIES
                        && let Err(journal_error) = crate::task_activity::record(
                            &params.db,
                            params.sink.as_ref(),
                            &task_id,
                            "retry",
                            format!(
                                "ED2K 找源为空，准备第 {}/{MAX_SOURCE_RETRIES} 次找源",
                                source_retries + 1
                            ),
                            None,
                        )
                        .await
                    {
                        crate::log_error!(
                            "[task-activity] failed to persist retry: {}",
                            journal_error
                        );
                    }
                    if source_retries >= MAX_SOURCE_RETRIES {
                        break Err(if had_blacklisted {
                            DownloadError::Ed2kIntegrity(
                                "all available sources served corrupt data".into(),
                            )
                        } else {
                            DownloadError::Ed2k("no sources found for this ed2k file".into())
                        });
                    }
                    if let Err(error) = params.db.update_task_status(&task_id, 5, "").await {
                        break 'outer Err(error.into());
                    }
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

            // 与 cancel 竞速：暂停/删除不必等到某个块任务自然结束；JoinSet 随
            // 作用域结束 drop 时会 abort 仍在连接/握手/排队的块任务。
            let joined_next = tokio::select! {
                biased;
                () = params.cancel_token.cancelled() => break Err(DownloadError::Cancelled),
                j = join.join_next() => j,
            };
            let Some(joined) = joined_next else { continue };
            // 不额外等待时间窗：合并此刻已完成的块，立即同步数据并提交。
            ready.extend(
                std::iter::once(joined)
                    .chain(std::iter::from_fn(|| join.try_join_next()))
                    .filter_map(Result::ok)
                    .map(|(index, source, session, asked, result)| {
                        let checked = result.map(|md4| {
                            if is_single {
                                md4 == link.root_hash
                            } else {
                                hashset_cache
                                    .get()
                                    .and_then(|hashes| hashes.get(index as usize))
                                    == Some(&md4)
                            }
                        });
                        (index, source, session, asked, checked)
                    }),
            );
            updates.clear();
            updates.extend(ready.iter().filter_map(|(index, _, _, _, result)| {
                if matches!(result, Ok(true)) {
                    let (start, end) = hash::part_span(*index, total_bytes, part_size);
                    Some((*index, BLOCK_VERIFIED, (end - start) as i64, false))
                } else {
                    None
                }
            }));
            if !updates.is_empty() {
                let hashset = if is_single || hashset_persisted {
                    None
                } else {
                    hashset_cache.get().map(Vec::as_slice)
                };
                if let Err(error) =
                    persist_verified_blocks(&params.db, &task_id, &temp_path, &updates, hashset)
                        .await
                {
                    break Err(error);
                }
                hashset_persisted = true;
                match blocks.lock() {
                    Ok(mut snapshot) => {
                        for &(index, _, _, _) in &updates {
                            snapshot.set_verified(index, true, total_bytes, part_size);
                        }
                    }
                    Err(_) => {
                        break Err(DownloadError::Ed2k("block snapshot lock poisoned".into()));
                    }
                }
            }
            for (bi, src, session, asked, res) in ready.drain(..) {
                busy.remove(&src);
                if asked {
                    asked_at.insert(src, tokio::time::Instant::now());
                }
                // 块校验通过或对端只是缺这一块：连接仍持有上传槽，留给下一个块。
                let reusable = match &res {
                    Ok(verified) => *verified,
                    Err(DownloadError::Ed2k(message)) => message == peer::PEER_LACKS_BLOCK,
                    Err(_) => false,
                };
                if reusable && let Some(session) = session {
                    idle.insert(src, session);
                }
                // 未校验的部分字节不作为恢复状态。
                if let Ok(mut map) = progress.lock() {
                    map.remove(&bi);
                }
                match res {
                    Ok(ok) => {
                        if !ok {
                            // 块 MD4 不匹配 = 投毒/损坏 → 拉黑该源，块重下。
                            if let Err(error) = params
                                .db
                                .update_ed2k_blocks(&task_id, &[(bi, BLOCK_MISSING, 0, true)])
                                .await
                            {
                                break 'download Err(error.into());
                            }
                            pending.push_back(bi);
                            if strikes.get(&bi).copied().unwrap_or(0) + 1 < BLOCK_MAX_RETRIES
                                && let Err(journal_error) = crate::task_activity::record(
                                    &params.db,
                                    params.sink.as_ref(),
                                    &task_id,
                                    "retry",
                                    format!("ED2K 块 {bi} 校验失败，将更换源重试"),
                                    None,
                                )
                                .await
                            {
                                crate::log_error!(
                                    "[task-activity] failed to persist retry: {}",
                                    journal_error
                                );
                            }
                            *strikes.entry(bi).or_insert(0) += 1;
                            integrity_bl.insert(src);
                            if strikes.get(&bi).copied().unwrap_or(0) >= BLOCK_MAX_RETRIES {
                                break 'download Err(DownloadError::Ed2kIntegrity(format!(
                                    "block {bi} irrecoverable after {BLOCK_MAX_RETRIES} tries"
                                )));
                            }
                        }
                    }
                    Err(source) => {
                        if matches!(source, DownloadError::Cancelled) {
                            break 'download Err(DownloadError::Cancelled);
                        }
                        // 未校验的部分数据不作为恢复状态，不重复写入 missing。
                        pending.push_back(bi);
                        // 对端位图声明缺该块：只对（源, 块）退避，不牵连其它块，
                        // 也不刷重试日志。
                        if matches!(&source, DownloadError::Ed2k(m) if m == peer::PEER_LACKS_BLOCK)
                        {
                            lacks.insert((src, bi));
                            continue;
                        }
                        if let Err(journal_error) = crate::task_activity::record(
                            &params.db,
                            params.sink.as_ref(),
                            &task_id,
                            "retry",
                            format!("ED2K 块 {bi} 从 {src:?} 读取失败，将重试：{source}"),
                            None,
                        )
                        .await
                        {
                            crate::log_error!(
                                "[task-activity] failed to persist retry: {}",
                                journal_error
                            );
                        }
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

    drop(progress_guard);

    match outcome {
        Ok(total) => {
            let avoid: HashSet<String> = params
                .db
                .list_active_sibling_file_names(&params.save_dir, &task_id)
                .await?
                .into_iter()
                .map(|n| n.to_lowercase())
                .collect();
            let final_name =
                finalize_rename(&temp_path, save_dir, &file_name, &params.overwrite, &avoid)
                    .await?;
            persist_final_name(params, &task_id, &file_name, &final_name, total).await?;
            Ok((total, final_name))
        }
        Err(e) => Err(e),
    }
}

/// Persist the actual landed filename before publishing completion.
async fn persist_final_name(
    params: &DownloadParams,
    task_id: &str,
    planned: &str,
    final_name: &str,
    total_bytes: i64,
) -> Result<(), DownloadError> {
    if final_name != planned {
        params
            .db
            .update_task_file_info(task_id, final_name, total_bytes)
            .await?;
    }
    Ok(())
}

/// 进度旁路任务的 RAII 守卫：任何退出路径都终止内存快照上报。
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 单次选源时在待下载队列头部检视的块数上限。
const PICK_WINDOW: usize = 64;

/// 轮转为块 `block` 选取一个不在 `backoff ∪ integrity_bl ∪ busy` 且未声明缺该块的源；
/// 无可用源返回 `None`。
fn pick_source(
    sources: &[Source],
    backoff: &HashSet<Source>,
    integrity_bl: &HashSet<Source>,
    busy: &HashSet<Source>,
    lacks: &HashSet<(Source, u64)>,
    block: u64,
    round_robin: &mut usize,
) -> Option<Source> {
    if sources.is_empty() {
        return None;
    }
    for _ in 0..sources.len() {
        let idx = *round_robin % sources.len();
        *round_robin = round_robin.wrapping_add(1);
        let src = sources[idx];
        if !backoff.contains(&src)
            && !integrity_bl.contains(&src)
            && !busy.contains(&src)
            && !lacks.contains(&(src, block))
        {
            return Some(src);
        }
    }
    None
}

/// 旁路进度任务：只读内存快照，保留完整分段 wire 以兼容现有客户端。
/// 锁释放后再发送，Future 保持 Send。
/// 已校验块优先于残留的实时字节，避免完成交接时重复计数。
fn snapshot_progress(
    blocks: &BlockSnapshot,
    live: &HashMap<u64, i64>,
    total_bytes: u64,
    part_size: u64,
    tracker: &TransferTracker,
) -> (i64, Vec<SegmentProgressInfo>) {
    let mut downloaded = blocks.verified_bytes;
    let mut details = Vec::with_capacity(blocks.verified.len());
    for (index, verified) in blocks.verified.iter().enumerate() {
        let (start, end) = hash::part_span(index as u64, total_bytes, part_size);
        let bytes = if *verified {
            (end - start) as i64
        } else {
            let bytes = live.get(&(index as u64)).copied().unwrap_or(0);
            downloaded += bytes;
            bytes
        };
        details.push(SegmentProgressInfo {
            index: index as i32,
            start_byte: start as i64,
            end_byte: end as i64 - 1,
            downloaded_bytes: bytes,
            active: Some(tracker.is_active(index as i32)),
        });
    }
    (downloaded, details)
}

struct ProgressReporterContext {
    blocks: Arc<StdMutex<BlockSnapshot>>,
    progress_tx: tokio::sync::mpsc::Sender<ProgressUpdate>,
    task_id: String,
    total_bytes: u64,
    part_size: u64,
    progress: Arc<StdMutex<HashMap<u64, i64>>>,
    tracker: TransferTracker,
    connected: TransferTracker,
    concurrency_limit: Arc<AtomicU32>,
}

fn spawn_progress_reporter(context: ProgressReporterContext) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ProgressReporterContext {
            blocks,
            progress_tx,
            task_id,
            total_bytes,
            part_size,
            progress,
            tracker,
            connected,
            concurrency_limit,
        } = context;
        loop {
            tokio::time::sleep(PROGRESS_TICK).await;
            let sample_sequence = crate::transfer_activity::next_sample_sequence();
            let sampled_at_ms = chrono::Utc::now().timestamp_millis();
            let (downloaded, segment_details) = {
                let Ok(snapshot) = blocks.lock() else {
                    continue;
                };
                let Ok(live) = progress.lock() else { continue };
                snapshot_progress(&snapshot, &live, total_bytes, part_size, &tracker)
            };
            let runtime = TaskRuntime {
                task_id: task_id.clone(),
                sampled_at_ms,
                sample_sequence,
                active_transfers: Some(tracker.active()),
                connected_peers: Some(connected.active()),
                parallelism_limit: match concurrency_limit.load(Ordering::Relaxed) {
                    0 => None,
                    n => Some(n),
                },
                total_bytes: total_bytes as i64,
                segments: segment_details
                    .iter()
                    .map(|s| TaskSegment {
                        index: s.index,
                        start_byte: s.start_byte,
                        end_byte: s.end_byte,
                        downloaded_bytes: s.downloaded_bytes,
                        active: s.active,
                    })
                    .collect(),
                source_bytes: None,
            };
            if progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: total_bytes as i64,
                    status: 1,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: Some(segment_details),
                    runtime: Some(runtime),
                    ..Default::default()
                })
                .await
                .is_err()
            {
                tracing::debug!("ED2K progress receiver closed during shutdown");
                break;
            }
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

    if hash::is_single_block(total_bytes, part_size) {
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
    let db_hashes: Vec<[u8; 16]> = blob.as_chunks::<16>().0.to_vec();

    let bad: Vec<_> = disk_hashes
        .iter()
        .zip(db_hashes.iter())
        .enumerate()
        .filter(|(_, (disk, expected))| disk != expected)
        .map(|(index, _)| (index as u64, BLOCK_MISSING, 0, false))
        .collect();
    if !bad.is_empty() {
        db.update_ed2k_blocks(task_id, &bad).await?;
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
        db.reset_ed2k_blocks(task_id).await?;
        return Err(DownloadError::Ed2kIntegrity(
            "recomputed root mismatch (corrupt hashset table?)".into(),
        ));
    }
    Ok(())
}

/// 完成落盘：`sync_all` + 原子占名 rename temp→final，返回实际落盘的文件名。
///
/// 占名语义见 [`crate::downloader::claim_final_name`]：不覆盖同名旧文件，
/// overwrite 模式只对原名删除旧文件后重试一次，其余重新 dedup 换名（避开
/// 兄弟任务已预订的 `avoid`）。
async fn finalize_rename(
    temp: &Path,
    save_dir: &Path,
    name: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    avoid: &HashSet<String>,
) -> Result<String, DownloadError> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temp)
        .await
        .map_err(DownloadError::Io)?;
    file.sync_all().await.map_err(DownloadError::Io)?;
    crate::downloader::claim_final_name(temp, save_dir, name, overwrite, avoid).await
}

/// 确定本任务使用的文件名：manager 传入的名字（已 dedup / 用户自定义 / 已预订临时路径）
/// 优先，为空才回退链接内的名字。
fn resolve_file_name(param_name: &str, link_name: &str) -> String {
    let name = if param_name.trim().is_empty() {
        link_name
    } else {
        param_name
    };
    crate::naming::sanitize_filename(name)
}

/// Migrate a matching legacy temporary file without losing its verified blocks.
/// Missing files are expected; other I/O errors must not start a second download.
async fn adopt_legacy_temp(
    save_dir: &Path,
    link_name: &str,
    name: &str,
    total_bytes: u64,
    sibling_names: &HashSet<String>,
) -> Result<(), DownloadError> {
    if name == link_name || sibling_names.contains(&link_name.to_lowercase()) {
        return Ok(());
    }
    let new_temp = save_dir.join(format!("{name}{}", crate::downloader::TEMP_EXT));
    let legacy_temp = save_dir.join(format!("{link_name}{}", crate::downloader::TEMP_EXT));
    match tokio::fs::metadata(&new_temp).await {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match tokio::fs::metadata(&legacy_temp).await {
        Ok(metadata) if metadata.is_file() && metadata.len() == total_bytes => {
            tokio::fs::rename(&legacy_temp, &new_temp)
                .await
                .map_err(DownloadError::Io)?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
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

    #[test]
    fn file_name_prefers_manager_name() {
        assert_eq!(
            super::resolve_file_name("movie (1).iso", "movie.iso"),
            "movie (1).iso"
        );
        assert_eq!(super::resolve_file_name("", "movie.iso"), "movie.iso");
    }

    #[test]
    fn pick_source_skips_blacklisted_and_block_lacking_sources() {
        use std::collections::HashSet;
        let a = super::Source::LowId(1);
        let b = super::Source::LowId(2);
        let c = super::Source::LowId(3);
        let sources = [a, b, c];
        let backoff: HashSet<_> = [a].into_iter().collect();
        let bl: HashSet<_> = [b].into_iter().collect();
        let mut rr = 0;
        let none = HashSet::new();
        let none_busy = HashSet::new();
        assert_eq!(
            super::pick_source(&sources, &backoff, &bl, &none_busy, &none, 0, &mut rr),
            Some(c)
        );
        // c 声明缺块 0：块 0 无源可用，块 1 仍可用 c。
        let lacks: HashSet<_> = [(c, 0u64)].into_iter().collect();
        assert_eq!(
            super::pick_source(&sources, &backoff, &bl, &none_busy, &lacks, 0, &mut rr),
            None
        );
        assert_eq!(
            super::pick_source(&sources, &backoff, &bl, &none_busy, &lacks, 1, &mut rr),
            Some(c)
        );
        // c 正在传输：不再为其它块选它。
        let busy = HashSet::from([c]);
        assert_eq!(
            super::pick_source(&sources, &backoff, &bl, &busy, &none, 1, &mut rr),
            None
        );
    }

    #[tokio::test]
    async fn finalize_rename_never_overwrites_existing_file() {
        use std::collections::HashSet;
        let dir = std::env::temp_dir().join(format!("ed2k_finalize_{}", std::process::id()));
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
        let Ok(()) = tokio::fs::create_dir_all(&dir).await else {
            panic!("create dir");
        };
        tokio::fs::write(dir.join("movie.iso"), b"user data")
            .await
            .unwrap_or_else(|error| panic!("test fixture write failed: {error}"));
        let temp = dir.join(format!("movie.iso{}", crate::downloader::TEMP_EXT));
        tokio::fs::write(&temp, b"downloaded")
            .await
            .unwrap_or_else(|error| panic!("test fixture write failed: {error}"));

        let Ok(chosen) = super::finalize_rename(
            &temp,
            &dir,
            "movie.iso",
            &crate::file_exists::OverwritePolicy::Never,
            &HashSet::new(),
        )
        .await
        else {
            panic!("finalize failed");
        };
        assert_ne!(chosen, "movie.iso");
        assert_eq!(
            tokio::fs::read(dir.join("movie.iso"))
                .await
                .unwrap_or_default(),
            b"user data"
        );
        assert_eq!(
            tokio::fs::read(dir.join(&chosen)).await.unwrap_or_default(),
            b"downloaded"
        );

        // overwrite 模式：原名被替换。
        let temp2 = dir.join(format!("movie.iso{}", crate::downloader::TEMP_EXT));
        tokio::fs::write(&temp2, b"second")
            .await
            .unwrap_or_else(|error| panic!("test fixture write failed: {error}"));
        let Ok(chosen2) = super::finalize_rename(
            &temp2,
            &dir,
            "movie.iso",
            &crate::file_exists::OverwritePolicy::Any,
            &HashSet::new(),
        )
        .await
        else {
            panic!("overwrite finalize failed");
        };
        assert_eq!(chosen2, "movie.iso");
        assert_eq!(
            tokio::fs::read(dir.join("movie.iso"))
                .await
                .unwrap_or_default(),
            b"second"
        );
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

            if let Err(error) = std::fs::remove_dir_all(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
            }
        }
    }

    // --- peer 层：>4GB 文件的 I64 请求必须走 eMule 扩展协议头，否则对端不发数据 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_large_file_requests_use_emule_protocol() {
        let data: Vec<u8> = (0..321u32).map(|i| (i % 256) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, hash::PART_SIZE);
        let peer = match MockPeer::spawn(data.clone(), hash::PART_SIZE, PeerFault::None).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };
        let dir = it_scratch_dir("peer_large_proto");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            download_block_from_peer(
                peer.peer_addr(),
                &root,
                0,
                total,
                hash::PART_SIZE,
                true,
                &dest,
                &new_cancel(),
                &no_limit(),
                fresh_hashset_cache(),
                empty_progress(),
            ),
        )
        .await;
        match result {
            Ok(Ok((_, md4))) => assert_eq!(md4, root),
            Ok(Err(e)) => panic!("large-file download failed: {e:?}"),
            Err(_) => panic!("peer never answered I64 part request"),
        }
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_body_activity_starts_after_negotiation_and_stops_at_eof() {
        let data = b"actual peer body".to_vec();
        let total = data.len() as u64;
        let part_size = hash::PART_SIZE;
        let root = root_hash(&data, part_size);
        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::DelayBody).await {
            Ok(peer) => peer,
            Err(e) => panic!("mock peer: {e}"),
        };
        let dir = it_scratch_dir("peer_activity");
        let dest = dir.join("body.tmp");
        prep_dest(&dest, total).await;
        let tracker = crate::transfer_activity::TransferTracker::new();
        let observe = tracker.clone();
        let task = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(peer.addr)
                .await
                .map_err(DownloadError::Io)?;
            let cache = fresh_hashset_cache();
            let progress = empty_progress();
            let cancel = new_cancel();
            let limiter = no_limit();
            crate::ed2k::peer::download_block_on_stream(
                stream, &root, 0, total, part_size, false, &dest, &cancel, &limiter, &cache,
                &progress, &tracker,
            )
            .await
        });
        let observed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if observe.is_active(0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(observed.is_ok(), "peer body read never became active");
        assert_eq!(observe.active(), 1);
        let result = match task.await {
            Ok(result) => result,
            Err(e) => panic!("peer task panicked: {e}"),
        };
        let digest = match result {
            Ok(digest) => digest,
            Err(e) => panic!("peer transfer failed: {e}"),
        };
        assert_eq!(digest, hash::hash_part(&data));
        assert_eq!(observe.active(), 0);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    // --- peer 层：一条连接连续下多个块，只向对端申请一次上传槽 ---
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn peer_session_downloads_all_blocks_with_one_slot_request() {
        let part_size: u64 = 256;
        let data: Vec<u8> = (0..800u32).map(|i| (i % 251) as u8).collect();
        let total = data.len() as u64;
        let root = root_hash(&data, part_size);
        let peer = match MockPeer::spawn(data.clone(), part_size, PeerFault::None).await {
            Ok(p) => p,
            Err(e) => panic!("spawn mock peer failed: {e}"),
        };
        let dir = it_scratch_dir("peer_session");
        let dest = dir.join("out.tmp");
        prep_dest(&dest, total).await;

        let stream = match tokio::net::TcpStream::connect(peer.addr).await {
            Ok(stream) => stream,
            Err(e) => panic!("connect mock peer failed: {e}"),
        };
        let cancel = new_cancel();
        let mut session = match super::PeerSession::open(stream, &root, &cancel).await {
            Ok(session) => session,
            Err(e) => panic!("open session failed: {:?}", e.source),
        };
        let cache = fresh_hashset_cache();
        let progress = empty_progress();
        let limiter = no_limit();
        let tracker = super::TransferTracker::new();
        for block_index in 0..hash::part_count(total, part_size) {
            let md4 = match session
                .download_block(
                    &root,
                    block_index,
                    total,
                    part_size,
                    false,
                    &dest,
                    &cancel,
                    &limiter,
                    &cache,
                    &progress,
                    &tracker,
                )
                .await
            {
                Ok(md4) => md4,
                Err(e) => panic!("block {block_index} failed: {e:?}"),
            };
            let (s, e) = hash::part_span(block_index, total, part_size);
            assert_eq!(md4, hash::hash_part(&data[s as usize..e as usize]));
        }
        assert_eq!(peer.slot_requests.load(Ordering::Relaxed), 1);
        let on_disk = match tokio::fs::read(&dest).await {
            Ok(b) => b,
            Err(e) => panic!("read dest failed: {e}"),
        };
        assert_eq!(on_disk, data);

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
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

        if let Err(error) = std::fs::remove_dir_all(&dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    #[test]
    fn ed2k_space_reservation_reuses_allocated_temp_bytes() {
        assert_eq!(super::remaining_disk_bytes(100, 30, None), 70);
        assert_eq!(super::remaining_disk_bytes(100, 30, Some(80)), 20);
        assert_eq!(super::remaining_disk_bytes(100, 30, Some(100)), 0);
        assert_eq!(super::remaining_disk_bytes(100, 80, Some(30)), 20);
        assert_eq!(super::remaining_disk_bytes(100, 30, Some(128)), 0);
    }

    #[test]
    fn ed2k_disk_precheck_covers_margin_and_unknown_space() {
        let required = 1024;
        let threshold = required + crate::disk_space::PRECHECK_MARGIN;
        assert!(super::check_download_space(Some(threshold - 1), required).is_err());
        assert!(super::check_download_space(Some(threshold), required).is_ok());
        assert!(super::check_download_space(None, required).is_ok());
        assert!(super::check_download_space(Some(0), 0).is_ok());
    }

    #[tokio::test]
    async fn ed2k_disk_shortage_creates_neither_temp_nor_block_rows() {
        let (db, dir) = open_it_db("space_gate").await;
        let task_id = "space-gate";
        let total = hash::MAX_FILE_SIZE;
        insert_it_task(&db, task_id, total).await;
        let temp = dir.join("large.part");
        let result =
            super::prepare_temp_and_blocks(&db, task_id, &temp, total, hash::PART_SIZE, Some(0))
                .await;
        assert!(matches!(result, Err(DownloadError::Ed2k(_))));
        assert_eq!(
            tokio::fs::metadata(&temp)
                .await
                .expect_err("no temp file")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(
            db.load_ed2k_blocks(task_id)
                .await
                .expect("load blocks")
                .is_empty()
        );
        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    #[tokio::test]
    async fn ed2k_resume_snapshot_skips_durable_blocks_and_resets_recreated_temp() {
        let (db, dir) = open_it_db("resume_snapshot").await;
        let task_id = "resume-snapshot";
        let part_size = 32;
        let total = 65;
        insert_it_task(&db, task_id, total).await;
        let temp = dir.join("resume.part");
        super::prepare_temp_and_blocks(&db, task_id, &temp, total, part_size, None)
            .await
            .expect("prepare");
        let data = vec![7u8; total as usize];
        tokio::fs::write(&temp, &data).await.expect("write data");
        let hashes = [
            hash::hash_part(&data[..32]),
            hash::hash_part(&data[32..64]),
            hash::hash_part(&data[64..]),
        ];
        super::persist_verified_blocks(
            &db,
            task_id,
            &temp,
            &[
                (0, BLOCK_VERIFIED, 32, false),
                (2, BLOCK_VERIFIED, 1, false),
            ],
            Some(&hashes),
        )
        .await
        .expect("commit verified");
        let restored = super::prepare_temp_and_blocks(
            &db,
            task_id,
            &temp,
            total,
            part_size,
            Some(32 + crate::disk_space::PRECHECK_MARGIN),
        )
        .await
        .expect("resume using remaining space");
        assert_eq!(restored.verified_bytes, 33);
        assert_eq!(restored.pending().into_iter().collect::<Vec<_>>(), vec![1]);
        let blob = db
            .load_ed2k_hashset(task_id)
            .await
            .expect("hashset")
            .expect("present");
        assert_eq!(blob.as_chunks::<16>().0, &hashes);

        tokio::fs::write(&temp, b"invalid-size")
            .await
            .expect("replace temp");
        let reset = super::prepare_temp_and_blocks(&db, task_id, &temp, total, part_size, None)
            .await
            .expect("recreate");
        assert_eq!(reset.verified_bytes, 0);
        assert_eq!(
            reset.pending().into_iter().collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(
            db.load_ed2k_blocks(task_id)
                .await
                .expect("reset rows")
                .iter()
                .all(|(_, state, bytes, _)| *state == BLOCK_MISSING && *bytes == 0)
        );
        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    #[tokio::test]
    async fn ed2k_sync_failure_never_persists_verified_markers() {
        let (db, dir) = open_it_db("sync_order").await;
        let task_id = "sync-order";
        insert_it_task(&db, task_id, 32).await;
        db.init_ed2k_blocks(task_id, 1).await.expect("init");
        let result = super::persist_verified_blocks(
            &db,
            task_id,
            &dir.join("missing.part"),
            &[(0, BLOCK_VERIFIED, 32, false)],
            Some(&[[7u8; 16]]),
        )
        .await;
        assert!(matches!(result, Err(DownloadError::Io(_))));
        assert_eq!(
            db.load_ed2k_blocks(task_id).await.expect("blocks"),
            vec![(0, BLOCK_MISSING, 0, 0)]
        );
        assert!(
            db.load_ed2k_hashset(task_id)
                .await
                .expect("hashset")
                .is_none()
        );
        drop(db);
        if let Err(error) = tokio::fs::remove_dir_all(dir).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            crate::log_warn!("[ED2K tests] fixture cleanup failed: {}", error);
        }
    }

    #[tokio::test]
    async fn ed2k_reporter_samples_memory_through_completion_and_invalidation() {
        let total_bytes = 35;
        let part_size = 32;
        let blocks = Arc::new(StdMutex::new(super::BlockSnapshot::from_rows(
            total_bytes,
            part_size,
            &[(0, BLOCK_VERIFIED, 32, 0), (1, BLOCK_MISSING, 2, 0)],
        )));
        let progress = empty_progress();
        progress.lock().expect("progress").insert(1, 2);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let guard = super::AbortOnDrop(super::spawn_progress_reporter(
            super::ProgressReporterContext {
                blocks: Arc::clone(&blocks),
                progress_tx: tx,
                task_id: "memory-only".into(),
                total_bytes,
                part_size,
                progress: Arc::clone(&progress),
                tracker: crate::transfer_activity::TransferTracker::new(),
                connected: crate::transfer_activity::TransferTracker::new(),
                concurrency_limit: Arc::new(AtomicU32::new(2)),
            },
        ));
        let sample = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("first sample")
            .expect("sample");
        assert_eq!(sample.downloaded_bytes, 34);
        let details = sample.segment_details.expect("segments");
        assert_eq!(
            (
                details[1].start_byte,
                details[1].end_byte,
                details[1].downloaded_bytes
            ),
            (32, 34, 2)
        );
        blocks
            .lock()
            .expect("blocks")
            .set_verified(1, true, total_bytes, part_size);
        let sample = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("complete sample")
            .expect("sample");
        assert_eq!(
            sample.downloaded_bytes, 35,
            "live bytes must not double-count completed blocks"
        );
        blocks
            .lock()
            .expect("blocks")
            .set_verified(0, false, total_bytes, part_size);
        let sample = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("invalidated sample")
            .expect("sample");
        assert_eq!(sample.downloaded_bytes, 3);
        assert_eq!(
            blocks
                .lock()
                .expect("blocks")
                .pending()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![0]
        );
        drop(guard);
    }
}
