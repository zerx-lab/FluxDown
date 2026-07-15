//! `HostSelection` 实现 —— 把现状 HLS 的 oneshot+60s 超时逻辑与 BT 的轮询
//! 逻辑集中改写为对 `SelectionOutcome` 三态的构造。
//!
//! - HLS:超时 → `TimedOutDefaulted`(自动选最高带宽变体)。
//! - BT:桌面 GUI 场景保留"允许长时间等待"的行为(有真实用户会响应),
//!   用一个较长但有限的超时包裹以避免任务永久卡死。具体超时时长沿用与
//!   HLS 相同的 60 秒(暂定默认值,产品认为不合适可后续调整,不影响架构)。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use fluxdown_engine::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption};
use fluxdown_engine::selection::{HostSelection, SelectionOutcome};
use rinf::RustSignal;
use tokio::sync::oneshot;

use crate::logger::log_info;
use crate::signals;

/// 桌面 GUI 场景下,BT 文件选择"无限等待"改为"有限但较长"的默认超时。
/// 与 HLS 画质选择的超时常量保持一致——沿用现状 HLS 60 秒常量,具体秒数
/// 待产品确认,当前作为实现阶段默认值。
const BT_SELECTION_TIMEOUT: Duration = Duration::from_secs(60);

/// 实现 [`HostSelection`],把 HLS/BT 选择请求经 `RustSignal` 发送给 Dart,
/// 并维护一份按 `task_id` 索引的等待表,供 `provide_*` 方法投递答案时查找。
///
/// 现状机制(`download_manager.rs` 的 `hls_quality_tx: Option<oneshot::Sender<i32>>`)
/// 是嵌在 `DownloadManager` 每个任务条目里的字段;此结构体不持有
/// `active_tasks`,因此自行维护等待表。
pub struct RinfHostSelection {
    hls_pending: Mutex<HashMap<String, oneshot::Sender<i32>>>,
    bt_pending: Mutex<HashMap<String, oneshot::Sender<Vec<i32>>>>,
    variant_pending: Mutex<HashMap<String, oneshot::Sender<i32>>>,
}

impl RinfHostSelection {
    pub fn new() -> Self {
        Self {
            hls_pending: Mutex::new(HashMap::new()),
            bt_pending: Mutex::new(HashMap::new()),
            variant_pending: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for RinfHostSelection {
    fn default() -> Self {
        Self::new()
    }
}

/// 取出锁内容,`Mutex` 中毒时回退到内部值(桌面单进程场景不会真正中毒,
/// 此处仅做防御,避免 panic)。
fn lock_or_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[async_trait::async_trait]
impl HostSelection for RinfHostSelection {
    async fn select_hls_quality(
        &self,
        task_id: &str,
        options: &[HlsQualityOption],
        timeout: Duration,
    ) -> SelectionOutcome<i32> {
        let best_default = options
            .iter()
            .enumerate()
            .max_by_key(|(_, o)| o.bandwidth)
            .map(|(i, _)| i as i32)
            .unwrap_or(0);

        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.hls_pending).insert(task_id.to_string(), tx);

        signals::HlsQualityOptions {
            task_id: task_id.to_string(),
            options: options.iter().cloned().map(Into::into).collect(),
        }
        .send_signal_to_dart();

        log_info!(
            "[rinf-selection] task {} sent {} HLS quality options, waiting (timeout={}s)",
            task_id,
            options.len(),
            timeout.as_secs()
        );

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(idx)) => SelectionOutcome::UserChose(idx),
            Ok(Err(_)) => {
                // Channel closed (sender dropped) without an answer — treat like a timeout.
                log_info!(
                    "[rinf-selection] task {} HLS selection channel closed, defaulting",
                    task_id
                );
                SelectionOutcome::TimedOutDefaulted(best_default)
            }
            Err(_) => {
                log_info!(
                    "[rinf-selection] task {} HLS selection timed out ({}s), defaulting",
                    task_id,
                    timeout.as_secs()
                );
                SelectionOutcome::TimedOutDefaulted(best_default)
            }
        };
        // 无论哪种结果都必须从等待表移除该 task_id——不清理会导致 map
        // 无界增长,或后续 provide_hls_selection 尝试 send 到已丢弃的 Receiver。
        lock_or_recover(&self.hls_pending).remove(task_id);
        outcome
    }

