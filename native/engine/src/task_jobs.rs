//! 按任务登记的后台作业（BT 完整性校验、文件哈希、BT 数据搬移）：同一任务同类作业同时只跑
//! 一个，删除任务 / 引擎关停时取消。凭据释放即取消并注销。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio_util::sync::CancellationToken;

/// 在途作业登记表；每类作业各持一张。
#[derive(Clone, Default)]
pub(crate) struct TaskJobs {
    running: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl TaskJobs {
    /// 登记一次作业；该任务已有同类作业在跑时返回 `None`。
    pub(crate) fn begin(&self, task_id: &str) -> Option<TaskJobGuard> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.contains_key(task_id) {
            return None;
        }
        let token = CancellationToken::new();
        running.insert(task_id.to_owned(), token.clone());
        Some(TaskJobGuard {
            jobs: self.clone(),
            task_id: task_id.to_owned(),
            token,
        })
    }

    /// 该任务是否有本类作业在跑。
    #[cfg(any(test, feature = "bt"))]
    pub(crate) fn is_running(&self, task_id: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(task_id)
    }

    /// 取消该任务的在途作业（没有则无操作）。
    pub(crate) fn cancel(&self, task_id: &str) {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = running.get(task_id) {
            token.cancel();
        }
    }

    /// 取消全部在途作业。
    pub(crate) fn cancel_all(&self) {
        let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        for token in running.values() {
            token.cancel();
        }
    }
}

/// 作业登记凭据：释放（完成或调用方丢弃 future）即取消并注销。
pub(crate) struct TaskJobGuard {
    jobs: TaskJobs,
    task_id: String,
    token: CancellationToken,
}

impl TaskJobGuard {
    /// 本次作业的取消令牌（阻塞线程按块轮询）。
    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for TaskJobGuard {
    fn drop(&mut self) {
        self.token.cancel();
        self.jobs
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::TaskJobs;
    /// 同一任务同时只允许一个作业；凭据释放后可再次开始；取消会传到在途作业。
    #[test]
    fn registry_rejects_duplicates_and_propagates_cancel() {
        let jobs = TaskJobs::default();
        let first = jobs.begin("t").expect("first");
        assert!(jobs.begin("t").is_none());
        assert!(jobs.is_running("t") && !jobs.is_running("u"));
        let other = jobs.begin("u").expect("other");
        jobs.cancel("t");
        assert!(first.token().is_cancelled());
        assert!(!other.token().is_cancelled());
        drop(first);
        assert!(!jobs.is_running("t"));
        let again = jobs.begin("t").expect("again");
        jobs.cancel_all();
        assert!(again.token().is_cancelled() && other.token().is_cancelled());
    }
}
