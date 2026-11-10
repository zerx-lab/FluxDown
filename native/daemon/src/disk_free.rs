//! 运行时统计里「剩余空间」（`DaemonRuntimeStatsDto::disk_free_bytes`）的事件驱动刷新。
//!
//! 读数只在启动时量一次会永远停在启动那一刻：下载写盘、删除文件、改保存目录后各端
//! （GPUI 状态栏、Web、移动端「设备」页）都看不到变化。这里订阅 daemon 事件广播，
//! 任务 / 配置事件到达后合并成一次延后探测：
//!
//! - 只由任务进度、任务事实、删除与配置变化触发——空闲（无事件）时不碰 save_dir 所在卷，
//!   守住 NAS 硬盘休眠的空闲静默不变式；
//! - 两次探测至少间隔 [`MIN_INTERVAL`]，下载期间逐帧进度不会变成逐帧 `statvfs`；
//! - 事件后先等 [`SETTLE`]，让删除 / 完成收尾的文件系统操作先落地再读数。

use std::path::PathBuf;
use std::time::Duration;

use fluxdown_protocol::{DaemonEvent, EventFrame, ServiceEvent};
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::event_hub::DaemonEventHub;

/// 相邻两次探测的最小间隔。
const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// 触发事件到探测之间的沉淀时间。
const SETTLE: Duration = Duration::from_secs(1);

/// 统计口径的保存目录：`default_save_dir` 非空取之，否则平台下载目录。
pub(crate) fn stats_save_dir(configured: Option<&str>) -> String {
    configured
        .filter(|path| !path.trim().is_empty())
        .map_or_else(
            fluxdown_engine::user_dirs::download_dir_or_cwd,
            str::to_owned,
        )
}

/// 量一次当前保存目录所在卷的剩余空间并写回投影（数值不变不发布）。
async fn refresh(events: &DaemonEventHub) {
    let configured =
        events.inspect(|snapshot| snapshot.config.values.get("default_save_dir").cloned());
    let save_dir = stats_save_dir(configured.as_deref());
    let free = fluxdown_engine::disk_space::available_space_checked(PathBuf::from(&save_dir)).await;
    events.set_disk_free(configured.as_deref(), save_dir, free);
}

/// 后台刷新任务；`cancel` 触发或事件通道关闭后退出。
pub fn spawn_watcher(events: DaemonEventHub, cancel: CancellationToken) {
    let mut receiver = events.subscribe();
    tokio::spawn(async move {
        let mut last_probe: Option<Instant> = None;
        let mut due: Option<Instant> = None;
        loop {
            let wake = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = cancel.cancelled() => break,
                () = wake => {
                    due = None;
                    refresh(&events).await;
                    last_probe = Some(Instant::now());
                }
                frame = receiver.recv() => {
                    let wanted = match frame {
                        Ok(frame) => affects_disk_usage(&frame),
                        // 滞后丢帧：不知道漏了什么，保守地量一次。
                        Err(broadcast::error::RecvError::Lagged(_)) => true,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    if wanted && due.is_none() {
                        due = Some(next_due(last_probe, Instant::now()));
                    }
                }
            }
        }
    });
}

fn next_due(last_probe: Option<Instant>, now: Instant) -> Instant {
    let settled = now + SETTLE;
    last_probe.map_or(settled, |last| settled.max(last + MIN_INTERVAL))
}

