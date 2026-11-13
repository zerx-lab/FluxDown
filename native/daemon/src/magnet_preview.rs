//! 磁力元数据预览的预约：数量有上限、一次性使用，取消不必等慢速的网络 RPC。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use fluxdown_protocol::torrent_preview::{MagnetPreviewTicket, TorrentPreviewDto};
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, RpcErrorObject};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::blob_store::{BlobKind, BlobStore};

const LIFETIME: Duration = Duration::from_secs(120);
const MAX_RESERVED: usize = 16;
const MAX_RUNNING: usize = 4;
const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct MagnetPreviews {
    jobs: Mutex<HashMap<String, Job>>,
}

struct Job {
    url: String,
    cancel: CancellationToken,
    deadline: Instant,
    running: bool,
}

struct Claim<'a> {
    owner: &'a MagnetPreviews,
    id: String,
    url: String,
    cancel: CancellationToken,
    deadline: Instant,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.owner.jobs().remove(&self.id);
    }
}

impl MagnetPreviews {
    fn jobs(&self) -> MutexGuard<'_, HashMap<String, Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn prepare(&self, url: String) -> Result<MagnetPreviewTicket, RpcErrorObject> {
        validate_url(&url)?;
        let mut jobs = self.jobs();
        jobs.retain(|_, job| {
            if job.deadline <= Instant::now() {
                job.cancel.cancel();
                false
            } else {
                true
            }
        });
        if jobs.len() >= MAX_RESERVED {
            return Err(error(
                ApplicationErrorCode::Conflict,
                "too many pending magnet previews",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        jobs.insert(
            id.clone(),
            Job {
                url,
                cancel: CancellationToken::new(),
                deadline: Instant::now() + LIFETIME,
                running: false,
            },
        );
        Ok(MagnetPreviewTicket { preview_id: id })
    }

    pub(crate) fn cancel(&self, id: &str) {
        if let Some(job) = self.jobs().remove(id) {
            job.cancel.cancel();
        }
    }

    fn claim(&self, id: &str) -> Result<Claim<'_>, RpcErrorObject> {
        let mut jobs = self.jobs();
        if jobs.values().filter(|job| job.running).count() >= MAX_RUNNING {
            return Err(error(
                ApplicationErrorCode::Conflict,
                "too many running magnet previews",
            ));
        }
        let job = jobs.get_mut(id).ok_or_else(|| {
            error(
                ApplicationErrorCode::NotFound,
                "magnet preview no longer exists",
            )
        })?;
        if job.running {
            return Err(error(
                ApplicationErrorCode::Conflict,
                "magnet preview already resolving",
            ));
        }
        if job.deadline <= Instant::now() {
            job.cancel.cancel();
            jobs.remove(id);
            return Err(error(
                ApplicationErrorCode::Timeout,
                "magnet preview expired",
            ));
        }
        job.running = true;
        Ok(Claim {
            owner: self,
            id: id.to_owned(),
            url: job.url.clone(),
            cancel: job.cancel.clone(),
            deadline: job.deadline,
        })
    }

    pub(crate) async fn resolve(
        &self,
        blobs: &BlobStore,
        id: &str,
        config: fluxdown_engine::bt_config::BtConfig,
        proxy: fluxdown_engine::proxy_config::ProxyConfig,
        #[cfg(feature = "bt")] guard: Option<
            std::sync::Arc<fluxdown_engine::bt_guard::BtGuardHost>,
        >,
    ) -> Result<TorrentPreviewDto, RpcErrorObject> {
        self.resolve_with(blobs, id, |url, cancel| async move {
            #[cfg(feature = "bt")]
            {
                if !config.enabled {
                    return Err(error(
                        ApplicationErrorCode::Unsupported,
                        "BitTorrent is disabled",
                    ));
                }
                fluxdown_engine::bt_preview::resolve_magnet(
                    &url,
                    &config,
                    &proxy,
                    cancel,
                    guard.as_deref(),
                )
                .await
                .map_err(|cause| {
                    error(
                        ApplicationErrorCode::Unavailable,
                        &format!("magnet metadata: {cause:#}"),
                    )
                })
            }
            #[cfg(not(feature = "bt"))]
            {
                let _ = (url, cancel, config, proxy);
                Err(error(
                    ApplicationErrorCode::Unsupported,
                    "BitTorrent support is not compiled in",
                ))
            }
        })
        .await
    }

    async fn resolve_with<F, Fut>(
        &self,
        blobs: &BlobStore,
        id: &str,
        fetch: F,
    ) -> Result<TorrentPreviewDto, RpcErrorObject>
    where
        F: FnOnce(String, CancellationToken) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, RpcErrorObject>>,
    {
        let claim = self.claim(id)?;
        let bytes = tokio::select! {
            biased;
            _ = claim.cancel.cancelled() => return Err(error(ApplicationErrorCode::Cancelled, "magnet preview cancelled")),
            _ = tokio::time::sleep_until(claim.deadline) => return Err(error(ApplicationErrorCode::Timeout, "magnet metadata timed out")),
            result = fetch(claim.url.clone(), claim.cancel.clone()) => result?,
        };
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(error(
                ApplicationErrorCode::InvalidArgument,
                "torrent metadata exceeds 32 MiB",
            ));
        }
        let blob_id = blobs
            .put(BlobKind::Torrent, &bytes)
            .await
            .map_err(|cause| {
                error(
                    ApplicationErrorCode::Internal,
                    &format!("save metadata: {cause:#}"),
                )
            })?;
        let result = crate::torrent_preview::preview(blobs, blob_id.clone()).await;
        let result = if claim.cancel.is_cancelled() {
            Err(error(
                ApplicationErrorCode::Cancelled,
                "magnet preview cancelled",
            ))
        } else if Instant::now() >= claim.deadline {
            Err(error(
                ApplicationErrorCode::Timeout,
                "magnet metadata timed out",
            ))
        } else {
            result
        };
        if result.is_err()
            && let Err(cleanup) = crate::torrent_preview::discard(blobs, &blob_id).await
        {
            // 清理失败不能顶掉真正的失败原因（用户看到「malformed torrent」比
            // 看到一次 unlink 的 IO 错误有用得多）；残留 blob 由 TTL 兜底。
            tracing::warn!(error = %cleanup.message, "magnet preview cleanup failed");
        }
        result
    }
}

