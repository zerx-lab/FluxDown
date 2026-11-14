//! Unified task saves share the actor's maintenance channel while writers stop or files copy.
use super::{ActorError, ActorResult, MaintenanceEvent};
use fluxdown_engine::{
    Engine,
    download_manager::{TaskEditSession, TaskEditStep},
};
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<ActorResult, ActorError>>;

pub struct PendingEdit {
    pub session: TaskEditSession,
    pub deadline: tokio::time::Instant,
    pub ack: Reply,
}

pub(super) fn edit_error(error: String) -> ActorError {
    match error.as_str() {
        "not-found" => ActorError::NotFound,
        "edit-conflict" | "edit-busy" | "target-exists" => ActorError::Conflict(error),
        value
            if value.starts_with("db:")
                || value.starts_with("move")
                || value.starts_with("credentials:") =>
        {
            ActorError::Operation(error)
        }
        _ => ActorError::InvalidArgument(error),
    }
}

pub(super) async fn step(
    result: Result<TaskEditStep, String>,
    ack: Reply,
    deadline: tokio::time::Instant,
    engine: &mut Engine,
    tx: &mpsc::UnboundedSender<MaintenanceEvent>,
) {
    match result {
        Ok(TaskEditStep::Applied) => reply(ack, Ok(ActorResult::Unit)),
        Ok(TaskEditStep::Waiting(session)) => {
            if tokio::time::Instant::now() >= deadline {
                engine.manager.abandon_task_edit(session).await;
                reply(
                    ack,
                    Err(ActorError::TaskStateConflict(
                        "task writer did not stop in time".into(),
                    )),
                );
                return;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if tx
                    .send(MaintenanceEvent::TaskEditWait(Box::new(PendingEdit {
                        session,
                        deadline,
                        ack,
                    })))
                    .is_err()
                {
                    tracing::debug!("daemon closed while a task edit waited for its writer");
                }
            });
        }
        Ok(TaskEditStep::Started(job)) => {
            let tx = tx.clone();
            tokio::spawn(async move {
                let outcome = job.await;
                if tx
                    .send(MaintenanceEvent::TaskEditDone {
                        outcome: Box::new(outcome),
                        ack,
                    })
                    .is_err()
                {
                    tracing::debug!(
                        "daemon closed before task relocation commit; journal retained"
                    );
                }
            });
        }
        Err(error) => reply(ack, Err(edit_error(error))),
    }
}

pub(super) fn reply(ack: Reply, result: Result<ActorResult, ActorError>) {
    if let Err(result) = ack.send(result) {
        if let Err(error) = result {
            tracing::warn!(%error, "task edit failed after caller disconnected");
        } else {
            tracing::debug!("task edit caller disconnected before acknowledgement");
        }
    }
}
