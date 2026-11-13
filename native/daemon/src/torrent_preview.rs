//! 解析已上传的种子：不进任务 actor，也不启动 BT 会话。

use fluxdown_protocol::torrent_preview::TorrentPreviewDto;
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};

use crate::blob_store::{BlobError, BlobKind, BlobStore};

/// 单次预览最多接受的文件条数。元数据上限 32 MiB 已挡住巨型种子，但一份
/// 几十万条的文件清单会把预览 JSON 推近 RPC 单帧上限（32 MiB），拖垮 agent 通道。
#[cfg(feature = "bt")]
const MAX_PREVIEW_FILES: usize = 200_000;

pub(crate) async fn preview(
    blobs: &BlobStore,
    blob_id: String,
) -> Result<TorrentPreviewDto, RpcErrorObject> {
    preview_with_bytes(blobs, blob_id)
        .await
        .map(|(preview, _)| preview)
}

/// 预览并交回已读入的种子字节：确认建任务沿用这一份，不在提交锁内再读一遍磁盘。
pub(crate) async fn preview_with_bytes(
    blobs: &BlobStore,
    blob_id: String,
) -> Result<(TorrentPreviewDto, Vec<u8>), RpcErrorObject> {
    #[cfg(feature = "bt")]
    {
        let bytes = blobs
            .read(&blob_id, BlobKind::Torrent)
            .await
            .map_err(blob_error)?;
        let (result, bytes) = tokio::task::spawn_blocking(move || {
            let result =
                fluxdown_engine::bt_downloader::probe_torrent_meta(String::new(), bytes.clone());
            (result, bytes)
        })
        .await
        .map_err(|error| {
            RpcErrorObject::application(
                format!("torrent preview failed: {error:#}"),
                RpcErrorData::new(ApplicationErrorCode::Internal, false),
            )
        })?;
        if !result.error.is_empty() || result.files.is_empty() {
            return Err(RpcErrorObject::application(
                if result.error.is_empty() {
                    "torrent has no downloadable files".to_owned()
                } else {
                    result.error
                },
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false),
            ));
        }
        if result.files.len() > MAX_PREVIEW_FILES {
            return Err(RpcErrorObject::application(
                format!(
                    "torrent declares {} files; at most {MAX_PREVIEW_FILES} can be confirmed",
                    result.files.len()
                ),
                RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false),
            ));
        }
        let preview = TorrentPreviewDto {
            torrent_blob_id: blob_id,
            name: result.name,
            total_bytes: result.total_bytes,
            files: result
                .files
                .into_iter()
                .map(|file| fluxdown_protocol::BtFileDto {
                    index: file.index,
                    path: file.path,
                    size: file.size,
                })
                .collect(),
        };
        Ok((preview, bytes))
    }
    #[cfg(not(feature = "bt"))]
    {
        let _ = (blobs, blob_id);
        Err(RpcErrorObject::application(
            "daemon was built without BitTorrent support",
            RpcErrorData::new(ApplicationErrorCode::Unsupported, false),
        ))
    }
}