impl Drop for MagnetPreviews {
    fn drop(&mut self) {
        for job in self.jobs().values() {
            job.cancel.cancel();
        }
    }
}

fn error(code: ApplicationErrorCode, message: &str) -> RpcErrorObject {
    RpcErrorObject::application(message, RpcErrorData::new(code, false))
}

fn validate_url(url: &str) -> Result<(), RpcErrorObject> {
    #[cfg(feature = "bt")]
    {
        fluxdown_engine::bt_preview::validate_magnet(url).map_err(|cause| {
            error(
                ApplicationErrorCode::InvalidArgument,
                &format!("invalid magnet: {cause:#}"),
            )
        })
    }
    #[cfg(not(feature = "bt"))]
    {
        let _ = url;
        Err(error(
            ApplicationErrorCode::Unsupported,
            "BitTorrent support is not compiled in",
        ))
    }
}

#[cfg(all(test, feature = "bt"))]
mod tests {
    use super::{MAX_RESERVED, MAX_RUNNING, MagnetPreviews};
    use crate::blob_store::BlobStore;
    use fluxdown_protocol::ApplicationErrorCode;
    use std::time::Duration;
    use tokio::time::Instant;

    const URL: &str = "magnet:?xt=urn:btih:0000000000000000000000000000000000000000";
    const TORRENT: &[u8] = b"d4:infod6:lengthi4e4:name8:test.bin12:piece lengthi16384e6:pieces20:00000000000000000000ee";

    async fn store() -> Result<(std::path::PathBuf, BlobStore), crate::blob_store::BlobError> {
        let root =
            std::env::temp_dir().join(format!("fluxdown-magnet-preview-{}", uuid::Uuid::new_v4()));
        let blobs = BlobStore::open(root.clone()).await?;
        Ok((root, blobs))
    }

    #[tokio::test]
    async fn cancellation_interrupts_resolution_and_releases_reservation() {
        let (root, blobs) = store().await.expect("blob store");
        let previews = MagnetPreviews::default();
        let ticket = previews.prepare(URL.to_owned()).expect("reserve");
        let started = tokio::sync::Notify::new();
        let work = previews.resolve_with(&blobs, &ticket.preview_id, |_, _| async {
            started.notify_one();
            std::future::pending().await
        });
        let cancel = async {
            started.notified().await;
            previews.cancel(&ticket.preview_id);
            previews.cancel(&ticket.preview_id);
        };
        let (result, ()) = tokio::join!(work, cancel);
        assert_eq!(
            result.expect_err("cancelled").data.expect("code").code,
            ApplicationErrorCode::Cancelled
        );
        assert!(previews.jobs().is_empty());
        tokio::fs::remove_dir(root)
            .await
            .expect("no blob was written");
    }

