//! Single-selected-file BT classification, followed by the normal journaled storage move.
//! Recognition and copying never run in the owner actor. The persisted routing authorization
//! is consumed at first completion so reseeding or a later selection cannot move it again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::FutureExt;
use tokio::sync::{Mutex, mpsc};

use super::{BtMoveOutcome, BtMoveStart, DownloadManager, TaskDone, TaskDonePhase};
use crate::category_route::CategoryRoute;
use crate::model::TaskInfo;

pub(super) enum WorkResult {
    Pending,
    Plan {
        from: String,
        target: Result<Option<String>, String>,
    },
    Moved(BtMoveOutcome),
}

type Results = Arc<Mutex<HashMap<String, WorkResult>>>;

impl DownloadManager {
    pub(super) async fn schedule_bt_archive(&self, task: &TaskInfo) -> anyhow::Result<()> {
        let column = self.db.load_task_category_route(&task.task_id).await?;
        let Some(route) = CategoryRoute::from_column(&task.task_id, &column)
            .filter(|route| !route.base_dir.is_empty())
        else {
            return Ok(());
        };
        let mut consumed = route.clone();
        consumed.base_dir.clear();
        self.db
            .set_task_category_route(&task.task_id, &consumed.to_column()?)
            .await?;
        let Some(bt) = self.bt_session.as_ref() else {
            return Ok(());
        };
        let Some(live) = bt.live_task_files(&task.task_id).await else {
            return Ok(());
        };
        let mut selected = live.files.iter().enumerate().filter(|(index, file)| {
            !file.padding && live.levels.get(*index).is_some_and(|level| *level > 0)
        });
        let Some((index, file)) = selected.next() else {
            return Ok(());
        };
        if selected.next().is_some() {
            return Ok(());
        }
        let size = file.size;
        let task = task.clone();
        let sidecar = crate::bt_partfile::sidecar_path(&self.app_data_dir, &task.task_id);
        let interceptor = self
            .task_interceptor(&task.task_id, &task.url, &task.url)
            .await;
        if !interceptor.has_archivers() {
            return Ok(());
        }
        let cancel = bt.closing_token();
        let results = self.bt_archive_results.clone();
        let sender = self.done_tx.clone();
        results
            .lock()
            .await
            .insert(task.task_id.clone(), WorkResult::Pending);
        tokio::spawn(async move {
            let work = async {
                let root = PathBuf::from(&task.save_dir);
                let top = task.file_name.clone();
                let path = tokio::task::spawn_blocking(move || {
                    selected_path(&sidecar, &root, &top, index, size)
                })
                .await
                .map_err(|error| format!("archive file lookup failed: {error:#}"))??;
                crate::category_route::bt_archive_target(
                    &task,
                    &route,
                    path,
                    interceptor.as_ref(),
                    cancel,
                )
                .await
            };
            let target = std::panic::AssertUnwindSafe(work)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("BT archive classification panicked".to_owned()));
            publish(
                results,
                sender,
                task.task_id,
                WorkResult::Plan {
                    from: task.save_dir,
                    target,
                },
            )
            .await;
        });
        Ok(())
    }

    pub(super) async fn commit_bt_archive_work(&mut self, task_id: &str) {
        let result = self.bt_archive_results.lock().await.remove(task_id);
        match result {
            Some(WorkResult::Plan {
                from,
                target: Ok(Some(target)),
            }) => {
                match self.db.load_task_by_id(task_id).await {
                    Ok(Some(task)) if task.status == 3 && task.save_dir == from => {}
                    Ok(_) => return,
                    Err(error) => {
                        crate::logger::report_warning("BT", "reload archive task", &error);
                        return;
                    }
                }
                match self.begin_bt_move(task_id, &target, false).await {
                    Ok(BtMoveStart::Started(job)) => {
                        let results = self.bt_archive_results.clone();
                        let sender = self.done_tx.clone();
                        let id = task_id.to_owned();
                        results.lock().await.insert(id.clone(), WorkResult::Pending);
                        tokio::spawn(async move {
                            let result = match std::panic::AssertUnwindSafe(job)
                                .catch_unwind()
                                .await
                            {
                                Ok(outcome) => WorkResult::Moved(outcome),
                                Err(_) => WorkResult::Plan {
                                    from: String::new(),
                                    target: Err(
                                        "BT archive move panicked; journal retained for recovery"
                                            .to_owned(),
                                    ),
                                },
                            };
                            publish(results, sender, id, result).await;
                        });
                        return;
                    }
                    Ok(BtMoveStart::Unchanged) => {}
                    Ok(BtMoveStart::Wait { .. }) => tracing::warn!(
                        task_id,
                        "BT archive skipped: task changed while classification was running"
                    ),
                    Err(error) => {
                        crate::logger::report_warning("BT", "start archive storage move", &error)
                    }
                }
            }
            Some(WorkResult::Moved(outcome)) => {
                if let Err(error) = self.finish_bt_move(outcome).await {
                    crate::logger::report_warning("BT", "finish archive storage move", &error);
                }
            }
            Some(WorkResult::Plan {
                target: Err(error), ..
            }) => tracing::warn!(task_id, error, "BT content archive skipped"),
            Some(WorkResult::Plan {
                target: Ok(None), ..
            })
            | Some(WorkResult::Pending)
            | None => {}
        }
        if let Some(algorithm) = self.hash_after_download {
            self.spawn_auto_file_hash(task_id, algorithm).await;
        }
    }
}

fn selected_path(
    sidecar: &Path,
    root: &Path,
    top: &str,
    index: usize,
    size: u64,
) -> Result<PathBuf, String> {
    let path = match crate::bt_partfile::load_sidecar_layout(sidecar, root)? {
        Some(layout) => layout
            .selected_path(index)
            .map(Path::to_path_buf)
            .ok_or_else(|| "selected BT file is absent from the final storage map".to_owned())?,
        None => root.join(top),
    };
    let canonical = std::fs::canonicalize(&path)
        .map_err(|error| format!("resolve BT archive file: {error:#}"))?;
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("resolve BT archive root: {error:#}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("stat BT archive file: {error:#}"))?;
    if !canonical.starts_with(root) || !metadata.is_file() || metadata.len() != size {
        return Err("BT archive requires one complete file inside the task root".to_owned());
    }
    Ok(canonical)
}

async fn publish(results: Results, sender: mpsc::Sender<TaskDone>, id: String, result: WorkResult) {
    results.lock().await.insert(id.clone(), result);
    if !super::send_task_done(
        &sender,
        TaskDone {
            task_id: id.clone(),
            generation: 0,
            reserved_temp_path: None,
            phase: TaskDonePhase::BtArchiveWorkReady,
        },
    )
    .await
    {
        results.lock().await.remove(&id);
    }
}