/// 可能改变保存卷占用或保存目录本身的事件。不含 `RuntimeStatsChanged`：
/// 本模块自己就发布它，算进来会自激。
fn affects_disk_usage(frame: &EventFrame) -> bool {
    let ServiceEvent::Daemon(event) = &frame.event else {
        return false;
    };
    matches!(
        event,
        DaemonEvent::Engine(_)
            | DaemonEvent::TaskRuntimeChanged(_)
            | DaemonEvent::TaskChanged(_)
            | DaemonEvent::TasksChanged(_)
            | DaemonEvent::TaskDeleted { .. }
            | DaemonEvent::TasksDeleted { .. }
            | DaemonEvent::SnapshotReplaced(_)
            | DaemonEvent::TablesReloading { .. }
            | DaemonEvent::ConfigChanged(_)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fluxdown_protocol::{DaemonConfigSnapshot, DaemonEvent, DaemonSnapshot, ServiceEvent};

    use super::{MIN_INTERVAL, SETTLE, next_due, spawn_watcher};
    use crate::event_hub::DaemonEventHub;

    fn hub_with_save_dir(dir: &str) -> DaemonEventHub {
        DaemonEventHub::new(
            DaemonSnapshot {
                config: DaemonConfigSnapshot {
                    revision: 1,
                    values: BTreeMap::from([("default_save_dir".to_owned(), dir.to_owned())]),
                },
                ..DaemonSnapshot::default()
            },
            64,
        )
    }

    fn disk_free(hub: &DaemonEventHub) -> Option<u64> {
        hub.inspect(|snapshot| snapshot.runtime_stats.disk_free_bytes)
    }

    #[test]
    fn probes_wait_for_settle_and_respect_min_interval() {
        let now = tokio::time::Instant::now();
        assert_eq!(next_due(None, now), now + SETTLE);
        // 刚量过：推迟到间隔结束。
        assert_eq!(next_due(Some(now), now), now + MIN_INTERVAL);
        // 很久前量过：只等沉淀时间。
        let long_ago = now.checked_sub(MIN_INTERVAL * 10).unwrap_or(now);
        assert_eq!(next_due(Some(long_ago), now), now + SETTLE);
    }

    #[test]
    fn stale_probe_for_replaced_save_dir_is_dropped() {
        let dir = std::env::temp_dir().display().to_string();
        let hub = hub_with_save_dir(&dir);
        hub.set_disk_free(
            Some("/somewhere/else"),
            "/somewhere/else".to_owned(),
            Some(1),
        );
        assert_eq!(disk_free(&hub), None);
        hub.set_disk_free(Some(&dir), dir.clone(), Some(42));
        assert_eq!(disk_free(&hub), Some(42));
    }

    #[test]
    fn unchanged_reading_publishes_nothing() {
        let dir = std::env::temp_dir().display().to_string();
        let hub = hub_with_save_dir(&dir);
        hub.set_disk_free(Some(&dir), dir.clone(), Some(42));
        let mut receiver = hub.subscribe();
        hub.set_disk_free(Some(&dir), dir.clone(), Some(42));
        assert!(receiver.try_recv().is_err());
        hub.set_disk_free(Some(&dir), dir.clone(), Some(41));
        let frame = receiver.try_recv().unwrap();
        assert!(matches!(
            frame.event,
            ServiceEvent::Daemon(DaemonEvent::RuntimeStatsChanged(stats))
                if stats.disk_free_bytes == Some(41)
        ));
    }

    #[tokio::test]
    async fn task_event_refreshes_stale_reading() {
        let dir = std::env::temp_dir().display().to_string();
        let hub = hub_with_save_dir(&dir);
        // 启动时的旧读数：不可能等于真实剩余空间。
        hub.set_disk_free(Some(&dir), dir.clone(), Some(1));
        let cancel = tokio_util::sync::CancellationToken::new();
        spawn_watcher(hub.clone(), cancel.clone());
        // 删除任务（连同文件）：典型的「剩余空间变了」事件。
        hub.publish(DaemonEvent::TaskDeleted {
            task_id: "t".to_owned(),
        });
        let refreshed = tokio::time::timeout(SETTLE * 5, async {
            loop {
                if disk_free(&hub).is_some_and(|free| free != 1) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
        cancel.cancel();
        assert!(
            refreshed.is_ok(),
            "disk free never refreshed after a task event"
        );
    }

    #[tokio::test]
    async fn idle_hub_is_never_probed() {
        let dir = std::env::temp_dir().display().to_string();
        let hub = hub_with_save_dir(&dir);
        hub.set_disk_free(Some(&dir), dir.clone(), Some(1));
        let cancel = tokio_util::sync::CancellationToken::new();
        spawn_watcher(hub.clone(), cancel.clone());
        tokio::time::sleep(SETTLE * 2).await;
        cancel.cancel();
        assert_eq!(disk_free(&hub), Some(1));
    }
}
