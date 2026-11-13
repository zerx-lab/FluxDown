//! 带 blob 的建任务提交不随 RPC 等待方结束而中止；blob ID 是一次性的幂等键。

use crate::{
    actor::{ActorOperation, ActorResult, DaemonActorHandle},
    blob_store::{BLOB_TTL, BlobKind, BlobStore},
};
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};
use futures_util::FutureExt as _;
use serde_json::{Value, json};
use std::{collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Instant};
use tokio::sync::OwnedMutexGuard;

#[derive(Default)]
pub(crate) struct History(HashMap<String, (Instant, Value)>);

impl History {
    pub(crate) fn get(&self, id: &str) -> Option<Value> {
        self.0
            .get(id)
            .filter(|(at, _)| at.elapsed() < BLOB_TTL)
            .map(|(_, value)| value.clone())
    }

    fn record(&mut self, id: String, value: Value) {
        self.0.retain(|_, (at, _)| at.elapsed() < BLOB_TTL);
        // 被挤出缓存的 ID 不能再建任务：它们的 blob 已被消费（或已过期）。
        if self.0.len() >= 1024
            && let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| id.clone())
        {
            self.0.remove(&oldest);
        }
        self.0.insert(id, (Instant::now(), value));
    }
}

fn internal(message: &str) -> RpcErrorObject {
    RpcErrorObject::application(
        message,
        RpcErrorData::new(ApplicationErrorCode::Internal, false),
    )
}

pub(crate) async fn submit(
    actor: DaemonActorHandle,
    blobs: Arc<BlobStore>,
    blob_id: Option<String>,
    operation: ActorOperation,
    mut history: Option<OwnedMutexGuard<History>>,
) -> Result<Value, RpcErrorObject> {
    let detached = history.is_some();
    let work = async move {
        match actor
            .execute(operation)
            .await
            .map_err(crate::service::actor_error)?
        {
            ActorResult::Created(task_id) => {
                let result = json!({"taskId": task_id});
                if let Some(id) = blob_id {
                    if let Some(history) = &mut history {
                        history.record(id.clone(), result.clone());
                    }
                    if let Err(error) = blobs.consume(&id, BlobKind::Torrent).await {
                        tracing::warn!(error = %format!("{error:#}"), "created torrent task; upload cleanup failed");
                    }
                }
                Ok(result)
            }
            _ => Err(internal("unexpected actor result")),
        }
    };
    if !detached {
        return work.await;
    }
    // 持有 owned 锁：网络等待方断开后，丢弃 / 重试仍与这次提交串行。
    tokio::spawn(async move {
        match AssertUnwindSafe(work).catch_unwind().await {
            Ok(result) => result,
            Err(_) => {
                tracing::error!("torrent submission worker panicked");
                Err(internal("torrent submission worker panicked"))
            }
        }
    })
    .await
    .map_err(|error| {
        tracing::error!(error = %error, "torrent submission worker stopped");
        internal("torrent submission worker stopped")
    })?
}