    async fn select_bt_files(
        &self,
        task_id: &str,
        files: &[BtFileEntry],
        timeout: Option<Duration>,
    ) -> SelectionOutcome<Vec<i32>> {
        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.bt_pending).insert(task_id.to_string(), tx);

        signals::BtFilesInfo {
            task_id: task_id.to_string(),
            total_bytes: files.iter().map(|f| f.size).sum(),
            files: files.iter().cloned().map(Into::into).collect(),
        }
        .send_signal_to_dart();

        log_info!(
            "[rinf-selection] task {} sent {} BT files, waiting for selection",
            task_id,
            files.len()
        );

        // 桌面 GUI：`timeout` 为 None 时应用产品级默认超时（见模块顶部常量），
        // 而不是真正无限等待——避免任务在用户忘记响应对话框时永久卡死。
        let effective_timeout = timeout.unwrap_or(BT_SELECTION_TIMEOUT);

        let outcome = match tokio::time::timeout(effective_timeout, rx).await {
            Ok(Ok(indices)) => SelectionOutcome::UserChose(indices),
            Ok(Err(_)) => {
                log_info!(
                    "[rinf-selection] task {} BT selection channel closed, defaulting to all files",
                    task_id
                );
                SelectionOutcome::TimedOutDefaulted(Vec::new())
            }
            Err(_) => {
                log_info!(
                    "[rinf-selection] task {} BT selection timed out ({}s), defaulting to all files",
                    task_id,
                    effective_timeout.as_secs()
                );
                SelectionOutcome::TimedOutDefaulted(Vec::new())
            }
        };
        lock_or_recover(&self.bt_pending).remove(task_id);
        outcome
    }

    async fn select_resolve_variant(
        &self,
        task_id: &str,
        options: &[ResolveVariantOption],
        default_index: i32,
        timeout: Duration,
    ) -> SelectionOutcome<i32> {
        let (tx, rx) = oneshot::channel();
        lock_or_recover(&self.variant_pending).insert(task_id.to_string(), tx);

        signals::ResolveVariantSelectionRequest {
            task_id: task_id.to_string(),
            default_index,
            options: options.iter().cloned().map(Into::into).collect(),
        }
        .send_signal_to_dart();

        log_info!(
            "[rinf-selection] task {} sent {} resolve variant options, waiting (timeout={}s)",
            task_id,
            options.len(),
            timeout.as_secs()
        );

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(idx)) => SelectionOutcome::UserChose(idx),
            Ok(Err(_)) => {
                // Channel closed (sender dropped) without an answer — treat like a timeout.
                log_info!(
                    "[rinf-selection] task {} resolve variant selection channel closed, defaulting",
                    task_id
                );
                SelectionOutcome::TimedOutDefaulted(default_index)
            }
            Err(_) => {
                log_info!(
                    "[rinf-selection] task {} resolve variant selection timed out ({}s), defaulting",
                    task_id,
                    timeout.as_secs()
                );
                SelectionOutcome::TimedOutDefaulted(default_index)
            }
        };
        // 无论哪种结果都必须从等待表移除该 task_id——不清理会导致 map
        // 无界增长,或后续 provide_variant_selection 尝试 send 到已丢弃的 Receiver。
        lock_or_recover(&self.variant_pending).remove(task_id);
        outcome
    }

    fn provide_hls_selection(&self, task_id: &str, selected_index: i32) {
        if let Some(tx) = lock_or_recover(&self.hls_pending).remove(task_id) {
            // `send` 失败(Receiver 已被丢弃,例如已超时)静默忽略，不 panic。
            let _ = tx.send(selected_index);
        } else {
            log_info!(
                "[rinf-selection] no pending HLS selection for task {}",
                task_id
            );
        }
    }

    fn provide_bt_selection(&self, task_id: &str, selected_indices: Vec<i32>) {
        if let Some(tx) = lock_or_recover(&self.bt_pending).remove(task_id) {
            let _ = tx.send(selected_indices);
        } else {
            log_info!(
                "[rinf-selection] no pending BT selection for task {}",
                task_id
            );
        }
    }

    fn provide_variant_selection(&self, task_id: &str, selected_index: i32) {
        if let Some(tx) = lock_or_recover(&self.variant_pending).remove(task_id) {
            let _ = tx.send(selected_index);
        } else {
            log_info!(
                "[rinf-selection] no pending resolve variant selection for task {}",
                task_id
            );
        }
    }
}