    #[tokio::test]
    async fn timeout_and_cancel_before_start_never_create_a_blob() {
        let (root, blobs) = store().await.expect("blob store");
        let previews = MagnetPreviews::default();
        let ticket = previews.prepare(URL.to_owned()).expect("reserve");
        previews
            .jobs()
            .get_mut(&ticket.preview_id)
            .expect("job")
            .deadline = Instant::now() + Duration::from_millis(20);
        let result = previews
            .resolve_with(&blobs, &ticket.preview_id, |_, _| std::future::pending())
            .await;
        assert_eq!(
            result.expect_err("timeout").data.expect("code").code,
            ApplicationErrorCode::Timeout
        );
        assert!(previews.jobs().is_empty());
        let ticket = previews.prepare(URL.to_owned()).expect("reserve");
        previews.cancel(&ticket.preview_id);
        let result = previews
            .resolve_with(&blobs, &ticket.preview_id, |_, _| async {
                panic!("cancelled reservation must not resolve")
            })
            .await;
        assert_eq!(
            result.expect_err("removed").data.expect("code").code,
            ApplicationErrorCode::NotFound
        );
        tokio::fs::remove_dir(root)
            .await
            .expect("no blob was written");
    }

    #[tokio::test]
    async fn dropping_a_pending_request_cancels_its_worker() {
        let (root, blobs) = store().await.expect("blob store");
        let previews = MagnetPreviews::default();
        let ticket = previews.prepare(URL.to_owned()).expect("reserve");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut request =
            Box::pin(
                previews.resolve_with(&blobs, &ticket.preview_id, |_, cancel| async move {
                    sender.send(cancel).expect("worker cancellation token");
                    std::future::pending().await
                }),
            );
        let worker = tokio::select! {
            token = receiver => token.expect("worker started"),
            _ = &mut request => panic!("worker should still be waiting"),
        };
        drop(request);
        assert!(worker.is_cancelled());
        assert!(previews.jobs().is_empty());
        tokio::fs::remove_dir(root).await.expect("no blob written");
    }

    #[tokio::test]
    async fn metadata_is_retained_for_confirmation_and_malformed_metadata_is_cleaned() {
        let (root, blobs) = store().await.expect("blob store");
        let previews = MagnetPreviews::default();
        let ticket = previews.prepare(URL.to_owned()).expect("reserve");
        let result = previews
            .resolve_with(&blobs, &ticket.preview_id, |_, _| async {
                Ok(TORRENT.to_vec())
            })
            .await
            .expect("metadata preview");
        assert_eq!(result.name, "test.bin");
        assert_eq!(result.files.len(), 1);
        assert!(previews.claim(&ticket.preview_id).is_err());
        crate::torrent_preview::discard(&blobs, &result.torrent_blob_id)
            .await
            .expect("discard confirmed preview");
        let ticket = previews
            .prepare(URL.to_owned())
            .expect("reserve malformed fixture");
        assert!(
            previews
                .resolve_with(&blobs, &ticket.preview_id, |_, _| async {
                    Ok(b"broken".to_vec())
                })
                .await
                .is_err()
        );
        assert!(previews.jobs().is_empty());
        tokio::fs::remove_dir(root)
            .await
            .expect("all blobs cleaned");
    }

    #[test]
    fn reservations_are_bounded_expire_and_cannot_run_twice() {
        let previews = MagnetPreviews::default();
        assert!(
            previews
                .prepare("https://example.test/a.torrent".to_owned())
                .is_err()
        );
        let tickets = (0..MAX_RESERVED)
            .map(|_| previews.prepare(URL.to_owned()).expect("reserve"))
            .collect::<Vec<_>>();
        assert!(previews.prepare(URL.to_owned()).is_err());
        let active = tickets
            .iter()
            .take(MAX_RUNNING)
            .map(|t| previews.claim(&t.preview_id).expect("claim"))
            .collect::<Vec<_>>();
        assert!(previews.claim(&tickets[0].preview_id).is_err());
        assert!(previews.claim(&tickets[MAX_RUNNING].preview_id).is_err());
        drop(active);
        let recovered = previews
            .claim(&tickets[MAX_RUNNING].preview_id)
            .expect("capacity released");
        drop(recovered);
        for job in previews.jobs().values_mut() {
            job.deadline = Instant::now() - Duration::from_secs(1);
        }
        let fresh = previews
            .prepare(URL.to_owned())
            .expect("expired reservations reclaimed");
        assert_eq!(previews.jobs().len(), 1);
        previews.cancel(&fresh.preview_id);
    }
}