pub(crate) fn validate_confirmation(
    params: &fluxdown_protocol::torrent_preview::TorrentConfirmParams,
    preview: &TorrentPreviewDto,
) -> Result<(), RpcErrorObject> {
    let invalid = |message: String| {
        RpcErrorObject::application(
            message,
            RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false),
        )
    };
    if params
        .options
        .as_ref()
        .is_some_and(|options| options.torrent_b64.is_some() || options.audio_url.is_some())
    {
        return Err(invalid(
            "confirmation options cannot replace torrent content".to_owned(),
        ));
    }
    if params.save_dir.trim().is_empty()
        || params.save_dir.contains('\0')
        || !std::path::Path::new(&params.save_dir).is_absolute()
    {
        return Err(invalid("an absolute save directory is required".to_owned()));
    }
    // 改名是用户手输的单路径组件：可清洗性由 protocol 与引擎共用判据，这里拒绝
    // 而不是悄悄替换（`sanitize_filename` 会把非法字符改成 `_`、把超长名截断）。
    let custom_name = params.custom_name.trim();
    if !custom_name.is_empty()
        && let Err(issue) = fluxdown_protocol::torrent_preview::validate_custom_name(custom_name)
    {
        return Err(invalid(issue.message()));
    }
    if params.selected_file_indices.is_empty()
        || params.selected_file_indices.len() > preview.files.len()
    {
        return Err(invalid(
            "select at least one downloadable torrent file".to_owned(),
        ));
    }
    let available: std::collections::HashSet<_> =
        preview.files.iter().map(|file| file.index).collect();
    let mut selected = std::collections::HashSet::new();
    for index in &params.selected_file_indices {
        if !available.contains(index) || !selected.insert(*index) {
            return Err(invalid(
                "torrent selection contains an invalid or duplicate file index".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn discard(blobs: &BlobStore, blob_id: &str) -> Result<(), RpcErrorObject> {
    match blobs.consume(blob_id, BlobKind::Torrent).await {
        Ok(()) | Err(BlobError::NotFound | BlobError::Expired) => Ok(()),
        Err(error) => Err(blob_error(error)),
    }
}

fn blob_error(error: BlobError) -> RpcErrorObject {
    let code = match &error {
        BlobError::NotFound | BlobError::Expired => ApplicationErrorCode::NotFound,
        BlobError::WrongKind => ApplicationErrorCode::InvalidArgument,
        BlobError::Io(_) => ApplicationErrorCode::Internal,
    };
    RpcErrorObject::application(
        format!("torrent preview blob: {error:#}"),
        RpcErrorData::new(code, false),
    )
}

#[cfg(all(test, feature = "bt"))]
mod tests {
    use super::{discard, preview};
    use crate::blob_store::{BlobKind, BlobStore};

    const SINGLE: &[u8] = b"d4:infod6:lengthi4e4:name8:test.bin12:piece lengthi16384e6:pieces20:00000000000000000000ee";
    const MULTI: &[u8] = b"d4:infod5:filesld6:lengthi4e4:pathl5:a.txteed4:attr1:p6:lengthi1e4:pathl4:.padeed6:lengthi4e4:pathl5:b.txteee4:name6:bundle12:piece lengthi16384e6:pieces20:00000000000000000000ee";

    async fn store() -> Result<(std::path::PathBuf, BlobStore), crate::blob_store::BlobError> {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-torrent-preview-{}", uuid::Uuid::new_v4()));
        let store = BlobStore::open(dir.clone()).await?;
        Ok((dir, store))
    }

    #[tokio::test]
    async fn torrent_confirmation_validates_and_serializes_submissions() {
        use crate::actor::{ActorOperation, ActorResult, DaemonActorHandle, DaemonCommand};
        use fluxdown_protocol::method;
        use serde_json::json;
        let (dir, store) = store().await.expect("isolated store");
        let blobs = std::sync::Arc::new(store);
        let id = blobs.put(BlobKind::Torrent, MULTI).await.expect("upload");
        let db = fluxdown_engine::db::Db::connect("sqlite::memory:")
            .await
            .expect("memory database");
        let events = crate::event_hub::DaemonEventHub::new(Default::default(), 16);
        let (actor, mut commands) = DaemonActorHandle::test_channel();
        let service = crate::service::DaemonService::new(
            crate::service_hello("confirmation-test", Vec::new()),
            events.clone(),
            crate::selection::DaemonSelection::new(events),
            blobs.clone(),
            actor,
            db,
            dir.clone(),
            #[cfg(feature = "plugins")]
            None,
        );
        let confirm = |indices: Vec<i32>, directory: String| {
            json!({
                "torrentBlobId": id, "saveDir": directory,
                "selectedFileIndices": indices, "startPaused": true, "queueId": "chosen-queue",
            })
        };
        let save_dir = dir.join("downloads").to_string_lossy().into_owned();
        for invalid in [
            confirm(vec![], save_dir.clone()),
            confirm(vec![1], save_dir.clone()), // Padding index is not downloadable.
            confirm(vec![-1], save_dir.clone()),
            confirm(vec![2, 2], save_dir.clone()),
            confirm(vec![100], save_dir.clone()),
            confirm(vec![2], String::new()),
            confirm(vec![2], "relative".to_owned()),
        ] {
            assert!(
                service
                    .call_inner("test", true, method::DAEMON_TORRENT_CONFIRM, Some(invalid))
                    .await
                    .is_err()
            );
            assert!(
                commands.try_recv().is_err(),
                "invalid confirmation must not reach actor"
            );
            assert!(
                blobs.read(&id, BlobKind::Torrent).await.is_ok(),
                "validation failure retains preview"
            );
        }
        let valid = confirm(vec![2], save_dir.clone());
        let submit = || {
            service.call_inner(
                "test",
                true,
                method::DAEMON_TORRENT_CONFIRM,
                Some(valid.clone()),
            )
        };
        let reject_once = async {
            let DaemonCommand::Execute { ack, .. } = commands.recv().await.expect("first attempt")
            else {
                panic!("expected creation");
            };
            assert!(
                ack.send(Err(crate::actor::ActorError::Operation(
                    "persistence failed".into()
                )))
                .is_ok()
            );
        };
        let (failed, ()) = tokio::join!(submit(), reject_once);
        assert!(failed.is_err());
        assert!(
            blobs.read(&id, BlobKind::Torrent).await.is_ok(),
            "failed creation permits retry"
        );
        let actor_response = async {
            let DaemonCommand::Execute { operation, ack } =
                commands.recv().await.expect("create command")
            else {
                panic!("expected creation");
            };
            let ActorOperation::CreateTask {
                request,
                selected_file_indices,
                torrent_file_bytes,
                unattended,
                ..
            } = operation
            else {
                panic!("expected create task");
            };
            assert_eq!(selected_file_indices, vec![2]);
            assert_eq!(torrent_file_bytes, MULTI);
            assert_eq!(request.save_dir, save_dir);
            assert_eq!(request.queue_id, "chosen-queue");
            assert!(request.start_paused);
            assert!(!unattended);
            let discard_params = Some(json!({"torrentBlobId": "torrent:unrelated"}));
            let mut waiting_discard = Box::pin(service.call_inner(
                "test",
                true,
                method::DAEMON_TORRENT_DISCARD_PREVIEW,
                discard_params,
            ));
            assert!(futures_util::poll!(&mut waiting_discard).is_pending());
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                service.call_inner("test", true, method::DAEMON_TASK_LIST, None),
            )
            .await
            .expect("fast query remains available")
            .expect("task list");
            assert!(
                ack.send(Ok(ActorResult::Created("confirmed-task".into())))
                    .is_ok()
            );
            waiting_discard
                .await
                .expect("discard completes after confirmation");
        };
        let (first, second, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(submit(), submit(), actor_response)
        })
        .await
        .expect("concurrent confirmations finish");
        assert_eq!(
            first.expect("first confirmation"),
            second.expect("idempotent confirmation")
        );
        assert!(
            commands.try_recv().is_err(),
            "double submit must create only once"
        );
        assert!(blobs.read(&id, BlobKind::Torrent).await.is_err());
        service
            .call_inner(
                "test",
                true,
                method::DAEMON_TORRENT_DISCARD_PREVIEW,
                Some(json!({"torrentBlobId": id})),
            )
            .await
            .expect("late cleanup is idempotent");
        let abandoned_id = blobs
            .put(BlobKind::Torrent, MULTI)
            .await
            .expect("upload interrupted confirmation");
        let abandoned_params = json!({"torrentBlobId": abandoned_id, "saveDir": save_dir, "selectedFileIndices": [2],
            "queueId": "chosen-queue", "customName": "renamed", "options": {
                "url": "ignored", "proxyUrl": "socks5://localhost:1080", "userAgent": "draft-agent",
                "queueId": "ignored", "startPaused": false, "fileName": "ignored"
            }, "startPaused": true});
        let mut abandoned = Box::pin(service.call_inner(
            "test",
            true,
            method::DAEMON_TORRENT_CONFIRM,
            Some(abandoned_params.clone()),
        ));
        let command = tokio::select! {
            command = commands.recv() => command.expect("actor received interrupted request"),
            result = &mut abandoned => panic!("request completed before actor: {result:?}"),
        };
        drop(abandoned);
        let DaemonCommand::Execute {
            operation:
                ActorOperation::CreateTask {
                    request,
                    bt_custom_name,
                    ..
                },
            ack,
        } = command
        else {
            panic!("create expected");
        };
        assert_eq!(request.proxy_url, "socks5://localhost:1080");
        assert_eq!(request.user_agent, "draft-agent");
        assert_eq!(request.queue_id, "chosen-queue");
        assert_eq!(request.file_name, "renamed");
        assert_eq!(bt_custom_name, "renamed");
        assert!(request.start_paused);
        assert!(
            ack.send(Ok(ActorResult::Created("survived-disconnect".into())))
                .is_ok()
        );
        let replay = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.call_inner(
                "test",
                true,
                method::DAEMON_TORRENT_CONFIRM,
                Some(abandoned_params),
            ),
        )
        .await
        .expect("replay completes")
        .expect("replay result");
        assert_eq!(replay["taskId"], "survived-disconnect");
        assert!(commands.try_recv().is_err(), "replay must not reach actor");
        assert!(blobs.read(&abandoned_id, BlobKind::Torrent).await.is_err());
        let cancelled = blobs
            .put(BlobKind::Torrent, MULTI)
            .await
            .expect("upload another");
        service
            .call_inner(
                "test",
                true,
                method::DAEMON_TORRENT_DISCARD_PREVIEW,
                Some(json!({"torrentBlobId": cancelled})),
            )
            .await
            .expect("cancel");
        assert!(service.call_inner("test", true, method::DAEMON_TORRENT_CONFIRM,
            Some(json!({"torrentBlobId": cancelled, "saveDir": save_dir, "selectedFileIndices": [2]}))).await.is_err());
        assert!(
            commands.try_recv().is_err(),
            "cancelled preview must not create a task"
        );
        drop(service);
        tokio::fs::remove_dir(dir)
            .await
            .expect("remove empty store");
    }

    /// 校验器与引擎清洗必须同口径：校验通过的名字，引擎必须原样落盘——否则用户
    /// 在确认框里看到的名字与真实文件夹 / 文件名不一致，而且没有任何提示。
    #[test]
    fn accepted_rename_targets_are_left_untouched_by_engine_sanitizing() {
        use fluxdown_engine::naming::sanitize_filename;
        let cjk = "字".repeat(66);
        for name in [
            "改名后的名字",
            "Movie.2026.1080p.mkv",
            "第 01 集 (1).mp4",
            "a.b.c",
            cjk.as_str(),
        ] {
            assert!(
                fluxdown_protocol::torrent_preview::validate_custom_name(name).is_ok(),
                "{name:?} should be accepted"
            );
            assert_eq!(
                sanitize_filename(name),
                name,
                "validator accepted {name:?} but the engine would rewrite it"
            );
        }
        // 下面这些必须在校验层被拒：引擎会删字符 / 改字符 / 去首尾点或空格 /
        // 给保留名补前缀 / 按字节截断。
        let too_long = "x".repeat(201);
        for name in [
            "\u{200B}",
            "a\u{200B}b",
            ".hidden",
            "name.",
            "a:b",
            "CON",
            too_long.as_str(),
        ] {
            assert!(
                fluxdown_protocol::torrent_preview::validate_custom_name(name).is_err(),
                "{name:?} must be rejected"
            );
        }
    }

    #[test]
    fn confirmation_rejects_unportable_rename_targets() {
        use fluxdown_protocol::torrent_preview::{TorrentConfirmParams, TorrentPreviewDto};
        let preview = TorrentPreviewDto {
            torrent_blob_id: "torrent:preview".into(),
            name: "资料".into(),
            total_bytes: 4,
            files: vec![fluxdown_protocol::BtFileDto {
                index: 0,
                path: "a.txt".into(),
                size: 4,
            }],
        };
        let params = |custom: &str| TorrentConfirmParams {
            torrent_blob_id: "torrent:preview".into(),
            save_dir: if cfg!(windows) {
                r"C:\Downloads".into()
            } else {
                "/tmp".into()
            },
            selected_file_indices: vec![0],
            queue_id: String::new(),
            start_paused: false,
            custom_name: custom.into(),
            source_url: String::new(),
            options: None,
        };
        for accepted in ["", "改名后的名字", "第 01 集.mkv"] {
            assert!(
                super::validate_confirmation(&params(accepted), &preview).is_ok(),
                "{accepted:?}"
            );
        }
        for rejected in [
            "a/b",
            "a\\b",
            "名字.",
            "CON",
            "../escape",
            "x".repeat(201).as_str(),
        ] {
            let error = super::validate_confirmation(&params(rejected), &preview)
                .expect_err("rename target must be rejected");
            assert_eq!(
                error.data.expect("error data").code,
                fluxdown_protocol::ApplicationErrorCode::InvalidArgument,
                "{rejected:?}"
            );
        }
    }

    #[tokio::test]
    async fn preview_rpc_never_creates_tasks_or_publishes_download_events() {
        let (dir, store) = store().await.expect("open isolated blob store");
        let blobs = std::sync::Arc::new(store);
        let id = blobs.put(BlobKind::Torrent, SINGLE).await.expect("upload");
        let db = fluxdown_engine::db::Db::connect("sqlite::memory:")
            .await
            .expect("open isolated in-memory database");
        let events = crate::event_hub::DaemonEventHub::new(Default::default(), 16);
        let before = events.snapshot().sequence;
        let service = crate::service::DaemonService::new(
            crate::service_hello("torrent-preview-test", Vec::new()),
            events.clone(),
            crate::selection::DaemonSelection::new(events.clone()),
            blobs.clone(),
            // 断开的 actor：误建任务会直接失败，到不了真正的下载器。
            crate::actor::DaemonActorHandle::disconnected(),
            db.clone(),
            dir.clone(),
            #[cfg(feature = "plugins")]
            None,
        );
        let params = serde_json::json!({ "torrentBlobId": id });
        let result = service
            .call_inner(
                "preview-test",
                true,
                fluxdown_protocol::method::DAEMON_TORRENT_PREVIEW,
                Some(params.clone()),
            )
            .await
            .expect("preview through RPC dispatcher");
        assert_eq!(result["name"], "test.bin");
        assert_eq!(result["files"][0]["index"], 0);
        assert!(db.load_all_tasks().await.expect("read tasks").is_empty());
        assert_eq!(events.snapshot().sequence, before);
        service
            .call_inner(
                "preview-test",
                true,
                fluxdown_protocol::method::DAEMON_TORRENT_DISCARD_PREVIEW,
                Some(params),
            )
            .await
            .expect("cancel through RPC dispatcher");
        assert!(blobs.read(&id, BlobKind::Torrent).await.is_err());
        assert!(
            db.load_all_tasks()
                .await
                .expect("read tasks after cancellation")
                .is_empty()
        );
        assert_eq!(events.snapshot().sequence, before);
        drop(service);
        tokio::fs::remove_dir(dir)
            .await
            .expect("remove empty blob store");
    }

    #[tokio::test]
    async fn preview_keeps_upload_until_explicit_idempotent_discard() {
        // 有意不提供任务 actor、数据库、BT 会话和下载目录。
        let (dir, store) = store().await.expect("open isolated blob store");
        let id = store.put(BlobKind::Torrent, SINGLE).await.expect("upload");
        let result = preview(&store, id.clone()).await.expect("preview");
        assert_eq!(result.torrent_blob_id, id);
        assert_eq!(result.name, "test.bin");
        assert_eq!(result.total_bytes, 4);
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].index, 0);
        assert_eq!(result.files[0].path, "test.bin");
        assert_eq!(
            store.read(&id, BlobKind::Torrent).await.expect("retained"),
            SINGLE
        );
        let again = preview(&store, id.clone()).await.expect("repeat preview");
        assert_eq!(again.name, result.name);
        discard(&store, &id).await.expect("cancel");
        discard(&store, &id).await.expect("repeat cancel");
        assert!(preview(&store, id).await.is_err());
        assert!(
            tokio::fs::read_dir(&dir)
                .await
                .expect("read store")
                .next_entry()
                .await
                .expect("entry")
                .is_none()
        );
        tokio::fs::remove_dir(dir)
            .await
            .expect("remove empty store");
    }

    #[tokio::test]
    async fn preview_filters_padding_without_renumbering_files() {
        let (dir, store) = store().await.expect("open isolated blob store");
        let id = store.put(BlobKind::Torrent, MULTI).await.expect("upload");
        let result = preview(&store, id.clone()).await.expect("preview");
        assert_eq!(result.name, "bundle");
        assert_eq!(result.total_bytes, 8);
        assert_eq!(
            result.files.iter().map(|f| f.index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(
            result
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.txt", "b.txt"]
        );
        discard(&store, &id).await.expect("cancel");
        tokio::fs::remove_dir(dir)
            .await
            .expect("remove empty store");
    }

    #[tokio::test]
    async fn malformed_and_wrong_kind_uploads_are_rejected() {
        let (dir, store) = store().await.expect("open isolated blob store");
        let malformed = store
            .put(BlobKind::Torrent, b"not a torrent")
            .await
            .expect("upload");
        let plugin = store
            .put(BlobKind::Plugin, SINGLE)
            .await
            .expect("plugin upload");
        for id in [&malformed, &plugin] {
            let error = preview(&store, id.clone())
                .await
                .expect_err("reject invalid upload");
            assert_eq!(
                error.data.expect("error data").code,
                fluxdown_protocol::ApplicationErrorCode::InvalidArgument
            );
        }
        assert!(discard(&store, &plugin).await.is_err());
        assert_eq!(
            store
                .read(&plugin, BlobKind::Plugin)
                .await
                .expect("plugin retained"),
            SINGLE
        );
        discard(&store, &malformed)
            .await
            .expect("discard malformed torrent");
        store
            .consume(&plugin, BlobKind::Plugin)
            .await
            .expect("clean plugin fixture");
        tokio::fs::remove_dir(dir)
            .await
            .expect("remove empty store");
    }
}
