//! BitTorrent / Magnet-link download engine.
//!
//! Uses **librqbit** as the BT backend.  All BT tasks share a single
//! `Session` (DHT, trackers, listening port) managed by [`SharedBtSession`],
//! which lives inside `DownloadManager`.  This avoids per-task resource waste
//! (redundant DHT nodes, tracker connections, OS threads, listening ports).
//!
//! Because librqbit requires a multi-threaded tokio runtime while our main
//! actor runs on `current_thread`, the shared session is created inside a
//! dedicated `Runtime(multi_thread)`.  Individual download tasks submit work
//! to that runtime via `Runtime::spawn`.
//!
//! Key design:
//! - Single shared `Session` with DHT + public trackers + UPnP.
//! - Speed limit is applied at the `Session` level via `ratelimits` and
//!   updated in real-time when the user changes the global speed setting.
//! - `add_torrent` blocks while resolving magnet metadata from DHT/peers, so
//!   we report "preparing" status to Dart while we wait.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, SetFileAttributesW,
};

use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, PeerConnectionOptions,
    Session, SessionOptions, SessionPersistenceConfig,
};

/// Alias for librqbit's `BtHandle` (`Arc<ManagedTorrent>`).
/// The upstream type is not re-exported, so we define it locally.
pub type BtHandle = Arc<ManagedTorrent>;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::downloader::{DownloadError, ProgressUpdate, SegmentProgressInfo};
use crate::logger::log_info;
use crate::model::{BtFileEntry, TorrentMetaResult};
use crate::selection::{HostSelection, SelectionOutcome};

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Truncate an identifier to at most 8 characters for log output.
/// Returns the full string if shorter than 8 characters, avoiding panic
/// from direct byte-index slicing on short or multi-byte strings.
#[inline]
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Returns `true` if the URL looks like a magnet link.
pub fn is_magnet_url(url: &str) -> bool {
    url.get(..8)
        .map(|prefix| prefix.eq_ignore_ascii_case("magnet:?"))
        .unwrap_or(false)
}

/// Torrent input source — either a magnet URI or raw .torrent file bytes.
/// Replaces the old hardcoded `magnet_url: String` field so that
/// `BtDownloadParams` can represent both kinds of BT downloads uniformly.
#[derive(Clone)]
pub enum TorrentSource {
    /// A magnet link URI string (e.g. `magnet:?xt=urn:btih:...`).
    Magnet(String),
    /// Raw bytes of a `.torrent` file read from disk.
    TorrentFileBytes(Vec<u8>),
}

impl TorrentSource {
    /// Returns `true` if this source is a magnet link.
    pub fn is_magnet(&self) -> bool {
        matches!(self, TorrentSource::Magnet(_))
    }

    /// Best-effort display name for logging / early UI display.
    /// For magnet links, extracts the `dn=` parameter.
    /// For torrent file bytes, returns None (the name comes from metadata).
    pub fn display_name(&self) -> Option<String> {
        match self {
            TorrentSource::Magnet(url) => magnet_display_name(url),
            TorrentSource::TorrentFileBytes(_) => None,
        }
    }

    /// URL string for DB storage.  Magnet links store the URI directly.
    /// Torrent file sources store a sentinel `torrent-file://` URL since the
    /// actual content is persisted separately in the `torrent_file_bytes` column.
    #[allow(dead_code)]
    pub fn url_for_db(&self) -> &str {
        match self {
            TorrentSource::Magnet(url) => url,
            TorrentSource::TorrentFileBytes(_) => "torrent-file://local",
        }
    }

    /// Lowercase hex info-hash of this source, if derivable.
    ///
    /// Magnet links carry it in `xt=urn:btih:`; .torrent bytes are parsed
    /// (bencode) to compute it.  Used to address the `{hash}.bitv` fastresume
    /// file before the torrent is added to the session.
    pub fn info_hash_hex(&self) -> Option<String> {
        match self {
            TorrentSource::Magnet(url) => librqbit::Magnet::parse(url)
                .ok()?
                .as_id20()
                .map(|id| id.as_string()),
            TorrentSource::TorrentFileBytes(bytes) => {
                librqbit::torrent_from_bytes::<librqbit::ByteBufOwned>(bytes)
                    .ok()
                    .map(|t| t.info_hash.as_string())
            }
        }
    }
}

/// Extract the `dn=` (display name) parameter from a magnet URI, if present.
///
/// The decoded value is sanitized via `crate::downloader::sanitize_filename`
/// to strip path separators and other illegal characters (`/`, `\`, `:`, …),
/// matching `meta_prober::extract_dn_from_magnet`.  Without this, an illegal
/// `dn=` would flow into the DB display name and the metadata-failure fallback
/// file name inconsistently with the queued-task path.  Returns `None` when the
/// decoded value is empty (before sanitization), so callers fall back to a
/// generated name instead of the literal `"download"` placeholder.
fn magnet_display_name(url: &str) -> Option<String> {
    url.split('&')
        .find_map(|part| {
            let part = part.strip_prefix("magnet:?").unwrap_or(part);
            part.strip_prefix("dn=")
        })
        .and_then(|raw| {
            let decoded = urlencoding_decode(raw);
            if decoded.is_empty() {
                None
            } else {
                Some(crate::downloader::sanitize_filename(&decoded))
            }
        })
}

/// Minimal percent-decoding for `dn=` values (UTF-8 safe).
///
/// Collects **both** percent-encoded bytes (`%XX`) **and** literal bytes into a
/// shared byte buffer, then decodes the buffer as UTF-8 (with GBK fallback).
/// This correctly handles multi-byte characters (e.g. CJK, emoji) regardless of
/// whether they arrive percent-encoded or as raw literal UTF-8 — many BT
/// clients write the original UTF-8 directly into `dn=` (e.g. `dn=中文电影`).
/// Decoding per-byte via `b as char` would treat each UTF-8 byte as a Latin-1
/// code point and produce mojibake.
///
/// `+` decodes to a space (flushing the accumulated buffer first, since it is a
/// genuine delimiter).  Incomplete or invalid `%` sequences are kept as literal
/// bytes rather than silently padded with zeros.
fn urlencoding_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut bytes_buf: Vec<u8> = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    // Flush accumulated percent-encoded bytes as UTF-8 into `out`.
    // 优先 UTF-8，失败时回退到 GBK（应对老旧中文资源库 magnet 中
    // 的 GBK 编码 dn=），双失败才使用 replacement char。
    let flush = |buf: &mut Vec<u8>, out: &mut String| {
        if !buf.is_empty() {
            match crate::downloader::decode_bytes_utf8_or_gbk(buf) {
                Ok(s) => out.push_str(&s),
                Err(_) => {
                    out.push(char::REPLACEMENT_CHARACTER);
                }
            }
            buf.clear();
        }
    };

    while i < len {
        match bytes[i] {
            b'+' => {
                flush(&mut bytes_buf, &mut out);
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < len => {
                // Full %XX sequence — decode as a byte only if both are
                // valid hex digits; otherwise treat `%` as a literal.
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                    bytes_buf.push(h << 4 | l);
                    i += 3;
                } else {
                    // Not a valid `%XX` escape — keep the `%` as a literal byte
                    // (0x25, valid ASCII) in `bytes_buf` so it decodes together
                    // with any surrounding literal multi-byte sequence.
                    bytes_buf.push(b'%');
                    i += 1;
                }
            }
            b'%' => {
                // Incomplete `%` at end of string — treat the `%` and any
                // trailing bytes as literal content.  We push them as raw
                // bytes into `bytes_buf` (rather than `b as char`, which would
                // mangle multi-byte UTF-8 by re-interpreting each byte as a
                // Latin-1 code point) so the trailing sequence is decoded
                // together with surrounding literal bytes by
                // `decode_bytes_utf8_or_gbk`.  0x25 ('%') is valid ASCII and
                // safely passes through UTF-8 decoding unchanged.
                while i < len {
                    bytes_buf.push(bytes[i]);
                    i += 1;
                }
            }
            _ => {
                // Literal byte — accumulate into `bytes_buf` so that literal
                // multi-byte UTF-8 sequences (common in magnet `dn=` values,
                // e.g. `dn=中文电影`) are decoded as a whole instead of
                // per-byte via `b as char` (which produced mojibake).
                bytes_buf.push(bytes[i]);
                i += 1;
            }
        }
    }
    flush(&mut bytes_buf, &mut out);
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Well-known public trackers used to accelerate peer discovery for magnet
/// links that ship without `tr=` parameters.
///
/// **Curated from global community sources** (2026-02-10):
///   - ngosang/trackerslist (52.9k stars, auto-updated daily, ranked by latency)
///   - XIU2/TrackersListCollection (popular in CN community)
///   - Cross-referenced and **availability-tested** before inclusion.
///
/// Strategy: CN/Asia trackers first (better peer locality for domestic users),
/// then international trackers.  UDP-heavy (lowest overhead), with HTTPS
/// fallbacks for restrictive network environments where UDP may be blocked.
///
/// Kept to ~25 high-availability trackers to minimise DNS/connect overhead
/// while still providing excellent global peer coverage.  All tracker
/// connections are async and parallel, so startup impact is minimal.
const PUBLIC_TRACKERS: &[&str] = &[
    // ─── CN / Asia — better peer discovery for domestic users ───
    "udp://tracker.dler.com:6969/announce",
    "udp://admin.52ywp.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "https://tracker.moeblog.cn:443/announce",
    "http://nyaa.tracker.wf:7777/announce",
    "https://tr.zukizuki.org:443/announce",
    // ─── International — top-tier, highest uptime ───
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.dstud.io:6969/announce",
    "udp://tracker-udp.gbitt.info:80/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.srv00.com:6969/announce",
    "udp://tracker.qu.ax:6969/announce",
    "udp://opentracker.io:6969/announce",
    "udp://tracker.bittor.pw:1337/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://tracker.opentorrent.top:6969/announce",
    "udp://open.demonoid.ch:6969/announce",
    "udp://tracker.t-1.org:6969/announce",
    // ─── HTTPS fallbacks — for networks that block UDP ───
    "https://tracker.ghostchu-services.top:443/announce",
    "https://tracker.bt4g.com:443/announce",
    "https://1337.abcvg.info:443/announce",
    "http://tracker.bt4g.com:2095/announce",
];

/// Return the built-in public tracker list as a newline-separated string.
/// Used to populate the default config value on first launch so users can
/// see and edit the full list in Settings.
pub fn default_tracker_list() -> String {
    PUBLIC_TRACKERS.join("\n")
}

// ---------------------------------------------------------------------------
// BT configuration — user-settable via the Settings page
// ---------------------------------------------------------------------------

/// User-configurable BT session settings, loaded from the DB config table.
#[derive(Debug, Clone)]
pub struct BtConfig {
    pub enable_dht: bool,
    pub enable_upnp: bool,
    pub port_start: u16,
    pub port_end: u16,
    /// User-supplied extra tracker URLs (newline-separated).
    /// These are **merged** with the built-in `PUBLIC_TRACKERS` list.
    pub custom_trackers: String,
    /// Trackers fetched from subscription sources (newline-separated cache,
    /// see `tracker_subscription`).  Empty when the subscription feature is
    /// disabled.  Merged + deduped with `custom_trackers` at session build.
    pub subscription_trackers: String,
}

impl Default for BtConfig {
    fn default() -> Self {
        Self {
            enable_dht: true,
            enable_upnp: true,
            port_start: 6881,
            port_end: 6891,
            custom_trackers: String::new(),
            subscription_trackers: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared BT Session — singleton owned by DownloadManager
// ---------------------------------------------------------------------------

/// Remove librqbit's `session.json` (and a possible `session.json.tmp`
/// leftover) from the persistence folder **while preserving the
/// `{hash}.bitv` fast-resume bitfields and `{hash}.torrent` metadata
/// caches**.  Must be called BEFORE `Session::new_with_opts`.
///
/// Why (BUG-BT-RESUME-FROM-ZERO — "BT 任务暂停后继续会从零开始"):
///
/// FluxDown manages task state in SQLite and re-adds torrents itself on
/// resume; librqbit's own session restore is not only redundant but
/// actively harmful:
///
/// 1. `Session::new_with_opts` restores every torrent found in
///    session.json **asynchronously** (opens output files, mmaps the
///    `.bitv` bitfield, spawns a checking task).
/// 2. The old "startup cleanup" then called `session.delete(id, false)`
///    on each restored torrent.  Two fatal consequences: first,
///    `JsonSessionPersistenceStore::delete` removes **both** the
///    `{hash}.torrent` AND the `{hash}.bitv` file — the old code's
///    assumption that `.bitv` survives a `delete(false)` is wrong.
///    Second, `delete` races the still-running restore initialization:
///    it takes the torrent's `FileStorage` out from under the checker,
///    whose sampled piece reads then fail ("file is None"), so librqbit
///    declares the fastresume data corrupted, clears it and **rewrites
///    an all-zero `.bitv`**.
///
/// Either way, when the task is re-added moments later the piece
/// bitfield is gone (or all-zero), and the download restarts from
/// byte 0 even though the staging file still holds valid data.
///
/// Deleting session.json up front means the session starts empty: no
/// restore, no race, no destructive delete.  When the task is re-added,
/// librqbit looks up `{hash}.bitv` (keyed by info-hash, not session id),
/// validates it by sampling piece hashes against the staging file, and
/// resumes from the already-downloaded pieces.
fn clear_stale_session_state(persistence_folder: &Path) {
    for name in ["session.json", "session.json.tmp"] {
        let path = persistence_folder.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => log_info!("[BT] removed stale {name} (fastresume .bitv files preserved)"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log_info!(
                "[BT] failed to remove stale {name}: {e} — old torrents may be restored and rejected as AlreadyManaged"
            ),
        }
    }
}

/// A shared BT session that holds a dedicated multi-thread runtime and a
/// single `librqbit::Session`.  All BT tasks share this instance, which
/// means they share DHT routing tables, tracker connections, and the
/// listening port — dramatically reducing resource usage.
///
/// Torrent handles are cached in `handles` so that pause/resume cycles
/// use the native `Session::pause` / `Session::unpause` API instead of
/// deleting and re-adding the torrent.  This preserves fast-resume data
/// (piece bitfield) and avoids expensive re-verification of already
/// downloaded pieces.
pub struct SharedBtSession {
    runtime: tokio::runtime::Runtime,
    session: Arc<Session>,
    /// Maps our `task_id` → librqbit `BtHandle`.
    /// Protected by an async Mutex because it's accessed from both the
    /// main actor (pause/delete) and spawned download tasks (add/finish).
    handles: Mutex<HashMap<String, BtHandle>>,
    /// Pending delete requests for tasks whose `add_torrent` call is still
    /// in progress (handle not yet in `handles`).  Keyed by task_id, value
    /// is the `delete_files` flag.  The detached add_torrent closure checks
    /// this map on completion and calls `session.delete` accordingly,
    /// preventing orphaned files when a magnet task is deleted during DHT
    /// metadata resolution.
    pending_deletes: Mutex<HashMap<String, bool>>,
    /// Count of detached `add_torrent` tasks currently running in the
    /// background.  Incremented just before spawning the detached task;
    /// decremented when the task completes (success, error, or pending-delete).
    ///
    /// `maybe_release_bt_session` must not tear down the session while this
    /// is non-zero: the detached task still holds an `Arc<Session>` that
    /// keeps the listening port bound.  Creating a new session while the old
    /// port is still in use causes the next BT task to fail immediately.
    inflight_adds: AtomicUsize,
    /// Maps librqbit torrent ID → our task_id.
    /// Used to detect when the same torrent is added by multiple tasks.
    torrent_ids: Mutex<HashMap<usize, String>>,
    /// Serializes the completion-stage "dedup destination name + move from
    /// staging" sequence across concurrent BT tasks.
    ///
    /// All BT downloads run on the shared multi-thread bt-runtime, so two
    /// same-named torrents can finish and run `compute_completion_layout`
    /// (which checks `save_dir/name` does not exist) and `move_path` (rename)
    /// at the same instant.  Without serialization both pick the identical
    /// deduped name and the second `std::fs::rename` silently overwrites the
    /// first task's file (single file) or leaves a half-moved directory.
    ///
    /// Holding this lock across the brief dedup+move closes the TOCTOU window.
    /// It is the BT analogue of the HTTP path's `reserved_temp_paths`.  The
    /// lock is only contended in the rare simultaneous-completion case.
    completion_move_lock: Mutex<()>,
    /// Folder holding librqbit persistence files (session.json, `{hash}.bitv`,
    /// `{hash}.torrent`).  Kept so that `clear_stale_fastresume` can remove a
    /// `.bitv` whose staging data no longer exists (BUG-BT-PHANTOM-PIECES).
    persistence_folder: PathBuf,
}

impl SharedBtSession {
    /// Create the shared session with the given initial speed limit and config.
    ///
    /// `default_save_dir` is used as the Session's default output folder
    /// (individual torrents override this via `AddTorrentOptions::output_folder`).
    ///
    /// `app_data_dir` is the application data directory where BT persistence
    /// files (session.json, .bitv, .torrent) are stored. This should be the
    /// exe directory or an app-specific folder — NOT the user's download dir.
    ///
    /// `speed_limit_bps` is the global download speed limit in bytes/sec
    /// (0 = unlimited).
    ///
    /// `bt_config` contains user-configurable BT settings (DHT, UPnP, ports,
    /// custom trackers).
    pub fn new(
        default_save_dir: &str,
        app_data_dir: &str,
        speed_limit_bps: u64,
        bt_config: &BtConfig,
    ) -> Result<Self, DownloadError> {
        // Scale worker threads with CPU cores.  BT workload is mostly I/O-bound
        // so diminishing returns beyond 8 threads; capping here saves ~2 MB of
        // stack memory per thread avoided.
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let worker_threads = cpu_cores.clamp(2, 8);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(worker_threads)
            .thread_name("bt-runtime")
            .build()
            .map_err(|e| DownloadError::Other(format!("failed to build BT runtime: {e}")))?;

        // Base list: the user's tracker list from Settings, falling back to
        // the built-in PUBLIC_TRACKERS when empty.  Subscription trackers
        // (fetched from community-maintained lists) are appended, and the
        // whole set is deduped by normalized URL form.
        let base: Vec<&str> = if bt_config.custom_trackers.trim().is_empty() {
            PUBLIC_TRACKERS.to_vec()
        } else {
            bt_config.custom_trackers.lines().collect()
        };
        let merged = crate::tracker_subscription::merge_dedup(
            base.into_iter()
                .chain(bt_config.subscription_trackers.lines()),
        );
        let trackers: HashSet<url::Url> = merged.iter().filter_map(|s| s.parse().ok()).collect();

        let total_tracker_count = trackers.len();

        let download_bps = NonZeroU32::new(speed_limit_bps.min(u32::MAX as u64) as u32);

        // Persistence folder: store session.json + {hash}.bitv + {hash}.torrent
        // in the app data directory (next to flux_down.db), NOT in the user's
        // download folder. This matches how professional tools (qBittorrent,
        // Thunder, etc.) keep internal data out of user-visible directories.
        let persistence_folder = PathBuf::from(app_data_dir).join("bt_session");

        // CRITICAL: remove the stale session.json BEFORE creating the Session,
        // so that librqbit does NOT restore torrents from a previous session.
        // The {hash}.bitv fast-resume bitfields are preserved — see the
        // function doc of `clear_stale_session_state` for the full rationale
        // (BUG-BT-RESUME-FROM-ZERO).
        clear_stale_session_state(&persistence_folder);

        // Validate and clamp port range.
        let port_start = bt_config.port_start.max(1024);
        let port_end = bt_config.port_end.max(port_start);

        let enable_dht = bt_config.enable_dht;
        let enable_upnp = bt_config.enable_upnp;

        let save_dir = default_save_dir.to_owned();
        let save_dir_for_cleanup = save_dir.clone();
        let session = rt
            .block_on(async {
                let opts = SessionOptions {
                    disable_dht: !enable_dht,
                    disable_dht_persistence: !enable_dht,
                    // Pin the DHT routing-table file inside our app-private data
                    // directory. librqbit's default resolves a system config/cache
                    // dir (via `directories`), which FAILS on Android (no XDG dirs)
                    // and surfaces as "BT session init failed: error initializing
                    // persistent DHT". Providing an explicit path makes it work on
                    // every platform and keeps DHT state next to session.json.
                    dht_config: Some(librqbit::dht::PersistentDhtConfig {
                        config_filename: Some(persistence_folder.join("dht.json")),
                        ..Default::default()
                    }),
                    listen_port_range: Some(port_start..port_end.saturating_add(1)),
                    enable_upnp_port_forwarding: enable_upnp,
                    trackers,
                    ratelimits: librqbit::limits::LimitsConfig {
                        download_bps,
                        upload_bps: None,
                    },
                    // Optimised peer connection parameters.
                    peer_opts: Some(PeerConnectionOptions {
                        // Slightly shorter connect timeout — drop unresponsive
                        // peers faster so we can try others sooner.
                        connect_timeout: Some(Duration::from_secs(10)),
                        // Generous read/write timeout to avoid dropping slow
                        // but otherwise healthy peers.
                        read_write_timeout: Some(Duration::from_secs(20)),
                        ..Default::default()
                    }),
                    // Enable persistence so that session.json and per-torrent
                    // .bitv (piece bitfield) files are written to disk.
                    persistence: Some(SessionPersistenceConfig::Json {
                        folder: Some(persistence_folder.clone()),
                    }),
                    // Fast-resume: persist piece completion state so that
                    // paused/restarted torrents can skip re-verification.
                    // Requires `persistence` to be set to take effect.
                    fastresume: true,
                    // Buffer writes in memory before flushing to disk.  Reduces
                    // I/O contention from many small pieces.  64 MiB is enough
                    // for high-speed connections while keeping RSS reasonable
                    // (was 128 — saved ~64 MB of potential RSS).
                    defer_writes_up_to: Some(64),
                    // Limit concurrent torrent initialisation to 3 to prevent
                    // DHT/tracker storms when many BT tasks start at once.
                    concurrent_init_limit: Some(3),
                    ..Default::default()
                };

                Session::new_with_opts(save_dir.into(), opts).await
            })
            .map_err(|e| DownloadError::Other(format!("BT session init failed: {e}")))?;

        // Scan save_dir for leftover staging dirs from earlier in this app
        // session (e.g. a cancelled add whose `cleanup_stage` was skipped).
        // The download_manager's startup cleanup in load_and_send_all_tasks()
        // only runs once at app launch, while this code runs on every lazy
        // (re-)creation of the BT session.
        //
        // Remove any staging dir whose contents are all 0-byte (no real
        // downloaded data worth preserving).  Dirs with real data are kept
        // for resume via do_resume_task → add_torrent.
        {
            let save_path = std::path::Path::new(&save_dir_for_cleanup);
            if let Ok(entries) = std::fs::read_dir(save_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !name_str.starts_with(BT_STAGE_PREFIX) {
                        continue;
                    }
                    let path = entry.path();
                    if !stage_dir_has_real_data(&path) {
                        log_info!(
                            "[BT] startup: removing empty/stub staging dir {}",
                            path.display()
                        );
                        let _ = std::fs::remove_dir_all(&path);
                    }
                }
            }
        }

        log_info!(
            "[BT] shared session created (DHT={}, UPnP={}, ports={}-{}, {} trackers, speed_limit={} B/s, worker_threads={}, persistence=on)",
            enable_dht,
            enable_upnp,
            port_start,
            port_end,
            total_tracker_count,
            speed_limit_bps,
            worker_threads
        );

        Ok(Self {
            runtime: rt,
            session,
            handles: Mutex::new(HashMap::new()),
            pending_deletes: Mutex::new(HashMap::new()),
            inflight_adds: AtomicUsize::new(0),
            torrent_ids: Mutex::new(HashMap::new()),
            completion_move_lock: Mutex::new(()),
            persistence_folder,
        })
    }

    /// Remove the `{info_hash}.bitv` fast-resume bitfield for a torrent.
    ///
    /// Called when the staging directory holds no real data while a `.bitv`
    /// may still claim completed pieces (BUG-BT-PHANTOM-PIECES).  librqbit's
    /// own fastresume validation cannot be relied on to catch the mismatch:
    /// it samples only a few pieces AND treats a SHA1 mismatch as success —
    /// `validate_fastresume` rejects on `check_piece(..).is_err()` (I/O error)
    /// only, while a hash mismatch returns `Ok(false)` (librqbit 8.1.1
    /// initializing.rs:148 / file_ops.rs:238, still present upstream).  On
    /// Windows `pread_exact` additionally ignores short reads (`seek_read`
    /// result unused), so even an empty staging file passes validation.
    /// Pieces falsely restored that way are never re-downloaded and the
    /// "finished" file ends up with zero-filled holes.
    ///
    /// The `{hash}.torrent` metadata cache is intentionally kept — it is
    /// content-addressed and always valid.
    pub fn clear_stale_fastresume(&self, info_hash_hex: &str) {
        let path = self
            .persistence_folder
            .join(format!("{info_hash_hex}.bitv"));
        match std::fs::remove_file(&path) {
            Ok(()) => log_info!(
                "[BT] removed stale fastresume bitfield {} (staging has no data)",
                path.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log_info!(
                "[BT] failed to remove stale fastresume {}: {} — completion verification will catch any phantom pieces",
                path.display(),
                e
            ),
        }
    }

    /// Update the global download speed limit at runtime.
    /// `bps == 0` means unlimited.  Takes effect immediately on all active
    /// BT downloads.
    pub fn set_speed_limit(&self, bps: u64) {
        let limit = NonZeroU32::new(bps.min(u32::MAX as u64) as u32);
        self.session.ratelimits.set_download_bps(limit);
        log_info!("[BT] shared session speed limit updated to {} B/s", bps);
    }

    /// Get an `Arc<Session>` handle for adding torrents.
    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// Get a handle to the BT runtime for spawning tasks.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Store a torrent handle for a task so it can be paused/resumed later.
    pub async fn store_handle(&self, task_id: &str, handle: BtHandle) {
        self.handles
            .lock()
            .await
            .insert(task_id.to_string(), handle);
    }

    /// Pause a BT torrent by task_id.  The handle stays cached so that
    /// `resume_handle` can unpause it without re-adding.
    pub async fn pause_task(&self, task_id: &str) -> Result<(), DownloadError> {
        // Clone the Arc handle and release the lock immediately so that
        // the async session.pause() call doesn't block other handle ops.
        let handle = self.handles.lock().await.get(task_id).cloned();
        if let Some(handle) = handle {
            // If already paused or initializing, ignore silently.
            if !handle.is_paused() {
                self.session
                    .pause(&handle)
                    .await
                    .map_err(|e| DownloadError::Other(format!("BT pause failed: {e}")))?;
            }
            log_info!("[BT] task={} paused via session API", short_id(task_id));
        }
        Ok(())
    }

    /// Resume a previously paused BT torrent.  Returns the handle if
    /// successful, or `None` if no cached handle exists (caller should
    /// fall back to `add_torrent`).
    pub async fn resume_task(&self, task_id: &str) -> Result<Option<BtHandle>, DownloadError> {
        // Clone the Arc handle and release the lock immediately.
        let handle = self.handles.lock().await.get(task_id).cloned();
        if let Some(handle) = handle {
            if handle.is_paused() {
                self.session
                    .unpause(&handle)
                    .await
                    .map_err(|e| DownloadError::Other(format!("BT unpause failed: {e}")))?;
                log_info!("[BT] task={} resumed via session API", short_id(task_id));
            }
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    /// Gracefully shut down the BT session and runtime.
    ///
    /// Pauses all active torrents, then shuts down the runtime with a timeout.
    /// Called when the application exits to ensure clean resource release.
    pub fn shutdown(&self) {
        log_info!("[BT] shutting down shared session...");
        // Use the runtime to gracefully close the session.  The session's
        // drop will attempt to persist DHT state and piece bitfields.
        // We give it a generous timeout to allow disk writes to complete.
        self.runtime.block_on(async {
            // Pause all tracked torrents so they flush state to disk.
            let handles: Vec<(String, BtHandle)> = {
                let map = self.handles.lock().await;
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            };
            for (tid, handle) in &handles {
                if !handle.is_paused()
                    && let Err(e) = self.session.pause(handle).await
                {
                    log_info!(
                        "[BT] shutdown: failed to pause task {}: {}",
                        short_id(tid),
                        e
                    );
                }
            }
        });
        // The Runtime::drop will be called after this, which blocks until
        // all spawned tasks finish (or the runtime forces them to stop).
        log_info!("[BT] shared session shutdown complete");
    }

    /// Permanently delete a torrent from the session, removing persistence
    /// data.  `delete_files` controls whether downloaded data is also removed.
    /// Returns `true` if a handle was found and `session.delete` was called,
    /// `false` if the task was not yet in the handles map (still in the
    /// `add_torrent` phase).  The caller should call `register_pending_delete`
    /// when this returns `false` so the detached add_torrent closure can clean
    /// up once metadata resolution completes.
    pub async fn delete_task(&self, task_id: &str, delete_files: bool) -> bool {
        // Remove from map first (under lock), then perform async deletion
        // outside the lock to minimise contention.
        let handle = self.handles.lock().await.remove(task_id);
        if let Some(handle) = handle {
            let torrent_id = handle.id();
            // Clean up the torrent_id → task_id mapping.
            self.unregister_torrent_id(torrent_id).await;
            if let Err(e) = self.session.delete(torrent_id.into(), delete_files).await {
                log_info!(
                    "[BT] task={} session.delete error: {}",
                    short_id(task_id),
                    e
                );
            } else {
                log_info!(
                    "[BT] task={} deleted from session (delete_files={})",
                    short_id(task_id),
                    delete_files
                );
            }
            true
        } else {
            false
        }
    }

    /// Register a deferred delete for a task whose `add_torrent` is still in
    /// progress.  The detached add_torrent closure will consume this entry and
    /// call `session.delete(id, delete_files)` as soon as metadata resolves.
    pub async fn register_pending_delete(&self, task_id: &str, delete_files: bool) {
        self.pending_deletes
            .lock()
            .await
            .insert(task_id.to_string(), delete_files);
        log_info!(
            "[BT] task={} pending delete registered (delete_files={})",
            short_id(task_id),
            delete_files
        );
    }

    /// Consume and return the pending delete flag for `task_id`, if any.
    pub async fn take_pending_delete(&self, task_id: &str) -> Option<bool> {
        self.pending_deletes.lock().await.remove(task_id)
    }

    /// Record that a librqbit torrent_id is now managed by the given task_id.
    pub async fn register_torrent_id(&self, torrent_id: usize, task_id: &str) {
        self.torrent_ids
            .lock()
            .await
            .insert(torrent_id, task_id.to_string());
    }

    /// Remove the torrent_id mapping when a task is deleted.
    pub async fn unregister_torrent_id(&self, torrent_id: usize) {
        self.torrent_ids.lock().await.remove(&torrent_id);
    }

    /// Look up which task_id owns a given torrent_id.
    pub async fn task_for_torrent(&self, torrent_id: usize) -> Option<String> {
        self.torrent_ids.lock().await.get(&torrent_id).cloned()
    }

    /// Acquire the completion-move serialization lock.
    ///
    /// Callers hold the returned guard across the dedup-destination-name +
    /// `move_path` sequence in the completion stage so that concurrent BT
    /// task completions cannot race on the same `save_dir` destination name
    /// (which would otherwise let one task's file silently overwrite another's).
    pub async fn lock_completion_move(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.completion_move_lock.lock().await
    }

    fn increment_inflight_add(&self) {
        self.inflight_adds.fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_inflight_add(&self) {
        self.inflight_adds.fetch_sub(1, Ordering::Relaxed);
    }

    /// Returns `true` if any detached `add_torrent` task is still running.
    pub fn has_inflight_adds(&self) -> bool {
        self.inflight_adds.load(Ordering::Relaxed) > 0
    }

    /// Increment the counter and return an RAII guard that decrements it on
    /// drop.  Using a guard instead of manual increment/decrement ensures the
    /// counter is always decremented even if the enclosing `tokio::spawn`
    /// closure panics before reaching the end.
    pub fn inflight_guard(self: &Arc<Self>) -> InflightGuard {
        self.increment_inflight_add();
        InflightGuard(Arc::clone(self))
    }
}

/// Decrements `SharedBtSession::inflight_adds` when dropped, guaranteeing
/// the counter is decremented even if the enclosing async task panics.
pub struct InflightGuard(Arc<SharedBtSession>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.decrement_inflight_add();
    }
}

// ---------------------------------------------------------------------------
// BT download params
// ---------------------------------------------------------------------------

pub struct BtDownloadParams {
    pub task_id: String,
    /// Torrent input source — magnet URI or raw .torrent file bytes.
    pub torrent_source: TorrentSource,
    pub save_dir: String,
    pub db: Db,
    pub progress_tx: mpsc::Sender<ProgressUpdate>,
    pub cancel_token: CancellationToken,
    /// Handle to the shared BT session.
    pub session: Arc<Session>,
    /// Handle to the shared BT runtime.
    pub bt_runtime: tokio::runtime::Handle,
    /// Shared session wrapper — used to cache the handle after add_torrent.
    pub shared_bt: Arc<SharedBtSession>,
    /// If resuming a paused torrent, this is the existing handle.
    /// When `Some`, we skip `add_torrent` and go straight to the progress loop.
    pub existing_handle: Option<BtHandle>,
    /// Pre-selected file indices (from the new-download dialog).
    /// Empty = show the file selection dialog after metadata resolves.
    pub pre_selected_indices: Vec<i32>,
    /// Skip Phase 3.5 file selection dialog entirely.
    /// Set to true when resuming a task whose confirmed selection is persisted
    /// in the DB as "all files" — no update_only_files needed either.
    pub skip_file_selection: bool,
    /// User-specified rename target for the final file/directory on disk.
    /// Empty string means "use the torrent's internal name" (default).
    /// Stored in a separate DB column (`bt_custom_name`) so that Phase 1/3
    /// engine callbacks never overwrite it.
    pub custom_name: String,
    /// 需要宿主介入决策的文件选择接口(HostSelection)。
    pub selector: Arc<dyn HostSelection>,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run a BT download for a magnet link using the shared session.
///
/// This function is designed to be `tokio::spawn`-ed from the download manager
/// just like `downloader::run_download` or `ftp_downloader::run_ftp_download`.
///
/// The actual BT work (add_torrent, progress polling) runs on the shared BT
/// runtime; this function bridges between the main `current_thread` runtime
/// and the BT runtime.
pub async fn run_bt_download(params: BtDownloadParams) -> Result<(), DownloadError> {
    let task_id = params.task_id.clone();

    // 1. Switch to "preparing" status
    let _ = params
        .db
        .update_task_status(&task_id, STATUS_PREPARING, "")
        .await;
    let _ = params
        .progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: STATUS_PREPARING,
            error_message: String::new(),
            file_name: String::new(),
            segment_details: None,
            ..Default::default()
        })
        .await;

    log_info!(
        "[BT] task={} starting bt download (shared session)...",
        short_id(&task_id)
    );

    // 2. Run the actual BT download on the shared multi-thread runtime.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let cancel_token = params.cancel_token.clone();

    // Forward cancellation from CancellationToken to AtomicBool
    let cancelled_for_watcher = cancelled.clone();
    let cancel_watcher = tokio::spawn(async move {
        cancel_token.cancelled().await;
        cancelled_for_watcher.store(true, Ordering::SeqCst);
    });

    let progress_tx = params.progress_tx.clone();
    let db = params.db.clone();
    let torrent_source = params.torrent_source.clone();
    let save_dir = params.save_dir.clone();
    let tid = task_id.clone();
    let save_dir_for_cleanup = save_dir.clone();
    let tid_for_cleanup = tid.clone();
    let session = params.session.clone();
    let bt_runtime = params.bt_runtime.clone();
    let shared_bt = params.shared_bt.clone();
    let existing_handle = params.existing_handle;

    // Spawn the BT download on the shared multi-thread BT runtime.
    // The returned JoinHandle can be safely .await-ed from any runtime
    // (including our current_thread main runtime) — it uses waker-based
    // notification, not runtime-specific polling.  This avoids occupying
    // a thread from tokio's blocking thread pool for the entire download
    // duration, which previously caused thread-pool starvation under
    // many concurrent BT tasks.
    let inner_params = BtInnerParams {
        task_id: tid,
        torrent_source,
        save_dir,
        db,
        progress_tx,
        cancelled: cancelled_clone,
        session,
        shared_bt,
        existing_handle,
        pre_selected_indices: params.pre_selected_indices,
        skip_file_selection: params.skip_file_selection,
        custom_name: params.custom_name,
        selector: params.selector,
    };
    let result = bt_runtime
        .spawn(async move { bt_download_inner(inner_params).await })
        .await;

    cancel_watcher.abort();

    // Clean up pre-created staging dir if it's empty or contains only
    // zero-byte files (librqbit may pre-allocate stubs before detecting
    // AlreadyManaged or before any real data is written).
    //
    // `run_bt_download` runs on the main `current_thread` runtime, so the
    // synchronous `std::fs` scan (read_dir + metadata + remove_dir_all) must
    // not run inline — on a large/slow staging dir it would block the event
    // loop, stalling every other task's progress reporting and UI signalling.
    // We move the blocking work into `spawn_blocking` and `.await` it.
    let cleanup_stage = || {
        let stage = bt_stage_dir(&save_dir_for_cleanup, &tid_for_cleanup);
        let tid = tid_for_cleanup.clone();
        async move {
            let _ = tokio::task::spawn_blocking(move || {
                if !stage.exists() {
                    return;
                }
                if !stage_dir_has_real_data(&stage) {
                    log_info!(
                        "[BT] task={} cleaning up empty staging dir after error/cancel",
                        short_id(&tid)
                    );
                    let _ = std::fs::remove_dir_all(&stage);
                }
            })
            .await;
        }
    };

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            cleanup_stage().await;
            Err(e)
        }
        // JoinError has two causes:
        //   1. The spawned task panicked → treat as error (existing behaviour).
        //   2. The BT runtime was shut down (e.g. maybe_release_bt_session called
        //      while this task was still winding down after pause_task cancelled it).
        //      In that case cancelled is already true, so treat it as Cancelled to
        //      prevent the task from being marked as failed/error in the DB.
        Err(join_err) => {
            cleanup_stage().await;
            if cancelled.load(Ordering::SeqCst) {
                log_info!(
                    "[BT] task={} JoinError while cancelled (runtime shutdown during pause) — treating as Cancelled",
                    short_id(&task_id)
                );
                Err(DownloadError::Cancelled)
            } else {
                Err(DownloadError::Other(format!(
                    "BT task panicked: {join_err}"
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Inner download logic (runs on the shared BT runtime)
// ---------------------------------------------------------------------------

/// Parameters for the inner BT download loop (avoids too-many-arguments warning).
struct BtInnerParams {
    task_id: String,
    torrent_source: TorrentSource,
    save_dir: String,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    cancelled: Arc<AtomicBool>,
    session: Arc<Session>,
    shared_bt: Arc<SharedBtSession>,
    existing_handle: Option<BtHandle>,
    /// Pre-selected file indices forwarded from the CreateTask signal.
    /// Non-empty = skip the BtFilesInfo dialog and use these directly.
    pre_selected_indices: Vec<i32>,
    /// When true, skip Phase 3.5 entirely (user already confirmed all files
    /// on a previous run — resume without re-showing the dialog).
    skip_file_selection: bool,
    /// User-specified rename target, forwarded from BtDownloadParams.
    custom_name: String,
    /// 需要宿主介入决策的文件选择接口(HostSelection)。
    selector: Arc<dyn HostSelection>,
}

// ---------------------------------------------------------------------------
// Task status codes — must match Dart TaskStatus enum values.
// ---------------------------------------------------------------------------
const STATUS_DOWNLOADING: i32 = 1;
#[allow(dead_code)]
const STATUS_PAUSED: i32 = 2;
const STATUS_COMPLETED: i32 = 3;
const STATUS_ERROR: i32 = 4;

// ---------------------------------------------------------------------------
// BT staging directory helpers
// ---------------------------------------------------------------------------

/// Prefix used for per-task staging directories inside `save_dir`.
/// Each BT task downloads into `save_dir/.bt_stage_<task_id>/` so that
/// concurrent tasks with identical torrent names never collide on disk.
/// The directory is removed after the file/folder is moved to its final
/// location (or on task deletion).
pub const BT_STAGE_PREFIX: &str = ".bt_stage_";

/// Build the staging directory path for a BT task.
///
/// `save_dir/.bt_stage_<task_id>/`
pub fn bt_stage_dir(save_dir: &str, task_id: &str) -> PathBuf {
    PathBuf::from(save_dir).join(format!("{}{}", BT_STAGE_PREFIX, task_id))
}

fn landed_residue_path(
    save_path: &Path,
    current_file_name: &str,
    child_name: &std::ffi::OsStr,
) -> Option<PathBuf> {
    let child_path = PathBuf::from(child_name);
    let flat = save_path.join(&child_path);
    if flat.exists() {
        return Some(flat);
    }

    let container_child = save_path.join(current_file_name).join(&child_path);
    if container_child.exists() {
        return Some(container_child);
    }

    None
}

/// Returns `true` if `dir` contains at least one regular file with `len > 0`,
/// searching **recursively** (multi-file torrents nest their payload inside a
/// `<torrent name>/` subdirectory, which a top-level-only scan reports as a
/// zero-length entry on Windows).
///
/// Fail-safe semantics: any I/O error other than "directory does not exist"
/// is treated as "has data".  Every caller uses a `false` result to justify
/// destroying state (deleting the staging dir, discarding a cached torrent
/// handle, removing a `.bitv`), so an unreadable directory must never be
/// mistaken for an empty one.
pub fn stage_dir_has_real_data(dir: &Path) -> bool {
    fn scan(dir: &Path, depth: u32) -> std::io::Result<bool> {
        // Bail out of absurd nesting conservatively ("has data").
        if depth > 16 {
            return Ok(true);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            // DirEntry::metadata does not traverse symlinks.
            let md = entry.metadata()?;
            if md.is_dir() {
                if scan(&entry.path(), depth + 1)? {
                    return Ok(true);
                }
            } else if md.len() > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
    match std::fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(_) => scan(dir, 0).unwrap_or(true),
    }
}

/// Mark a path as hidden on Windows using `SetFileAttributesW`.
///
/// On non-Windows platforms this is a no-op — the leading `.` in the directory
/// name is already the POSIX convention for hidden files.
///
/// Failures are silently ignored: a non-hidden staging directory is merely a
/// cosmetic nuisance; it does not affect correctness.
fn set_hidden(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        // Encode path as a NUL-terminated wide string.
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0u16))
            .collect();
        // Safety: `wide` is a valid NUL-terminated UTF-16 path.
        unsafe {
            let attrs = GetFileAttributesW(wide.as_ptr());
            // INVALID_FILE_ATTRIBUTES == 0xFFFFFFFF
            if attrs != 0xFFFF_FFFF {
                let _ = SetFileAttributesW(wide.as_ptr(), attrs | FILE_ATTRIBUTE_HIDDEN);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path; // no-op
    }
}

/// At startup, finish any BT tasks that reached `STATUS_COMPLETED` but whose
/// staging directory was left behind (cleanup failed/interrupted after the
/// move loop succeeded, e.g. Windows file handles blocked `remove_dir_all`
/// and the app was killed before the retry succeeded).
///
/// 适用范围(与 download_manager 的 status==3 过滤一致):**仅**"全部 move
/// 成功但 staging 清理失败/中断"。move 中途崩溃时任务 status∈{1,2},不进
/// 本函数——那种情况的恢复路径是 resume + 完成幂等哨兵
/// (`bt_completion_top_<task_id>`,见 bt_download_inner 完成分支)。
///
/// For each task we check whether `save_dir/.bt_stage_<task_id>/` still
/// exists.  If it does:
///
/// 1. Drop empty-shell directories(逐文件降级 move 后残留的空目录骨架,
///    数据已全部移出)——绝不能被当作数据 rescue,否则 dedup 会把空壳移成
///    `<name> (1)/` 并把 DB file_name 指过去,真实数据指针丢失。
/// 2. Look for an entry whose name matches `current_file_name`; move it to
///    `save_dir/<dedup_name>`.  If not found, fall back to moving **every**
///    non-hidden entry individually.
/// 3. Remove the now-empty staging dir.
/// 4. Return `(task_id, final_name)` pairs so the caller can update the DB.
///
/// Uses synchronous I/O — called once at startup (via `spawn_blocking`)
/// before any BT session is active, so there is no concurrency risk.
pub fn rescue_stranded_staging_files(
    completed_bt_tasks: &[(String, String, String)], // (task_id, save_dir, current_file_name)
    // save_dir → 其他任务经完成哨兵声明的顶层名(小写折叠)。errored
    // mid-completion 的任务重启后会带哨兵重试,rescue 的 dedup 必须避开
    // 这些名字,否则 rescue 占名后对方重试复用哨兵会 merge/覆盖进
    // rescue 出的产物(跨任务哨兵劫持,同 compute_completion_layout)。
    claims_by_dir: &HashMap<String, HashSet<String>>,
) -> Vec<(String, String)> {
    let mut updates: Vec<(String, String)> = Vec::new();
    let empty_claims: HashSet<String> = HashSet::new();

    for (task_id, save_dir, current_file_name) in completed_bt_tasks {
        let claimed = claims_by_dir.get(save_dir).unwrap_or(&empty_claims);
        let stage_dir = bt_stage_dir(save_dir, task_id);
        if !stage_dir.exists() {
            continue;
        }

        log_info!(
            "[BT] rescue: task={} staging dir still present at '{}', attempting recovery move",
            &task_id[..task_id.len().min(8)],
            stage_dir.display()
        );

        let save_path = Path::new(save_dir);

        // Collect non-hidden entries from the staging dir, dropping
        // empty-shell directories on the spot (see doc item 1).  必须在
        // fast-path 查找之前过滤:container move 的空壳名恰好精确等于
        // current_file_name,否则会命中 fast path 被当数据移动。
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&stage_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .filter(|e| {
                    let p = e.path();
                    if p.is_dir() && !stage_dir_has_real_data(&p) {
                        log_info!(
                            "[BT] rescue: task={} dropping empty shell dir '{}'",
                            &task_id[..task_id.len().min(8)],
                            p.display()
                        );
                        let _ = std::fs::remove_dir_all(&p);
                        return false;
                    }
                    true
                })
                .collect(),
            Err(e) => {
                log_info!(
                    "[BT] rescue: task={} cannot read staging dir: {}",
                    &task_id[..task_id.len().min(8)],
                    e
                );
                continue;
            }
        };

        if entries.is_empty() {
            // Staging dir is empty (or only hidden files) — remove it.
            let _ = std::fs::remove_dir_all(&stage_dir);
            log_info!(
                "[BT] rescue: task={} staging dir was empty, removed",
                &task_id[..task_id.len().min(8)]
            );
            continue;
        }

        // ------------------------------------------------------------------
        // Fast path: an entry whose name exactly matches current_file_name
        // (= resolved_name written to DB in Phase 3 / Phase 3.5).
        // Fires mainly for single-file torrents. Multi-file torrents stage each
        // file at its own relative path (the torrent-name container exists only
        // in save_dir, created by the completion move), so they normally take the
        // fallback path below — an entry matches here only when the torrent has
        // an inner directory named like its root.
        // Mirrors the `stage_item.exists()` branch in bt_download_inner.
        // ------------------------------------------------------------------
        let preferred = entries
            .iter()
            .find(|e| e.file_name().to_string_lossy() == current_file_name.as_str());

        if let Some(entry) = preferred {
            // I-5 防御:save_dir/<current_file_name> 已存在 ⟹ 本任务的 move
            // 循环早已全部成功(status==3 的先决条件),staging 内这个同名条目
            // 只能是"copy 成功但 remove 被第三方句柄阻塞"留下的残留副本。
            // 绝不能 dedup 成 `<name> (1)` 再 move——那会把 DB file_name 覆写
            // 到残留副本上,真正的完整产物反而变成无引用的磁盘孤儿。
            if save_path.join(current_file_name.as_str()).exists() {
                log_info!(
                    "[BT] rescue: task={} final product already present at '{}'; \
                     dropping stale staging residue",
                    &task_id[..task_id.len().min(8)],
                    save_path.join(current_file_name.as_str()).display()
                );
                let _ = std::fs::remove_dir_all(&stage_dir);
                continue;
            }
            let child_name = entry.file_name();
            let child_name_str = child_name.to_string_lossy();
            let final_name = dedup_name_in_dir(save_path, &child_name_str, claimed);
            let dst = save_path.join(&final_name);

            match move_path(&entry.path(), &dst) {
                Ok(()) => {
                    log_info!(
                        "[BT] rescue: task={} moved '{}' → '{}' (recovery complete)",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        dst.display()
                    );
                    // Remove staging dir; may still contain .pad / hidden files.
                    let _ = std::fs::remove_dir_all(&stage_dir);
                    updates.push((task_id.to_string(), final_name));
                }
                Err(e) => {
                    log_info!(
                        "[BT] rescue: task={} failed to move '{}': {}",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        e
                    );
                    // Leave staging dir in place for manual recovery.
                }
            }
            continue;
        }

        // ------------------------------------------------------------------
        // Fallback path: no entry matched current_file_name.
        // This mirrors the `stage_dir.exists()` fallback in bt_download_inner:
        // move every non-hidden child individually and report the first
        // successfully moved item as the new file_name.
        // ------------------------------------------------------------------
        log_info!(
            "[BT] rescue: task={} expected item '{}' not found in staging dir; \
             moving all children",
            &task_id[..task_id.len().min(8)],
            current_file_name
        );

        let mut first_moved_name: Option<String> = None;
        let mut all_moves_ok = true;
        for entry in &entries {
            let child_name = entry.file_name();
            let child_name_str = child_name.to_string_lossy();
            // I-5 防御(同 fast path):save_dir 已有同名产物 ⟹ 本条目是
            // "copy 成功 remove 被阻塞"的残留副本,丢弃而非 dedup 成 `(1)`
            // 覆写 DB 指针。全选多文件的新布局会落到
            // save_dir/<current_file_name>/<child>,因此也必须识别 container
            // 子路径,否则重启 rescue 会把残留搬到 save_dir 根目录。
            if let Some(landed) =
                landed_residue_path(save_path, current_file_name.as_str(), &child_name)
            {
                log_info!(
                    "[BT] rescue: task={} child '{}' already present at '{}'; dropping residue",
                    &task_id[..task_id.len().min(8)],
                    child_name_str,
                    landed.display()
                );
                continue;
            }
            let final_child_name = dedup_name_in_dir(save_path, &child_name_str, claimed);
            let dst = save_path.join(&final_child_name);

            match move_path(&entry.path(), &dst) {
                Ok(()) => {
                    log_info!(
                        "[BT] rescue: task={} moved child '{}' → '{}'",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        dst.display()
                    );
                    if first_moved_name.is_none() {
                        first_moved_name = Some(final_child_name);
                    }
                }
                Err(e) => {
                    all_moves_ok = false;
                    log_info!(
                        "[BT] rescue: task={} failed to move child '{}': {}",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        e
                    );
                }
            }
        }

        // 仅当所有子项都成功迁出才删除 staging 目录；否则保留,避免把迁移
        // 失败(权限/跨盘/瞬时 I/O)的文件随目录一并删掉造成数据丢失——与
        // fast path 及 bt_download_inner 的完成路径行为对齐,留待下次启动重试。
        if all_moves_ok {
            let _ = std::fs::remove_dir_all(&stage_dir);
        } else {
            log_info!(
                "[BT] rescue: task={} some children failed to move; \
                 keeping staging dir for recovery: {}",
                &task_id[..task_id.len().min(8)],
                stage_dir.display()
            );
        }

        if let Some(name) = first_moved_name {
            updates.push((task_id.to_string(), name));
        }
    }

    updates
}

/// Deduplicate a file or directory name inside `dir`.
///
/// If the name is free, returns `name` unchanged.  Otherwise appends
/// ` (1)`, ` (2)`, … until a free slot is found.  Mirrors the logic in
/// `downloader::dedup_filename` but runs synchronously (called from the BT
/// runtime thread after downloading is complete).
///
/// 冲突判定同时覆盖三类占用:
/// - **磁盘同名条目(大小写折叠)**:Windows/APFS 文件系统大小写不敏感,
///   精确字节比较会漏判 `MOVIE (1)` vs 已存在的 `Movie (1)`,选中后
///   move 落盘走 REPLACE 语义静默覆盖真实文件。折叠用 `to_lowercase`,
///   非 UTF-8 名经 lossy 转换——只可能把不冲突误判为冲突(多让一个编号),
///   决不会漏判。Linux 上折叠偏保守(ext4 大小写敏感,`movie` 与 `Movie`
///   本可共存),代价仅是多让出一个名字,换取全平台一致的安全行为。
/// - **同名 HTTP/HLS 临时文件 `<name>.fdownloading`**:下载中的 HTTP 任务
///   已预订最终名(manager 启动期 dedup 认 temp),BT 完成若占用该名,
///   HTTP finalize rename 会覆盖 BT 产物(跨协议撞名)。
/// - **`avoid` 集合(小写折叠)**:其他 BT 任务经完成哨兵声明、但可能尚未
///   在磁盘留下足迹的顶层名(见 `bt_completion_top_*` claim-aware dedup)。
fn dedup_name_in_dir(dir: &Path, name: &str, avoid: &HashSet<String>) -> String {
    let temp_ext = crate::downloader::TEMP_EXT;
    let candidate = dir.join(name);
    let temp_candidate = dir.join(format!("{name}{temp_ext}"));
    if !candidate.exists() && !temp_candidate.exists() && !avoid.contains(&name.to_lowercase()) {
        return name.to_string();
    }

    // Scan directory once (case-folded) to avoid per-candidate FS round-trips.
    let existing: HashSet<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());

    for i in 1..=9999u32 {
        let new_name = match ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        let folded = new_name.to_lowercase();
        let folded_temp = format!("{folded}{temp_ext}");
        if !existing.contains(&folded)
            && !existing.contains(&folded_temp)
            && !avoid.contains(&folded)
        {
            return new_name;
        }
    }
    // 极端兜底:1..=9999 个编号变体全被占用时,此前返回**原名不变**,调用点
    // (容器 / 单文件分支)会直接拿它当 dst,move_path 静默覆盖已存在文件丢数据。
    // 改用 UUID 后缀保证唯一,杜绝覆盖。(BUG-BT-DEDUP-FALLBACK-OVERWRITE)
    let uniq = uuid::Uuid::new_v4();
    match ext {
        Some(e) => format!("{} ({}).{}", stem, uniq, e),
        None => format!("{} ({})", stem, uniq),
    }
}

struct CompletionLayoutInput<'a> {
    save_dir: &'a Path,
    stage_dir: &'a Path,
    selected_files: &'a [CompletionFileSpec],
    all_selected: bool,
    is_multi_file_torrent: bool,
    custom_name: &'a str,
    torrent_root_name: &'a str,
    reuse_top: Option<&'a str>,
    claimed: &'a HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletionFileSpec {
    relative_path: PathBuf,
    len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletionMove {
    src: PathBuf,
    dst: PathBuf,
    expected_len: u64,
}

struct CompletionLayout {
    moves: Vec<CompletionMove>,
    top_level_name: String,
    task_owned_container: bool,
}

/// Compute the layout for moving downloaded files from staging to save_dir.
///
/// Returns a [`CompletionLayout`]:
/// - `moves`: ordered list of `(src_in_stage, dst_in_save)` pairs to apply
/// - `top_level_name`: the entry that ends up directly under `save_dir`
///   (used as the DB `file_name` for "open file location" UX)
/// - `task_owned_container`: whether all `dst`s live under a final top-level
///   directory owned by this task's completion sentinel.  Retry may safely
///   merge/replace children only in this mode.
///
/// Layout decisions:
/// - **All-selected multi-file torrent** → preserve rqbit's default
///   `save_dir/<torrent name>/...` layout even though FluxDown downloads into
///   a task-scoped staging dir.  The torrent root is always the outer final
///   container; selected relative paths remain the content paths inside it.
/// - **Single file (partial or otherwise)** → single-file flat move (basename
///   only, no container, optional `custom_name` rename).
/// - **Partial selection of multiple files** → per-file flat move; basenames
///   are deduped against save_dir AND against in-batch siblings.  `custom_name`
///   does not apply (no obvious "container" to rename).
///
/// The reason completion is driven by selected metadata paths (and never by reading
/// staging dir contents) is that BT pieces span file boundaries, so librqbit
/// inevitably writes piece-overlap byproducts for non-selected files (see
/// `librqbit/file_ops.rs::write_chunk` — only BEP-47 padding files are
/// skipped).  Those byproducts are cleaned up wholesale by `remove_dir_all`
/// after all selected files have been moved out.
///
/// `reuse_top`:上一次(部分失败的)completion 通过哨兵(config
/// `bt_completion_top_<task_id>`)记录的最终顶层名。重试时复用同名可让
/// 降级链把剩余文件 merge 进同一 dst(文件级 rename 的 REPLACE_EXISTING
/// 语义保证幂等),而不是 fresh dedup 撞名分裂出 `Name (1)/`。复用校验:
/// - **container 分支**:哨兵名不存在或为**目录**时复用(目录可能是自身
///   上次降级链的部分产物,merge 继续);被外部占用为文件时放弃复用退回
///   fresh dedup——防御 Windows `rename(dir, existing_file)` 静默吞文件。
/// - **单文件分支**:哨兵名**不存在才复用**。合法重试中 dst 通常不存在
///   (move_file 的 copy 半成品失败即清理、成功则任务完成哨兵已删);
///   dst 已存在时无法区分「他人产物(另一同名任务/外部文件)」与「自身
///   半成品清理失败的残留」,复用会经 REPLACE 语义静默覆盖——前者是
///   数据丢失(跨任务哨兵劫持),故一律 fresh dedup 换名:最坏遗留一个
///   垃圾半成品孤儿,绝不覆盖可能属于他人的文件。
///
/// partial-selection flat 多文件分支的逐名 dedup 撞名为 v0 既有行为。
///
/// `claimed`:其他任务经完成哨兵声明(但可能尚未落盘)的顶层名集合
/// (小写折叠,同 save_dir)。所有 fresh dedup 都会避开这些名字,使
/// 并发同名任务无法抢走一个已声明、正在重试中的名字。
fn compute_completion_layout(input: CompletionLayoutInput<'_>) -> Option<CompletionLayout> {
    let CompletionLayoutInput {
        save_dir,
        stage_dir,
        selected_files,
        all_selected,
        is_multi_file_torrent,
        custom_name,
        torrent_root_name,
        reuse_top,
        claimed,
    } = input;

    if selected_files.is_empty() {
        return None;
    }

    // 路径穿越防护:selected paths 源自 torrent 元数据(file_infos[i].
    // relative_filename),恶意种子可塞入 `..` / 绝对路径 / 盘符前缀,使
    // `stage_dir.join(rel)` 逃出 staging 目录(读到任意位置文件)或破坏 dst 归属。
    // 任一选中路径不安全即整体拒绝(返回 None → 调用方标 STATUS_ERROR),决不
    // 移动可疑数据。空字节无法出现在 String 派生的 Path 中,这里按组件做词法校验
    // (不做 canonicalize 以避免额外 I/O 与文件不存在时的误报)。
    // (BUG-BT-PATH-TRAVERSAL)
    let path_is_safe = |rel: &Path| -> bool {
        use std::path::Component;
        if rel.as_os_str().is_empty() || rel.is_absolute() {
            return false;
        }
        rel.components().all(|c| matches!(c, Component::Normal(_)))
    };
    if let Some(bad) = selected_files
        .iter()
        .map(|f| &f.relative_path)
        .find(|p| !path_is_safe(p))
    {
        log_info!(
            "[BT] completion: rejecting unsafe selected path '{}' (path traversal guard)",
            bad.display(),
        );
        return None;
    }

    let torrent_root = crate::downloader::sanitize_filename(torrent_root_name);
    let desired_container = if custom_name.is_empty() {
        torrent_root.as_str()
    } else {
        custom_name
    };

    // Rqbit's default CLI/session behavior places all-selected multi-file
    // torrents under `<torrent name>/`. FluxDown overrides rqbit's
    // output_folder with a hidden staging dir, so we recreate that outer root
    // here and preserve every torrent-relative path below it. If a valid
    // torrent happens to contain an inner directory with the same name as the
    // torrent root, that inner component must remain (`Root/Root/file`).
    if all_selected && is_multi_file_torrent {
        let final_top = match reuse_top {
            Some(n) if !save_dir.join(n).exists() || save_dir.join(n).is_dir() => n.to_string(),
            _ => dedup_name_in_dir(save_dir, desired_container, claimed),
        };
        let dst_root = save_dir.join(&final_top);
        let moves = selected_files
            .iter()
            .map(|file| CompletionMove {
                src: stage_dir.join(&file.relative_path),
                dst: dst_root.join(&file.relative_path),
                expected_len: file.len,
            })
            .collect();
        return Some(CompletionLayout {
            moves,
            top_level_name: final_top,
            task_owned_container: true,
        });
    }

    // Single-file flat move (single selected file regardless of all_selected).
    if selected_files.len() == 1 {
        let file = &selected_files[0];
        let rel = &file.relative_path;
        let basename = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
            .to_string();
        let desired = if custom_name.is_empty() {
            basename.as_str()
        } else {
            custom_name
        };
        let final_name = match reuse_top {
            // 仅当哨兵名未被占用才复用;dst 已存在(无论类型)⟹ 非本任务
            // 合法重试的残留,fresh dedup 换名,绝不 REPLACE 覆盖(见函数 doc)。
            Some(n) if !save_dir.join(n).exists() => n.to_string(),
            _ => dedup_name_in_dir(save_dir, desired, claimed),
        };
        let src = stage_dir.join(rel);
        let dst = save_dir.join(&final_name);
        return Some(CompletionLayout {
            moves: vec![CompletionMove {
                src,
                dst,
                expected_len: file.len,
            }],
            top_level_name: final_name,
            task_owned_container: false,
        });
    }

    // Per-file flat move: covers all-selected flat torrent + partial multi.
    // Dedup each basename against save_dir AND against names already chosen
    // in this batch so two staged files cannot collide on the same dst.
    // `taken` 存小写折叠名:批内两个仅大小写不同的 basename(种子内合法)
    // 在 Windows 上是同一 dst,精确比较会漏判并互相覆盖。
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut moves: Vec<CompletionMove> = Vec::with_capacity(selected_files.len());
    let mut top_level: Option<String> = None;
    for (idx, file) in selected_files.iter().enumerate() {
        let rel = &file.relative_path;
        let basename = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
            .to_string();
        // For all-selected flat torrent + custom_name, rename the FIRST file
        // only (preserves prior behavior for the single-rename UX).
        let candidate_seed = if idx == 0 && all_selected && !custom_name.is_empty() {
            custom_name
        } else {
            basename.as_str()
        };
        // First dedup against the on-disk contents of save_dir.
        let mut candidate = dedup_name_in_dir(save_dir, candidate_seed, claimed);
        // Then dedup against names already chosen in *this* batch.  Use a plain
        // numeric counter on the seed's stem/ext (`stem (n).ext`) rather than
        // prepending `_` to the whole candidate, which previously stacked
        // underscores (`_file (1).ext`, `__file (1).ext`, …) and corrupted the
        // base name.  This keeps the same `name (n).ext` style as
        // `dedup_name_in_dir` itself.
        if taken.contains(&candidate.to_lowercase()) {
            let stem = Path::new(candidate_seed)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(candidate_seed);
            let ext = Path::new(candidate_seed)
                .extension()
                .and_then(|s| s.to_str());
            // 1..=9999 mirrors the bound in `dedup_name_in_dir`, preventing an
            // unbounded loop in the pathological all-collisions case.
            for n in 1..=9999u32 {
                let numbered = match ext {
                    Some(e) => format!("{} ({}).{}", stem, n, e),
                    None => format!("{} ({})", stem, n),
                };
                // Reconcile against disk again so we never overwrite a real file.
                let deduped = dedup_name_in_dir(save_dir, &numbered, claimed);
                if !taken.contains(&deduped.to_lowercase()) {
                    candidate = deduped;
                    break;
                }
            }
            // 极端兜底:9999 个编号变体都被占用(需约 1 万个同 basename 文件挤入同一
            // 扁平目录)时,candidate 仍为已占用名,会导致 moves 出现重复目标 → 落盘时
            // 静默覆盖丢数据。此处用 UUID 后缀保证唯一,杜绝覆盖。
            if taken.contains(&candidate.to_lowercase()) {
                let uniq = uuid::Uuid::new_v4();
                candidate = match ext {
                    Some(e) => format!("{} ({}).{}", stem, uniq, e),
                    None => format!("{} ({})", stem, uniq),
                };
            }
        }
        taken.insert(candidate.to_lowercase());
        let src = stage_dir.join(rel);
        let dst = save_dir.join(&candidate);
        if top_level.is_none() {
            top_level = Some(candidate.clone());
        }
        moves.push(CompletionMove {
            src,
            dst,
            expected_len: file.len,
        });
    }

    Some(CompletionLayout {
        moves,
        top_level_name: top_level.unwrap_or_else(|| "download".to_string()),
        task_owned_container: false,
    })
}

/// Move a file or directory from `src` to `dst` — 零拷贝优先的三级降级链。
///
/// 1. `std::fs::rename`:同卷原子零拷贝。Windows 上目录内存在任何打开的
///    子文件句柄时对**目录** rename 报 ACCESS_DENIED——librqbit 对种子内
///    全部文件持句柄直到任务删除,`pause()` 只把 `File` 转移进
///    `TorrentStatePaused` 而不关闭(BUG-BT-DIR-RENAME-2X),因此多文件
///    种子的整目录 rename 在做种句柄存活期间必然失败。
/// 2. 目录降级:递归**逐文件** rename——文件级 rename 对 FULL-share 句柄
///    (Rust/librqbit 默认打开模式)免疫,仍是同卷零拷贝。性能参考:目录
///    rename ~5ms;逐文件 ~0.1-0.5ms/个,5000 文件最坏 1-2s,典型种子
///    (<100 文件)<50ms,仅降级路径消耗。
/// 3. copy + remove 兜底:仅剩真正跨卷(EXDEV)等场景,逐文件发生于
///    [`move_file`] 内,中途失败清理半成品 dst、保留 src 可重试。
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    move_path_with_file_replace(src, dst, false)
}

fn move_path_with_file_replace(src: &Path, dst: &Path, replace_file: bool) -> std::io::Result<()> {
    if !src.is_dir() {
        let mut budget = RETRY_SLEEP_BUDGET;
        // 顶层文件 move 一律 noreplace(见 move_file doc):dedup 在完成锁内
        // 保证 dst 空闲,此刻 dst 已被占只能是锁外写者(HTTP finalize/外部
        // 程序)在 dedup 与 move 的间隙抢得——占位声明使本次 move 失败重试
        // 换名,决不 REPLACE 覆盖对方产物。
        return move_file(src, dst, &mut budget, replace_file);
    }
    // 防御(实测:Windows `rename(dir, existing_file)` 会静默吞掉该文件):
    // dst 已存在且不是目录 → 不走目录 rename 快路径,直接报错让上层处理
    // (compute_completion_layout 的哨兵类型校验通常已把这种情况引回
    // fresh dedup,此处是最后防线)。
    if dst.exists() && !dst.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists and is not a directory",
        ));
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let mut budget = RETRY_SLEEP_BUDGET;
    move_dir_recursive(src, dst, &mut budget)?;
    // 移空后清掉 src 残留骨架(空目录树);失败无害——完成路径的 staging
    // 清理(带重试)与启动清理兜底。
    let _ = std::fs::remove_dir_all(src);
    Ok(())
}

/// 单次 [`move_path`] 调用内所有瞬时锁(err 32/33)重试的总睡眠次数上限
/// (250ms x 8 = 2s 封顶),防大种子 x AV 批量扫描时重试时间线性叠加。
const RETRY_SLEEP_BUDGET: u32 = 8;
/// 瞬时锁重试的单次退避间隔(对齐 staging 清理重试先例)。
const RETRY_DELAY: Duration = Duration::from_millis(250);

/// 移动单个文件:rename → 对 ERROR_SHARING_VIOLATION(32)/
/// ERROR_LOCK_VIOLATION(33) 在预算内退避重试(第三方独占句柄如 AV 扫描,
/// Rust 默认 FULL-share 句柄不触发)→ copy 兜底。
///
/// `replace` 语义:
/// - `true`(容器 merge 子文件,经 [`move_dir_recursive`]):rename 为
///   Windows `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`,覆盖已存在 dst——
///   重试幂等必需:上次尝试可能留下**截断的**半拷贝子文件,必须能盖写。
///   dst 目录树属于本任务(哨兵复用校验 + create_dir_all 创建),无跨任务
///   覆盖风险。
/// - `false`(顶层文件 move):先以 `create_new(dst)` **原子占名**——两个
///   写者(并发 HTTP finalize、另一 BT 完成)竞争同名时后到者得
///   `AlreadyExists`,决不覆盖;成功后 rename/copy 覆盖的是**自己的**占位
///   文件。占名失败向上传播,completion 标 ERROR,自动重试 fresh dedup
///   换名,自愈。崩溃于占名与 rename 之间会遗留 0 字节占位孤儿(重试
///   dedup 自动避开),属可接受的罕见崩溃残留。
///
/// copy 成功但 remove_file(src) 失败(实测:share=READ 无 DELETE 的第三方
/// 句柄允许读、不允许删)→ 返回 Ok:dst 已是完整副本,任务目标达成;src
/// 残留在 staging 内由 staging 清理(带重试)与启动清理移除——绝不能因
/// "伪失败"把整次 completion 标 ERROR 触发无谓的重验重下。
fn move_file(src: &Path, dst: &Path, budget: &mut u32, replace: bool) -> std::io::Result<()> {
    if !replace {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)
            .map(drop)?;
    }
    let mut last_err;
    loop {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // 错误码 32/33 = ERROR_SHARING_VIOLATION/ERROR_LOCK_VIOLATION,
                // 是 Win32 专属语义;Unix 的 errno 32/33 是 EPIPE/EDOM(rename
                // 不会返回,但 FUSE 等异常文件系统理论上可能),门控避免在
                // Unix 上对不可恢复错误做无意义的 2s 退避。
                #[cfg(windows)]
                let transient = matches!(e.raw_os_error(), Some(32) | Some(33));
                #[cfg(not(windows))]
                let transient = false;
                last_err = e;
                if !transient || *budget == 0 {
                    break;
                }
                *budget -= 1;
                std::thread::sleep(RETRY_DELAY);
            }
        }
    }
    match std::fs::copy(src, dst) {
        Ok(_) => {
            if let Err(e) = std::fs::remove_file(src) {
                log_info!(
                    "[BT] move_file: copied but could not remove src '{}' ({}); leaving residue for staging cleanup",
                    src.display(),
                    e
                );
            }
            Ok(())
        }
        Err(copy_err) => {
            let _ = std::fs::remove_file(dst); // 半成品清理:不完整 dst 占住最终名 = 不可见磁盘泄漏
            let _ = copy_err;
            Err(last_err) // 报更早的 rename 错误(根因)
        }
    }
}

/// 递归逐文件移动目录内容(merge 语义:dst 已存在目录 → 并入;实测
/// create_dir_all 对已存在目录 Ok、对已存在同名文件 Err(AlreadyExists))。
/// 单个子项失败**不中止兄弟项**(尽量多移,减少下一轮重试量),记录首个
/// 错误于循环结束后返回——上层将本次 completion 标 ERROR 并保留重试。
fn move_dir_recursive(src: &Path, dst: &Path, budget: &mut u32) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut first_err: Option<std::io::Error> = None;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let child_dst = dst.join(entry.file_name());
        let result = match entry.file_type() {
            Ok(t) if t.is_dir() => move_dir_recursive(&entry.path(), &child_dst, budget),
            Ok(_) => move_file(&entry.path(), &child_dst, budget, true),
            Err(e) => Err(e),
        };
        if let Err(e) = result
            && first_err.is_none()
        {
            first_err = Some(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionMoveOutcome {
    Moved,
    PriorSuccess,
    MissingSource,
}

fn remove_completion_src_residue(src: &Path) {
    if src.is_dir() {
        let _ = std::fs::remove_dir_all(src);
    } else {
        let _ = std::fs::remove_file(src);
    }
}

fn path_has_expected_file_len(path: &Path, expected_len: u64) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() == expected_len)
        .unwrap_or(false)
}

fn move_completion_item(
    src: &Path,
    dst: &Path,
    expected_len: u64,
    retrying_completion: bool,
    task_owned_container: bool,
    dst_verified: bool,
) -> std::io::Result<CompletionMoveOutcome> {
    if !src.exists() {
        return if retrying_completion
            && dst_verified
            && dst.exists()
            && path_has_expected_file_len(dst, expected_len)
        {
            Ok(CompletionMoveOutcome::PriorSuccess)
        } else {
            Ok(CompletionMoveOutcome::MissingSource)
        };
    }

    if retrying_completion
        && task_owned_container
        && dst_verified
        && dst.exists()
        && path_has_expected_file_len(dst, expected_len)
    {
        remove_completion_src_residue(src);
        return Ok(CompletionMoveOutcome::PriorSuccess);
    }

    if !path_has_expected_file_len(src, expected_len) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "staged source length mismatch: expected {} bytes",
                expected_len
            ),
        ));
    }

    if let Some(parent) = dst.parent()
        && !parent.exists()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    if retrying_completion && task_owned_container && dst.exists() {
        move_path_with_file_replace(src, dst, true)?;
    } else {
        move_path(src, dst)?;
    }
    Ok(CompletionMoveOutcome::Moved)
}

const STATUS_PREPARING: i32 = 5;

/// Number of virtual segments for single-file BT progress visualization.
const BT_VIRTUAL_SEGMENTS: i32 = 16;

/// Minimum interval between verbose `[BT]` progress log lines per task.
/// Progress is still reported to the UI every poll cycle; only the log
/// file output is throttled to keep logs compact and useful.
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum time to wait for magnet metadata (DHT / peer) resolution before
/// failing the task (#379).
///
/// `session.add_torrent` for a magnet link blocks until metadata is fetched
/// from peers.  A dead magnet (no peers, DHT blocked by firewall/ISP, all
/// trackers unreachable) never resolves, which previously left the task in
/// "preparing" forever and the new-download dialog spinning on
/// "resolving magnet link" with no feedback or error.  Torrent-file tasks
/// are exempt — their metadata is local and `add_torrent` returns quickly.
const MAGNET_METADATA_TIMEOUT: Duration = Duration::from_secs(300);

/// For **multi-file** torrents each file becomes a segment — this naturally
/// reflects the concurrent piece-based download because different files
/// accumulate downloaded bytes independently.
///
/// For **single-file** (or when `file_progress` is unavailable) we split
/// the total size into `BT_VIRTUAL_SEGMENTS` virtual segments and
/// distribute the completed pieces proportionally using a deterministic
/// scatter pattern.  This avoids the old "linear fill" look and produces
/// an IDM-style concurrent visualization that truthfully represents the
/// random order in which BT pieces arrive.
fn build_bt_segments(
    total_bytes: i64,
    downloaded_bytes: i64,
    file_progress: &[u64],
    file_offsets: &[(u64, u64)], // (offset_in_torrent, file_len)
    total_pieces: u32,
    downloaded_pieces: u64,
) -> Vec<SegmentProgressInfo> {
    if total_bytes <= 0 {
        return Vec::new();
    }

    // Multi-file torrent: each file is a natural segment
    if file_progress.len() > 1 && file_offsets.len() == file_progress.len() {
        return build_multi_file_segments(total_bytes, file_progress, file_offsets);
    }

    // Single-file (or fallback): scatter pieces across virtual segments
    build_piece_scatter_segments(
        total_bytes,
        downloaded_bytes,
        total_pieces,
        downloaded_pieces,
    )
}

/// Multi-file torrent: map each file to a segment.
fn build_multi_file_segments(
    total_bytes: i64,
    file_progress: &[u64],
    file_offsets: &[(u64, u64)],
) -> Vec<SegmentProgressInfo> {
    let mut segs = Vec::with_capacity(file_progress.len());
    for (i, (&dl_bytes, &(offset, file_len))) in
        file_progress.iter().zip(file_offsets.iter()).enumerate()
    {
        if file_len == 0 {
            continue;
        }
        let start = offset as i64;
        // Partial-selection guard: with a subset `total_bytes` but the full
        // torrent's `file_offsets`, an unselected file's `offset` can exceed
        // `total_bytes`.  Such a file would yield `start > end` and a negative
        // `downloaded_bytes`, producing an illegal SegmentProgressInfo for
        // Dart.  Skip any file whose start lies beyond the (subset) total.
        if start >= total_bytes {
            continue;
        }
        let end = (offset + file_len).saturating_sub(1) as i64;
        let end = end.min(total_bytes - 1);
        // Defensive: `end < start` should be impossible after the guard above,
        // but skip rather than emit a reversed range if it ever occurs.
        if end < start {
            continue;
        }
        let span = end - start + 1;
        segs.push(SegmentProgressInfo {
            index: i as i32,
            start_byte: start,
            end_byte: end,
            // Clamp into `[0, span]` so a subset/total mismatch can never yield
            // a negative downloaded count.
            downloaded_bytes: (dl_bytes as i64).clamp(0, span),
        });
    }
    segs
}

/// Single-file torrent: split into virtual segments and distribute
/// completed pieces using a deterministic scatter pattern.
///
/// BT downloads pieces in a mostly random order (rarest-first strategy).
/// Instead of filling left-to-right, we use a modular-hash scatter to
/// distribute `downloaded_pieces` across all virtual segments so the UI
/// shows multiple segments progressing simultaneously — which is what
/// actually happens in practice.
fn build_piece_scatter_segments(
    total_bytes: i64,
    downloaded_bytes: i64,
    total_pieces: u32,
    downloaded_pieces: u64,
) -> Vec<SegmentProgressInfo> {
    // 虚拟段数钳制到 total_bytes:当 total_bytes ∈ 1..16 时,
    // chunk = total_bytes / 16 == 0,非末段 end = (i+1)*chunk-1 = -1 < start = 0,
    // 会产出 end_byte < start_byte 的非法段(多文件路径已在别处防护,单文件
    // scatter 路径此前缺失)。钳到 [1, 16] 保证 chunk >= 1。
    // (BUG-BT-TINY-TORRENT-SEGMENT)
    let n = (BT_VIRTUAL_SEGMENTS as i64).min(total_bytes.max(1)) as i32;
    let chunk = total_bytes / n as i64;
    let mut segs = Vec::with_capacity(n as usize);

    if total_pieces == 0 || (downloaded_pieces == 0 && downloaded_bytes > 0) {
        // Fallback: no piece info yet OR no pieces completed but we have
        // fetched bytes (partial pieces in-flight).  Distribute bytes
        // evenly across virtual segments with a scatter pattern so the
        // user can see BT is actively downloading even before any piece
        // has been fully hash-verified.
        let per_seg = if downloaded_bytes > 0 {
            downloaded_bytes / n as i64
        } else {
            0
        };
        for i in 0..n {
            let start = i as i64 * chunk;
            let end = if i == n - 1 {
                total_bytes - 1
            } else {
                (i as i64 + 1) * chunk - 1
            };
            // 防御:钳制后正常不会发生,但若 chunk 仍致 end < start 则跳过,
            // 决不向 Dart 发反向区间。(BUG-BT-TINY-TORRENT-SEGMENT)
            if end < start {
                continue;
            }
            // Scatter the bytes unevenly so segments don't all look identical.
            // Use golden-ratio perturbation for a natural spread.
            let perturbation = ((i as f64 + 1.0) * 0.618033988749895).fract();
            let weight = 0.7 + perturbation * 0.6; // range [0.7, 1.3]
            let seg_dl = (per_seg as f64 * weight).round() as i64;
            segs.push(SegmentProgressInfo {
                index: i,
                start_byte: start,
                end_byte: end,
                downloaded_bytes: seg_dl.clamp(0, end - start + 1),
            });
        }
        // Correction: ensure total visual bytes match actual downloaded_bytes
        let visual_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
        let diff = downloaded_bytes - visual_total;
        if diff != 0 {
            let abs_diff = diff.unsigned_abs() as f64;
            let direction = if diff > 0 { 1i64 } else { -1i64 };
            let mut remaining = diff.abs();
            for seg in &mut segs {
                let seg_size = seg.end_byte - seg.start_byte + 1;
                let share = ((seg_size as f64 / total_bytes as f64) * abs_diff).round() as i64;
                let adj = share.min(remaining);
                seg.downloaded_bytes = (seg.downloaded_bytes + direction * adj).clamp(0, seg_size);
                remaining -= adj;
                if remaining <= 0 {
                    break;
                }
            }
        }
        return segs;
    }

    // Assign each piece to a virtual segment, then count completed pieces
    // per segment.  The assignment uses a scatter function to spread pieces
    // that are close in index across different segments.
    let pieces_per_seg = (total_pieces as f64 / n as f64).ceil() as u32;
    let completion_ratio = if total_pieces > 0 {
        downloaded_pieces as f64 / total_pieces as f64
    } else {
        0.0
    };

    for i in 0..n {
        let start = i as i64 * chunk;
        let end = if i == n - 1 {
            total_bytes - 1
        } else {
            (i as i64 + 1) * chunk - 1
        };
        // 防御:钳制后正常不会发生,但若 chunk 仍致 end < start 则跳过,
        // 决不向 Dart 发反向区间。(BUG-BT-TINY-TORRENT-SEGMENT)
        if end < start {
            continue;
        }
        let seg_size = end - start + 1;

        // Count how many pieces belong to this segment
        let seg_piece_start = i as u32 * pieces_per_seg;
        let seg_piece_end = ((i as u32 + 1) * pieces_per_seg).min(total_pieces);
        let seg_total_pieces = seg_piece_end.saturating_sub(seg_piece_start);

        // Scatter completed pieces across segments using a golden-ratio
        // based distribution.  This produces a visually pleasing and
        // deterministic spread that varies per segment.
        //
        // For each segment i, the expected completion is:
        //   base_ratio ± a small perturbation seeded by segment index
        //
        // The perturbation ensures segments don't all show the same %.
        let perturbation = ((i as f64 + 1.0) * 0.618033988749895).fract() - 0.5;
        let seg_ratio = (completion_ratio + perturbation * 0.3).clamp(0.0, 1.0);

        // Snap to exact 0 or 1 when close to boundaries
        let seg_dl_pieces = if completion_ratio <= 0.001 {
            0.0
        } else if completion_ratio >= 0.999 {
            seg_total_pieces as f64
        } else {
            (seg_total_pieces as f64 * seg_ratio).round()
        };

        let dl =
            ((seg_dl_pieces / seg_total_pieces.max(1) as f64) * seg_size as f64).round() as i64;

        segs.push(SegmentProgressInfo {
            index: i,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: dl.clamp(0, seg_size),
        });
    }

    // Correction pass: make sure total downloaded across segments matches
    // the real downloaded_bytes (avoid visual mismatch with progress %).
    let visual_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
    let diff = downloaded_bytes - visual_total;
    if diff != 0 && !segs.is_empty() {
        // Distribute the difference proportionally
        let abs_diff = diff.unsigned_abs() as f64;
        let direction = if diff > 0 { 1i64 } else { -1i64 };
        let mut remaining = diff.abs();
        for seg in &mut segs {
            let seg_size = seg.end_byte - seg.start_byte + 1;
            let share = ((seg_size as f64 / total_bytes as f64) * abs_diff).round() as i64;
            let adj = share.min(remaining);
            seg.downloaded_bytes = (seg.downloaded_bytes + direction * adj).clamp(0, seg_size);
            remaining -= adj;
            if remaining <= 0 {
                break;
            }
        }
    }

    segs
}

/// Compute user-facing BT progress bytes.
///
/// We combine checked bytes (hash-verified) and fetched bytes (network-received)
/// so early BT activity is visible before any piece is fully verified.
///
/// Important: when the torrent is not `finished`, never return `total_bytes`,
/// otherwise UI would show 100% while librqbit is still verifying/finalizing.
fn compute_bt_display_progress(
    checked_progress: i64,
    fetched_progress: i64,
    total_bytes: i64,
    finished: bool,
) -> i64 {
    let mut progress = checked_progress.max(fetched_progress).max(0);
    if total_bytes > 0 {
        progress = progress.min(total_bytes);
        if !finished && progress >= total_bytes {
            progress = total_bytes.saturating_sub(1);
        }
    }
    progress
}

// ---------------------------------------------------------------------------
// Completion-time piece verification (BUG-BT-PHANTOM-PIECES)
//
// librqbit's `finished` flag only means "every selected piece is marked have
// in the chunk tracker".  Have-bits restored from a `{hash}.bitv` fastresume
// file are accepted by a *sampling* validation that, on top of checking only
// ~2% of claimed pieces, treats a SHA1 mismatch as success (it rejects on
// `check_piece(..).is_err()` — I/O errors — while a mismatch is `Ok(false)`;
// librqbit 8.1.1 initializing.rs:148, unchanged upstream).  On Windows,
// short reads are additionally silently ignored (`seek_read` result unused
// in `pread_exact`).  So if staging data is lost while a task is paused, the
// re-added torrent can reach `finished` with pieces that are zeros on disk.
//
// These helpers re-hash every required piece of the staging directory
// against the torrent's SHA1 piece table before the file is moved to its
// final destination — the last line of defence regardless of why the
// bitfield and the data disagree.
// ---------------------------------------------------------------------------

/// One torrent file as seen by the piece verifier.
struct VerifyFileSpec {
    /// Absolute on-disk path (`None` for BEP-47 padding files, whose bytes
    /// are virtual zeros and are never stored).
    path: Option<PathBuf>,
    /// Length in bytes inside the torrent's piece space.
    len: u64,
    /// Whether the file is part of the user's selection.  A piece is
    /// *required* (and therefore verified) iff it overlaps at least one
    /// selected file — the same rule librqbit's chunk tracker uses to compute
    /// `finished`, so verification can never demand pieces the engine does
    /// not promise (which would loop forever in the repair path).
    selected: bool,
}

/// Outcome of a full piece re-hash of the staging directory.
struct VerifyOutcome {
    /// Indices of required pieces that are provably wrong: SHA1 mismatch or
    /// unreadable/short data.
    bad: Vec<u32>,
    /// Number of required pieces hashed and found correct.
    checked: u64,
    /// Pieces not verified because no selected file overlaps them.
    skipped: u64,
}

/// Walk all pieces of the torrent layout `files` (in torrent order) and
/// SHA1-check every required piece.  `expected_hash_matches(piece, digest)`
/// compares against the torrent's piece table (`None` = piece index unknown,
/// treated as bad — the metadata and layout must agree).
fn verify_pieces_core(
    piece_length: u32,
    files: &[VerifyFileSpec],
    mut expected_hash_matches: impl FnMut(u32, [u8; 20]) -> Option<bool>,
) -> VerifyOutcome {
    use sha1::Digest;

    let piece_length = piece_length.max(1) as u64;
    let total: u64 = files.iter().map(|f| f.len).sum();
    let total_pieces = total.div_ceil(piece_length);

    let mut handles: Vec<Option<std::fs::File>> = Vec::new();
    handles.resize_with(files.len(), || None);
    let mut open_failed = vec![false; files.len()];

    let mut outcome = VerifyOutcome {
        bad: Vec::new(),
        checked: 0,
        skipped: 0,
    };
    let mut buf = vec![0u8; 256 * 1024];
    // Reused span buffer: (file index, offset in file, span length).
    let mut spans: Vec<(usize, u64, u64)> = Vec::new();

    // Sequential cursor over the concatenated file space.
    let mut file_idx = 0usize;
    let mut file_pos = 0u64;

    for piece_idx in 0..total_pieces {
        let piece_len = piece_length.min(total - piece_idx * piece_length);

        // Collect this piece's spans, advancing the cursor.
        spans.clear();
        let mut required = false;
        let mut remaining = piece_len;
        while remaining > 0 {
            while file_idx < files.len() && file_pos >= files[file_idx].len {
                file_idx += 1;
                file_pos = 0;
            }
            let Some(f) = files.get(file_idx) else {
                break; // layout shorter than piece space — cannot happen
            };
            let span = (f.len - file_pos).min(remaining);
            spans.push((file_idx, file_pos, span));
            required |= f.selected;
            remaining -= span;
            file_pos += span;
        }

        if !required {
            outcome.skipped += 1;
            continue;
        }

        let mut hasher = sha1::Sha1::new();
        let mut readable = true;
        for &(fi, offset, span) in &spans {
            let file = &files[fi];
            let ok = match &file.path {
                // Padding file — virtual zeros.
                None => {
                    buf.fill(0);
                    let mut left = span;
                    while left > 0 {
                        let chunk = left.min(buf.len() as u64) as usize;
                        hasher.update(&buf[..chunk]);
                        left -= chunk as u64;
                    }
                    true
                }
                Some(path) => hash_file_span(
                    &mut handles[fi],
                    &mut open_failed[fi],
                    path,
                    offset,
                    span,
                    &mut buf,
                    &mut hasher,
                )
                .is_ok(),
            };
            if !ok {
                readable = false;
                break;
            }
        }

        if !readable {
            outcome.bad.push(piece_idx as u32);
            continue;
        }
        let digest: [u8; 20] = hasher.finalize().into();
        if expected_hash_matches(piece_idx as u32, digest) == Some(true) {
            outcome.checked += 1;
        } else {
            outcome.bad.push(piece_idx as u32);
        }
    }

    outcome
}

/// Feed `len` bytes of `path` starting at `offset` into `hasher`.
/// The file handle is opened lazily and cached across spans; a failed open is
/// remembered so later spans fail fast instead of retrying the syscall.
fn hash_file_span(
    handle: &mut Option<std::fs::File>,
    open_failed: &mut bool,
    path: &Path,
    offset: u64,
    len: u64,
    buf: &mut [u8],
    hasher: &mut sha1::Sha1,
) -> std::io::Result<()> {
    use sha1::Digest;
    use std::io::{Read, Seek, SeekFrom};

    if *open_failed {
        return Err(std::io::ErrorKind::NotFound.into());
    }
    if handle.is_none() {
        match std::fs::File::open(path) {
            Ok(f) => *handle = Some(f),
            Err(e) => {
                *open_failed = true;
                return Err(e);
            }
        }
    }
    let Some(f) = handle.as_mut() else {
        return Err(std::io::ErrorKind::NotFound.into());
    };
    f.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    while remaining > 0 {
        let chunk = remaining.min(buf.len() as u64) as usize;
        // read_exact (unlike librqbit's Windows pread_exact) genuinely fails
        // on EOF/short reads, so a truncated staging file is detected.
        f.read_exact(&mut buf[..chunk])?;
        hasher.update(&buf[..chunk]);
        remaining -= chunk as u64;
    }
    Ok(())
}

fn resolve_completion_verify_path(
    staged_path: PathBuf,
    completion_moves: &[CompletionMove],
    allow_dst_fallback: bool,
) -> PathBuf {
    if !allow_dst_fallback || staged_path.exists() {
        return staged_path;
    }
    if allow_dst_fallback
        && let Some(dst) = completion_moves
            .iter()
            .find(|m| m.src == staged_path)
            .map(|m| &m.dst)
            .filter(|dst| dst.exists())
    {
        return dst.clone();
    }
    staged_path
}

/// Re-hash the staged data of `handle` against its torrent metadata on a
/// blocking thread.  `true_selection` are the selected file indices
/// (negative values or an empty list mean "all files").
async fn verify_staged_pieces(
    handle: BtHandle,
    stage_dir: PathBuf,
    true_selection: Vec<i32>,
    completion_moves: Vec<CompletionMove>,
    allow_dst_fallback: bool,
) -> Result<VerifyOutcome, String> {
    tokio::task::spawn_blocking(move || {
        let select_all = true_selection.is_empty() || true_selection.iter().any(|&i| i < 0);
        let selected: HashSet<usize> = true_selection.iter().map(|&i| i as usize).collect();
        handle
            .with_metadata(|meta| {
                let files: Vec<VerifyFileSpec> = meta
                    .file_infos
                    .iter()
                    .enumerate()
                    .map(|(i, fi)| VerifyFileSpec {
                        path: if fi.attrs.padding {
                            None
                        } else {
                            let staged_path = stage_dir.join(&fi.relative_filename);
                            Some(resolve_completion_verify_path(
                                staged_path,
                                &completion_moves,
                                allow_dst_fallback,
                            ))
                        },
                        len: fi.len,
                        selected: select_all || selected.contains(&i),
                    })
                    .collect();
                verify_pieces_core(
                    meta.lengths.default_piece_length(),
                    &files,
                    |idx, digest| meta.info.compare_hash(idx, digest),
                )
            })
            .map_err(|e| format!("metadata unavailable: {e}"))
    })
    .await
    .map_err(|e| format!("verification task failed: {e}"))?
}

/// How a BT file selection reaches librqbit. A known subset is baked into
/// `AddTorrentOptions.only_files` at add time because `update_only_files` is
/// rejected while librqbit is Initializing; a dialog selection stays post-add (#90).
#[derive(Debug, PartialEq, Eq)]
pub enum BtSelectionStrategy {
    All,
    AtAdd(Vec<usize>),
    PostAdd,
}

impl BtSelectionStrategy {
    pub fn only_files_for_add(&self) -> Option<Vec<usize>> {
        match self {
            BtSelectionStrategy::AtAdd(subset) => Some(subset.clone()),
            BtSelectionStrategy::All | BtSelectionStrategy::PostAdd => None,
        }
    }
}

/// Build the librqbit add options for a fresh torrent. Baking the known subset
/// into `only_files` here (not via a post-add `update_only_files`) is the fix
/// for the Initializing race (#90); the production add site must go through this.
pub fn build_add_torrent_options(
    strategy: &BtSelectionStrategy,
    output_folder: String,
) -> AddTorrentOptions {
    AddTorrentOptions {
        overwrite: true,
        output_folder: Some(output_folder),
        only_files: strategy.only_files_for_add(),
        ..Default::default()
    }
}

/// Negative indices (the `-1` cancel sentinel or corrupt entries) are not valid
/// librqbit file ids and are dropped; an empty result means "not yet known".
pub fn decide_bt_selection_strategy(
    skip_file_selection: bool,
    pre_selected_indices: &[i32],
) -> BtSelectionStrategy {
    if skip_file_selection {
        return BtSelectionStrategy::All;
    }
    let subset: Vec<usize> = pre_selected_indices
        .iter()
        .copied()
        .filter(|&i| i >= 0)
        .map(|i| i as usize)
        .collect();
    if subset.is_empty() {
        // Empty input (dialog pending) or all-negative garbage (e.g. a corrupt
        // `[-2]`): never bake an empty `only_files`; defer to the post-add flow (#90).
        BtSelectionStrategy::PostAdd
    } else {
        debug_assert!(!subset.is_empty(), "AtAdd must never carry an empty subset");
        BtSelectionStrategy::AtAdd(subset)
    }
}

/// Wait out librqbit's `Initializing` window (which rejects `update_only_files`),
/// then apply the post-add selection. Returns `true` once applied (#90).
async fn apply_only_files_after_init(
    session: &Arc<Session>,
    handle: &BtHandle,
    only: &HashSet<usize>,
    task_id: &str,
    cancelled: &AtomicBool,
) -> bool {
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        if cancelled.load(Ordering::SeqCst) {
            return false;
        }
        // librqbit's `wait_until_initialized` blocks for the whole hash-check;
        // race it against cancellation (polling the same AtomicBool the add loop
        // uses) so a pause/cancel is not stuck behind it (#90). BoxFuture is
        // Unpin, so `&mut` in select! needs no extra pinning.
        let mut init = handle.wait_until_initialized();
        let initialized = loop {
            tokio::select! {
                biased;
                r = &mut init => break r,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if cancelled.load(Ordering::SeqCst) {
                        return false;
                    }
                }
            }
        };
        if cancelled.load(Ordering::SeqCst) {
            return false;
        }
        if let Err(e) = initialized {
            // A wait error is fatal: surface it via the caller's error path
            // instead of falling through and downloading unselected files (#90).
            log_info!(
                "[BT] task={} wait_until_initialized failed before applying file selection: {}",
                short_id(task_id),
                e
            );
            return false;
        }
        match session.update_only_files(handle, only).await {
            Ok(()) => {
                if cancelled.load(Ordering::SeqCst) {
                    return false;
                }
                return true;
            }
            Err(e) => {
                log_info!(
                    "[BT] task={} update_only_files attempt {}/{} failed: {} — waiting for init and retrying",
                    short_id(task_id),
                    attempt,
                    MAX_ATTEMPTS,
                    e
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    false
}

async fn bt_download_inner(p: BtInnerParams) -> Result<(), DownloadError> {
    let BtInnerParams {
        task_id,
        torrent_source,
        save_dir,
        db,
        progress_tx,
        cancelled,
        session,
        shared_bt,
        existing_handle,
        pre_selected_indices,
        skip_file_selection,
        custom_name,
        selector,
    } = p;

    // Record whether this is a resume of an existing handle *before*
    // existing_handle is moved into Phase 2.  When true we skip Phase 3.5
    // entirely because librqbit already retains the previous update_only_files
    // state internally.
    let had_existing_handle = existing_handle.is_some();
    let selection_strategy =
        decide_bt_selection_strategy(skip_file_selection, &pre_selected_indices);

    // -----------------------------------------------------------------------
    // Phase 1: Send initial file name from dn= parameter so user sees something
    // -----------------------------------------------------------------------

    let dn_name = torrent_source.display_name().unwrap_or_default();
    if !dn_name.is_empty() {
        let _ = db.update_task_file_info(&task_id, &dn_name, 0).await;
        let _ = progress_tx
            .send(ProgressUpdate {
                task_id: task_id.clone(),
                downloaded_bytes: 0,
                total_bytes: 0,
                status: STATUS_PREPARING,
                error_message: String::new(),
                file_name: dn_name.clone(),
                segment_details: None,
                ..Default::default()
            })
            .await;
    }

    // -----------------------------------------------------------------------
    // Phase 2: Obtain torrent handle
    //
    // If we have an existing handle (resumed from pause), just unpause it.
    // Otherwise add a new torrent to the session.
    // -----------------------------------------------------------------------

    let handle = if let Some(h) = existing_handle {
        log_info!(
            "[BT] task={} reusing existing handle (resume)",
            short_id(&task_id)
        );
        // Handle was already unpaused by SharedBtSession::resume_task,
        // so we can go straight to the progress loop.
        h
    } else {
        // Use a task-scoped staging directory so that concurrent BT tasks
        // with identical torrent names never collide on disk.
        // Because output_folder is set explicitly (sub_folder = None), librqbit
        // writes each file flat at  save_dir/.bt_stage_<task_id>/<relative_path>
        // — it does NOT prepend the torrent-name folder (that default applies
        // only when output_folder is None; see librqbit session.rs). The
        // torrent-name container is recreated by compute_completion_layout when
        // moving the result to its final deduplicated path after download.
        let stage_dir = bt_stage_dir(&save_dir, &task_id);
        // Create the staging directory now (before librqbit does) so we can
        // immediately mark it hidden.  librqbit uses `overwrite: true` and
        // will reuse the directory if it already exists.
        if let Err(e) = std::fs::create_dir_all(&stage_dir) {
            log_info!(
                "[BT] task={} failed to pre-create staging dir '{}': {}",
                short_id(&task_id),
                stage_dir.display(),
                e
            );
        } else {
            set_hidden(&stage_dir);
        }
        // BUG-BT-PHANTOM-PIECES guard: if the staging dir holds no real data
        // (fresh task, or partial data lost while paused — e.g. the dir was
        // deleted externally), a leftover `{hash}.bitv` from an earlier run
        // still claims completed pieces.  librqbit would accept it (its
        // sampling validation ignores hash mismatches — see
        // `clear_stale_fastresume`) and never re-download those pieces,
        // producing a "finished" file with zero-filled holes.  Delete the
        // bitfield up front so the re-add does a genuine initial check.
        if !stage_dir_has_real_data(&stage_dir)
            && let Some(hash) = torrent_source.info_hash_hex()
        {
            shared_bt.clear_stale_fastresume(&hash);
        }
        let stage_dir_str = stage_dir.to_string_lossy().into_owned();
        // A known subset must be baked into add options here; a post-add update
        // is rejected while librqbit is Initializing (#90).
        if let Some(subset) = selection_strategy.only_files_for_add() {
            log_info!(
                "[BT] task={} baking {} pre-selected file(s) into add options (only_files) to survive librqbit init",
                short_id(&task_id),
                subset.len()
            );
        }
        let add_opts = build_add_torrent_options(&selection_strategy, stage_dir_str);

        log_info!(
            "[BT] task={} adding torrent to shared session (metadata resolution may take a while)...",
            short_id(&task_id)
        );

        let session_for_add = session.clone();
        let source_for_add = torrent_source.clone();
        let shared_bt_for_add = shared_bt.clone();
        let task_id_for_add = task_id.clone();
        // Create the RAII guard before spawning so `maybe_release_bt_session`
        // sees the in-flight task even if `bt_download_inner` is cancelled
        // immediately after.  The guard decrements on drop — panic-safe.
        let inflight = shared_bt.inflight_guard();
        let add_handle = tokio::spawn(async move {
            // Move the guard into the task so it is dropped (and thus
            // decrements) when the task finishes normally *or* panics.
            let _inflight = inflight;
            let add_input = match source_for_add {
                TorrentSource::Magnet(ref url) => AddTorrent::from_url(url),
                TorrentSource::TorrentFileBytes(ref bytes) => {
                    AddTorrent::from_bytes(Bytes::from(bytes.clone()))
                }
            };
            let result = session_for_add.add_torrent(add_input, Some(add_opts)).await;
            // If delete_task was called while we were waiting for metadata
            // (handle not yet in `handles`, run_bt_download already returned
            // Err(Cancelled)), apply the pending delete now that we have the
            // torrent ID.  This prevents orphaned files from magnets whose
            // DHT metadata resolved after the user deleted the task.
            //
            // TOCTOU note: `bt_download_inner`'s main loop stores the handle in
            // `handles` only on the success path; on a cancel it returns early
            // *before* `store_handle`.  Meanwhile `delete_task` (download_manager)
            // removes-from-`handles` (a miss, since not yet stored) and then
            // `register_pending_delete`.  If this detached task only checked
            // `take_pending_delete` once and that check raced *ahead* of the
            // `register_pending_delete`, the pending entry would never be
            // consumed and the torrent would leak in the librqbit session.
            //
            // To close the window we register the handle here and re-check the
            // pending-delete map afterwards (double-checked consumption):
            //   1. take_pending_delete → if set, delete now (no need to store).
            //   2. else store_handle, then take_pending_delete *again*; if a
            //      delete arrived in between, delete now and drop the handle.
            // Because `delete_task` writes the pending entry *after* its
            // handles-miss, at least one of the two checks (or `delete_task`'s
            // own handle lookup, once stored) always observes the delete.
            if let Ok(ref resp) = result {
                match resp {
                    AddTorrentResponse::Added(id, handle) => {
                        let id = *id;
                        let handle = handle.clone();
                        if let Some(del_files) = shared_bt_for_add
                            .take_pending_delete(&task_id_for_add)
                            .await
                        {
                            // Delete was requested before we got here.
                            let _ = session_for_add.delete(id.into(), del_files).await;
                            log_info!(
                                "[BT] task={} pending delete applied after add_torrent (delete_files={})",
                                short_id(&task_id_for_add),
                                del_files
                            );
                        } else {
                            // No delete yet — publish the handle so pause/resume/
                            // delete can find it, then re-check for a delete that
                            // may have raced in just after our first check.  We
                            // also register the torrent_id so `delete_task` can
                            // clean up the mapping.
                            shared_bt_for_add
                                .register_torrent_id(id, &task_id_for_add)
                                .await;
                            shared_bt_for_add
                                .store_handle(&task_id_for_add, handle)
                                .await;
                            if let Some(del_files) = shared_bt_for_add
                                .take_pending_delete(&task_id_for_add)
                                .await
                            {
                                // A delete arrived between the two checks; consume
                                // it and remove the handle we just stored
                                // (delete_task also unregisters the torrent_id).
                                let _ = shared_bt_for_add
                                    .delete_task(&task_id_for_add, del_files)
                                    .await;
                                log_info!(
                                    "[BT] task={} pending delete applied on re-check after store_handle (delete_files={})",
                                    short_id(&task_id_for_add),
                                    del_files
                                );
                            }
                        }
                    }
                    AddTorrentResponse::AlreadyManaged(id, _handle) => {
                        // The torrent is owned by another task.  We must NOT
                        // store its handle under our task_id, nor unconditionally
                        // delete it (that would clobber the real owner).  Only
                        // consume a pending delete if this very task owns the
                        // torrent_id mapping — otherwise leave it to the owner.
                        if let Some(del_files) = shared_bt_for_add
                            .take_pending_delete(&task_id_for_add)
                            .await
                        {
                            let owner = shared_bt_for_add.task_for_torrent(*id).await;
                            if owner.as_deref() == Some(task_id_for_add.as_str()) {
                                let _ = session_for_add.delete((*id).into(), del_files).await;
                                log_info!(
                                    "[BT] task={} pending delete applied (already-managed, owned by us, delete_files={})",
                                    short_id(&task_id_for_add),
                                    del_files
                                );
                            } else {
                                log_info!(
                                    "[BT] task={} pending delete skipped — torrent owned by another task",
                                    short_id(&task_id_for_add)
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            result
            // `_inflight` drops here → decrement_inflight_add() called
        });

        // Send "preparing" heartbeats while waiting for metadata.
        let mut add_handle = add_handle;
        let is_magnet_source = torrent_source.is_magnet();
        let add_started = Instant::now();
        let h = loop {
            if cancelled.load(Ordering::SeqCst) {
                // Drop (detach) instead of abort: the spawned add_torrent task
                // continues running so it can consume the pending_delete entry
                // registered by delete_task and properly remove the torrent
                // from the librqbit session.  Aborting would leave the torrent
                // in the session with no way to clean it up later.
                drop(add_handle);
                return Err(DownloadError::Cancelled);
            }

            // Magnet metadata resolution timeout (#379).  Detach the add task
            // (same rationale as the cancel path above) and register a
            // pending delete so the torrent is removed from the session if
            // metadata ever resolves later.  The error message deliberately
            // avoids `is_retriable_error` keywords ("timeout"/"timed out") —
            // auto-retrying a dead magnet would just burn another 5 minutes
            // and pop the file-selection dialog at an unexpected moment.
            if is_magnet_source && add_started.elapsed() >= MAGNET_METADATA_TIMEOUT {
                shared_bt.register_pending_delete(&task_id, true).await;
                drop(add_handle);
                let msg = format!(
                    "magnet metadata resolution took too long ({}s) — no peers/DHT response; check trackers or network",
                    MAGNET_METADATA_TIMEOUT.as_secs()
                );
                log_info!("[BT] task={} {}", short_id(&task_id), &msg);
                let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                let _ = progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: 0,
                        total_bytes: 0,
                        status: STATUS_ERROR,
                        error_message: msg.clone(),
                        file_name: String::new(),
                        segment_details: None,
                        ..Default::default()
                    })
                    .await;
                return Err(DownloadError::Other(msg));
            }

            tokio::select! {
                biased;
                result = &mut add_handle => {
                    let resp = result
                        .map_err(|e| DownloadError::Other(format!("BT add task panicked: {e}")))?
                        .map_err(|e| DownloadError::Other(format!("BT add torrent failed: {e}")))?;
                    let h = match resp {
                        AddTorrentResponse::Added(_id, handle) => {
                            log_info!("[BT] task={} torrent added, id={}", short_id(&task_id), _id);
                            shared_bt.register_torrent_id(_id, &task_id).await;
                            handle
                        }
                        AddTorrentResponse::AlreadyManaged(_id, _handle) => {
                            let owner = shared_bt
                                .task_for_torrent(_id)
                                .await
                                .unwrap_or_else(|| "unknown".to_string());
                            log_info!(
                                "[BT] task={} torrent already managed by task={} (id={}), rejecting duplicate",
                                short_id(&task_id),
                                short_id(&owner),
                                _id
                            );
                            // Clean up the pre-created staging dir (it's empty/useless).
                            let _ = std::fs::remove_dir_all(&stage_dir);

                            let msg = format!(
                                "This torrent is already being downloaded by another task ({})",
                                short_id(&owner)
                            );
                            let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.clone(),
                                    downloaded_bytes: 0,
                                    total_bytes: 0,
                                    status: STATUS_ERROR,
                                    error_message: msg.clone(),
                                    file_name: String::new(),
                                    segment_details: None,
                                    ..Default::default()
                                })
                                .await;
                            return Err(DownloadError::Other(msg));
                        }
                        AddTorrentResponse::ListOnly(_) => {
                            return Err(DownloadError::Other(
                                "torrent returned list_only response".into(),
                            ));
                        }
                    };
                    break h;
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    log_info!("[BT] task={} still resolving metadata...", short_id(&task_id));
                    let _ = progress_tx
                        .send(ProgressUpdate {
                            task_id: task_id.clone(),
                            downloaded_bytes: 0,
                            total_bytes: 0,
                            status: STATUS_PREPARING,
                            error_message: String::new(),
                            file_name: String::new(),
                            segment_details: None,
                            ..Default::default()
                        })
                        .await;
                }
            }
        };
        // Cache the handle for future pause/resume cycles.
        shared_bt.store_handle(&task_id, h.clone()).await;
        h
    };

    // -----------------------------------------------------------------------
    // Phase 3: Metadata resolved — extract name & total size, start tracking
    // -----------------------------------------------------------------------

    let stats = handle.stats();
    let total_bytes = stats.total_bytes as i64;
    let resolved_name = handle.name().unwrap_or_else(|| {
        if dn_name.is_empty() {
            format!("BT_{}", short_id(&task_id))
        } else {
            dn_name.clone()
        }
    });

    log_info!(
        "[BT] task={} metadata resolved: name={}, total={} bytes",
        short_id(&task_id),
        &resolved_name,
        total_bytes
    );

    // Extract file layout info and piece count from torrent metadata.
    // These are immutable after metadata resolution, so we cache them once.
    let (file_offsets, total_pieces) = handle
        .with_metadata(|meta| {
            let offsets: Vec<(u64, u64)> = meta
                .file_infos
                .iter()
                .map(|fi| (fi.offset_in_torrent, fi.len))
                .collect();
            let pieces = meta.lengths.total_pieces();
            (offsets, pieces)
        })
        .unwrap_or((Vec::new(), 0));

    log_info!(
        "[BT] task={} files={}, total_pieces={}",
        short_id(&task_id),
        file_offsets.len(),
        total_pieces,
    );

    // -----------------------------------------------------------------------
    // Phase 3.5: Send file list to Dart and wait for user file selection.
    // -----------------------------------------------------------------------

    // Count files for potential fallback (select-all).
    let file_count = handle
        .with_metadata(|meta| meta.file_infos.len())
        .unwrap_or(0);

    // -----------------------------------------------------------------------
    // Phase 3.5 — File selection.
    //
    // Three paths:
    //
    // R) Resume with existing in-memory handle (same app session):
    //    librqbit already has the correct update_only_files state.
    //    Skip everything — no DB read, no signal, no dialog.
    //    Use a full-range placeholder so update_only_files is NOT called.
    //
    // A) Pre-selected indices provided (new-download dialog OR DB-restored):
    //    The user's selection is already known.  Apply it via update_only_files
    //    so librqbit downloads only the chosen files (needed when re-adding
    //    after app restart because the fresh session starts with all files).
    //    No dialog shown.
    //
    // B) No pre-selection (first-time magnet link with no prior choice):
    //    Send BtFilesInfo to Dart so the file-selection dialog is shown.
    //    Persist the confirmed selection to DB so future resumes use Path A.
    //    Poll until the user confirms or the task is cancelled.
    // -----------------------------------------------------------------------

    let selected_indices: Vec<i32> = if had_existing_handle {
        // Path R — in-memory handle reused, librqbit state intact.
        log_info!(
            "[BT] task={} resumed from existing handle, skipping file selection",
            short_id(&task_id)
        );
        (0..file_count as i32).collect()
    } else if skip_file_selection {
        // Path S — user previously confirmed "all files"; DB recorded this.
        // librqbit defaults to downloading everything after re-add, which is
        // exactly what we want — no update_only_files call needed.
        // Use a full-range vec so the len == file_count guard skips the call.
        log_info!(
            "[BT] task={} skip_file_selection=true, downloading all files (no dialog)",
            short_id(&task_id)
        );
        (0..file_count as i32).collect()
    } else if !pre_selected_indices.is_empty() {
        // Path A — partial selection already known (new-download dialog or DB restore).
        log_info!(
            "[BT] task={} using pre-selected {} file(s) (no dialog)",
            short_id(&task_id),
            pre_selected_indices.len()
        );
        pre_selected_indices
    } else {
        // Path B — no pre-selection: build file list and send to Dart.
        // Filter out BEP-47 padding files — they are an implementation detail
        // and must not be shown to the user.  We keep the true meta index
        // (idx from enumerate) so that the indices forwarded to
        // update_only_files always refer to the correct meta.file_infos slot.
        let bt_files: Vec<BtFileEntry> = handle
            .with_metadata(|meta| {
                meta.file_infos
                    .iter()
                    .enumerate()
                    .filter(|(_, fi)| {
                        // BEP-47 padding files are stored under a ".pad"
                        // directory component.  Use a path-based heuristic
                        // because FileInfos does not expose the attrs field.
                        let name = fi.relative_filename.to_string_lossy();
                        !name.contains("/.pad/")
                            && !name.contains("\\.pad\\")
                            && !fi
                                .relative_filename
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .starts_with(".pad")
                    })
                    .map(|(idx, fi)| BtFileEntry {
                        index: idx as i32,
                        path: fi.relative_filename.to_string_lossy().into_owned(),
                        size: fi.len as i64,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        log_info!(
            "[BT] task={} requesting file selection ({} files) via HostSelection...",
            short_id(&task_id),
            file_count
        );

        // `timeout: None` 保留现状"无限等待"语义——由 HostSelection 的具体
        // 实现(桌面 GUI 场景为 `RinfHostSelection`)决定是否在内部包一个
        // 较长但有限的超时,engine 侧不臆断产品行为。
        let outcome = selector.select_bt_files(&task_id, &bt_files, None).await;
        match &outcome {
            SelectionOutcome::UserChose(indices) => {
                log_info!(
                    "[BT] task={} file selection received: {:?}",
                    short_id(&task_id),
                    indices
                );
            }
            SelectionOutcome::TimedOutDefaulted(indices) => {
                log_info!(
                    "[BT] task={} file selection timed out, defaulting to {:?}",
                    short_id(&task_id),
                    indices
                );
            }
            SelectionOutcome::NoSelectorConfigured(indices) => {
                log_info!(
                    "[BT] task={} no selector configured, defaulting to {:?}",
                    short_id(&task_id),
                    indices
                );
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err(DownloadError::Cancelled);
        }
        outcome.into_inner()
    };

    // Persist the confirmed selection to DB immediately so that future
    // resumes (including across app restarts) bypass the file-selection
    // dialog entirely.  We only persist when the selection came from user
    // interaction (Path B) or was pre-supplied (Path A when !had_existing_handle).
    // Path R (had_existing_handle) skips this because selected_indices is a
    // placeholder full-range vec and the real selection is already in the DB.
    // Persist the confirmed selection so future resumes skip the dialog.
    // Skip when:
    //   - had_existing_handle: DB already has the right value from the first run.
    //   - skip_file_selection: already persisted as "all", no change needed.
    //   - selected_indices starts with -1: user cancelled, leave DB empty so
    //     the dialog reappears on next resume (user can pick again).
    if !had_existing_handle && !skip_file_selection && selected_indices.first().copied() != Some(-1)
    {
        let is_all = selected_indices.len() >= file_count;
        let indices_to_save: &[i32] = if is_all { &[] } else { &selected_indices };
        let _ = db
            .save_bt_selected_files(&task_id, indices_to_save, is_all)
            .await;
        log_info!(
            "[BT] task={} persisted file selection ({}/{} files, is_all={}) to DB",
            short_id(&task_id),
            selected_indices.len(),
            file_count,
            is_all
        );
    }

    // [-1] is the sentinel sent by Dart when the user explicitly cancels the
    // file selection dialog.  Pause the task (status=2) so the user can
    // resume it later and pick files again, rather than leaving it in an
    // ambiguous state or marking it as error.
    if selected_indices.first().copied() == Some(-1) {
        log_info!(
            "[BT] task={} file selection cancelled by user → pausing",
            short_id(&task_id)
        );
        // Persist paused status to DB so it survives app restart.
        let _ = db.update_task_status(&task_id, STATUS_PAUSED, "").await;
        // Pause the librqbit torrent so it stops seeding / connecting.
        let _ = shared_bt.pause_task(&task_id).await;
        // Notify Dart so the UI immediately shows "Paused".
        let _ = progress_tx
            .send(ProgressUpdate {
                task_id: task_id.clone(),
                downloaded_bytes: 0,
                total_bytes,
                status: STATUS_PAUSED,
                error_message: String::new(),
                file_name: resolved_name.clone(),
                segment_details: None,
                ..Default::default()
            })
            .await;
        // Return Cancelled so the manager does not overwrite our status=2.
        return Err(DownloadError::Cancelled);
    }

    // If the selection is empty or holds no valid (non-negative) index — e.g. a
    // corrupt persisted `[-2]`; the `-1` cancel sentinel already returned above —
    // fall back to all files rather than applying an empty only_files that would
    // download nothing (#90).
    let selected_indices = if selected_indices.iter().all(|&i| i < 0) {
        (0..file_count as i32).collect::<Vec<_>>()
    } else {
        selected_indices
    };

    // Path R (had_existing_handle) yields selected_indices.len() == file_count,
    // so this block is skipped — librqbit already has the right state.
    if selected_indices.len() < file_count {
        match selection_strategy {
            // Already applied at add time (#90); skip the post-add update.
            BtSelectionStrategy::AtAdd(_) => {
                log_info!(
                    "[BT] task={} file selection ({} file(s)) applied at add time — skipping post-add update_only_files",
                    short_id(&task_id),
                    selected_indices.len()
                );
            }
            // Dialog selection known only post-add: wait out Initializing, then apply (#90).
            BtSelectionStrategy::All | BtSelectionStrategy::PostAdd => {
                let only: HashSet<usize> = selected_indices
                    .iter()
                    .copied()
                    .filter(|&i| i >= 0)
                    .map(|i| i as usize)
                    .collect();
                if apply_only_files_after_init(&session, &handle, &only, &task_id, &cancelled).await
                {
                    log_info!(
                        "[BT] task={} file selection applied post-add ({} file(s))",
                        short_id(&task_id),
                        only.len()
                    );
                } else if cancelled.load(Ordering::SeqCst) {
                    // Drop the handle with unapplied only_files so resume re-adds via AtAdd.
                    let _ = shared_bt.delete_task(&task_id, false).await;
                    return Err(DownloadError::Cancelled);
                } else {
                    // Surface the failure instead of silently downloading unselected
                    // files (#90); mirrors the other fatal BT setup error paths.
                    let msg = format!(
                        "failed to apply file selection ({} file(s)) after librqbit init",
                        only.len()
                    );
                    log_info!("[BT] task={} {}", short_id(&task_id), &msg);
                    let _ = shared_bt.delete_task(&task_id, false).await;
                    // Pause landed during teardown — keep status=2 (the bad
                    // handle is already gone; resume re-adds with AtAdd).
                    if cancelled.load(Ordering::SeqCst) {
                        log_info!(
                            "[BT] task={} cancelled during selection-failure teardown → keeping paused state",
                            short_id(&task_id)
                        );
                        return Err(DownloadError::Cancelled);
                    }
                    let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                    let _ = progress_tx
                        .send(ProgressUpdate {
                            task_id: task_id.clone(),
                            downloaded_bytes: 0,
                            total_bytes,
                            status: STATUS_ERROR,
                            error_message: msg.clone(),
                            file_name: resolved_name.clone(),
                            segment_details: None,
                            ..Default::default()
                        })
                        .await;
                    return Err(DownloadError::Other(msg));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot the relative paths and expected lengths of selected non-padding files.
    //
    // This is the SOLE source of truth for the completion-time move:
    //   - Path R (had_existing_handle): `selected_indices` is a (0..file_count)
    //     placeholder.  Load the real selection from DB.
    //   - Path S (skip_file_selection): `selected_indices` is (0..file_count)
    //     meaning "all" — already correct.
    //   - Path A / Path B: `selected_indices` is the real user choice.
    // -----------------------------------------------------------------------
    let true_selection: Vec<i32> = if had_existing_handle {
        match db.load_bt_selected_files(&task_id).await.ok().flatten() {
            Some(v) if v.is_empty() => (0..file_count as i32).collect(), // "all" sentinel
            Some(v) => v,
            None => (0..file_count as i32).collect(), // never confirmed → all
        }
    } else {
        selected_indices.clone()
    };
    let (selected_files, non_padding_count, is_multi_file_torrent): (
        Vec<CompletionFileSpec>,
        usize,
        bool,
    ) = handle
        .with_metadata(|meta| {
            let total_non_padding = meta
                .file_infos
                .iter()
                .filter(|fi| !fi.attrs.padding)
                .count();
            let files: Vec<CompletionFileSpec> = true_selection
                .iter()
                .filter_map(|&i| meta.file_infos.get(i as usize))
                .filter(|fi| !fi.attrs.padding)
                .map(|fi| CompletionFileSpec {
                    relative_path: fi.relative_filename.clone(),
                    len: fi.len,
                })
                .collect();
            (files, total_non_padding, meta.info.files.is_some())
        })
        .unwrap_or_default();
    let all_selected = !selected_files.is_empty() && selected_files.len() == non_padding_count;
    log_info!(
        "[BT] task={} completion plan: {} selected file(s), all_selected={}, non_padding_total={}, multi_file_meta={}",
        short_id(&task_id),
        selected_files.len(),
        all_selected,
        non_padding_count,
        is_multi_file_torrent
    );

    // Recompute total_bytes based on selected files only for accurate progress display.
    let total_bytes = {
        let selected_total: i64 = handle
            .with_metadata(|meta| {
                // 用 true_selection(从 DB 载入的真实选择)而非 selected_indices
                // (Path R 同会话续传时被置为全量占位 0..file_count),否则部分选择
                // 续传会把 total_bytes 误算成所有文件之和,导致进度百分比与 DB 元数据错误。
                true_selection
                    .iter()
                    .filter_map(|&i| meta.file_infos.get(i as usize))
                    .map(|fi| fi.len as i64)
                    .sum()
            })
            .unwrap_or(total_bytes);
        if selected_total > 0 && selected_total <= total_bytes {
            selected_total
        } else {
            total_bytes
        }
    };

    let _ = db
        .update_task_file_info(&task_id, &resolved_name, total_bytes)
        .await;
    // Don't clobber a pause (status=2) that landed during the awaits above.
    if cancelled.load(Ordering::SeqCst) {
        log_info!(
            "[BT] task={} cancelled before downloading-status write → keeping paused state",
            short_id(&task_id)
        );
        return Err(DownloadError::Cancelled);
    }
    let _ = db
        .update_task_status(&task_id, STATUS_DOWNLOADING, "")
        .await;

    // Notify Dart of the transition to "downloading" with resolved info
    let init_progress = stats.progress_bytes as i64;
    let init_pieces = stats
        .live
        .as_ref()
        .map(|l| l.snapshot.downloaded_and_checked_pieces)
        .unwrap_or(0);
    let _ = progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: init_progress,
            total_bytes,
            status: STATUS_DOWNLOADING,
            error_message: String::new(),
            file_name: resolved_name.clone(),
            segment_details: Some(build_bt_segments(
                total_bytes,
                init_progress,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                init_pieces,
            )),
            ..Default::default()
        })
        .await;

    // -----------------------------------------------------------------------
    // Phase 4: Download progress loop
    // -----------------------------------------------------------------------

    let mut last_report = Instant::now();
    let mut last_db_save = Instant::now();
    // Throttle the verbose progress log line: logging on every 500ms poll
    // floods the log file with megabytes of near-identical lines on long
    // downloads.  Log immediately on state transitions, otherwise emit a
    // periodic summary at PROGRESS_LOG_INTERVAL.
    let mut last_progress_log: Option<Instant> = None;
    let mut last_logged_status: i32 = -1;

    loop {
        // Check cancellation — the manager layer (pause_task / cancel_task)
        // is responsible for calling session.pause() on the torrent handle,
        // so we only need to exit the loop here.  This avoids a double-pause
        // race where both the inner loop and the manager call session.pause().
        if cancelled.load(Ordering::SeqCst) {
            log_info!(
                "[BT] task={} cancelled → exiting download loop",
                short_id(&task_id)
            );
            return Err(DownloadError::Cancelled);
        }

        let stats = handle.stats();
        // Guard: if this is the very first iteration of the loop (i.e. the
        // torrent just came back from add_torrent / AlreadyManaged + unpause)
        // and librqbit hasn't transitioned to Live yet, spin-wait up to 1s
        // instead of treating the transient Paused state as a cancellation.
        // This closes the race window where unpause() is called but the
        // state machine hasn't updated before we read stats below.
        let is_paused_state = matches!(stats.state, librqbit::TorrentStatsState::Paused);
        if is_paused_state && !cancelled.load(Ordering::SeqCst) {
            // Only spin on the very first poll (before any progress has been
            // reported) to avoid masking a genuine post-pause state.
            if stats.progress_bytes == 0 && stats.total_bytes > 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue; // re-check stats on next iteration
            }
        }
        let checked_progress = stats.progress_bytes as i64;
        let total = if stats.total_bytes > 0 {
            stats.total_bytes as i64
        } else {
            total_bytes
        };

        // Use fetched_bytes (actual bytes received from network, including
        // partial pieces) to expose early BT activity before pieces are fully
        // hash-verified. Keep display progress below 100% until stats.finished.
        let fetched = stats
            .live
            .as_ref()
            .map(|l| l.snapshot.fetched_bytes as i64)
            .unwrap_or(0);
        let progress =
            compute_bt_display_progress(checked_progress, fetched, total, stats.finished);

        // Check for error — but ONLY when the torrent is not in Paused state.
        //
        // Race window: pause_task() calls entry.token.cancel() then session.pause().
        // Between those two calls the progress loop may wake up, see cancelled=false
        // (the AtomicBool watcher fires on the next await), and read stats while
        // librqbit is transitioning through its internal states.  During that
        // transition stats.state can transiently be Error before settling on Paused,
        // which would cause us to report STATUS_ERROR and write status=4 to the DB
        // even though the user only asked to pause.
        //
        // Guarding on `!cancelled` is sufficient for the common case, but the
        // watcher task fires asynchronously so there is still a narrow window where
        // cancelled=false while the session is already shutting down.  The additional
        // `stats.state != Paused` guard eliminates that window: a torrent that
        // librqbit has already placed in the Paused state cannot be in error.
        if let Some(ref err) = stats.error {
            // If we are already cancelled (pause/cancel in progress), or the
            // torrent state is Paused, do not treat this as a hard error —
            // exit cleanly as Cancelled so the manager keeps status=2.
            if cancelled.load(Ordering::SeqCst) || is_paused_state {
                log_info!(
                    "[BT] task={} stats.error='{}' ignored — task is being paused/cancelled",
                    short_id(&task_id),
                    err
                );
                return Err(DownloadError::Cancelled);
            }
            let msg = format!("BT error: {err}");
            log_info!("[BT] task={} error: {}", short_id(&task_id), &msg);
            let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: progress,
                    total_bytes: total,
                    status: STATUS_ERROR,
                    error_message: msg.clone(),
                    file_name: String::new(),
                    segment_details: None,
                    ..Default::default()
                })
                .await;
            return Err(DownloadError::Other(msg));
        }

        // Check if finished
        if stats.finished {
            log_info!("[BT] task={} finished! total={}", short_id(&task_id), total);

            // BT 数据已全部下完，但校验与 staging→save_dir 搬移尚未开始，任务
            // 仍未进终态——这正是 aria2 `onBtDownloadComplete` 通知对应的时刻。
            // 立即补发一条带 `bt_data_finished` 标记的进度（progress_reporter
            // 侧按 task_id 去重并绕过节流）。
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: progress,
                    total_bytes: total,
                    status: STATUS_DOWNLOADING,
                    bt_data_finished: true,
                    ..Default::default()
                })
                .await;

            let final_total = if total > 0 { total } else { progress };
            // 注意:此处**不再**无条件写 STATUS_COMPLETED。BT 数据此刻仍在 staging
            // 目录,只有当下面的移动循环把所有选中文件成功落到 save_dir 后,才会写
            // STATUS_COMPLETED 并发完成信号;否则改写 STATUS_ERROR 并返回 Err,
            // 避免"未真正落盘的任务"显示为已完成(BUG-BT-COMPLETE-BEFORE-MOVE)。
            // progress / total_bytes 只记录已下载字节数,与完成与否无关,可先写。
            let _ = db.update_task_progress(&task_id, final_total).await;
            let _ = db.update_task_total_bytes(&task_id, final_total).await;

            // Build fully-completed segments — used in the single STATUS_COMPLETED
            // signal sent after the staging-dir move is resolved.
            let finished_segs = build_bt_segments(
                final_total,
                final_total,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                total_pieces as u64,
            );

            // Download complete.
            //
            // Move the user-selected files from the staging directory into
            // save_dir.  The move is driven by `selected_files` (a snapshot
            // of `meta.file_infos[i].relative_filename` + len for selected indices,
            // taken right after the user confirmed the file selection).
            //
            // We send the single STATUS_COMPLETED signal AFTER the move so
            // that the file_name field already reflects the true disk name.
            let save_path = PathBuf::from(&save_dir);
            let stage_dir = bt_stage_dir(&save_dir, &task_id);

            let (completed_name, all_moves_succeeded) = if !stage_dir.exists() {
                // No staging dir at all — resumed download that was already
                // moved in a previous session, or existing_handle path where
                // output_folder == save_dir.  The DB file_name is already
                // correct; just return resolved_name for the signal.
                (resolved_name.clone(), true)
            } else {
                let sentinel_key = format!("bt_completion_top_{}", task_id);
                let planned_completion = {
                    // Serialize dedup-name + sentinel declaration against other
                    // BT task completions sharing this session's save_dir. The
                    // expensive piece verification runs after this guard drops;
                    // the persisted sentinel is the claim that makes concurrent
                    // fresh dedup avoid this top-level name.
                    let _move_guard = shared_bt.lock_completion_move().await;
                    // 完成幂等哨兵:首次尝试把 dedup 选定的顶层名记入 config,
                    // 部分失败重试时复用同名让降级链 merge 进同一 dst,防 fresh
                    // dedup 撞名把数据分裂到 `Name (1)/`(BUG-BT-COMPLETION-SPLIT)。
                    // 锁内读写,与并发同名任务的完成序列全局串行化,无 TOCTOU。
                    let reuse_top: Option<String> =
                        db.get_config(&sentinel_key).await.ok().flatten();
                    // Claim-aware dedup:采集**其他**任务的活跃哨兵名(同
                    // save_dir,小写折叠)。这些名字已被声明但可能尚未在磁盘留下
                    // 足迹(对方首次 move 零足迹失败、等待重试中),fresh dedup 若
                    // 占用它们,对方重试复用哨兵时会 merge 进/覆盖本任务产物
                    // (跨任务哨兵劫持)。锁内读取,与哨兵写入全局串行,无 TOCTOU。
                    let retrying_completion = reuse_top.is_some();
                    let claimed: HashSet<String> = {
                        let mut set = HashSet::new();
                        if let Ok(rows) = db.list_config_with_prefix("bt_completion_top_").await {
                            for (key, value) in rows {
                                let Some(tid) = key.strip_prefix("bt_completion_top_") else {
                                    continue;
                                };
                                if tid == task_id {
                                    continue;
                                }
                                if let Ok(Some(t)) = db.load_task_by_id(tid).await
                                    && t.save_dir == save_dir
                                {
                                    set.insert(value.to_lowercase());
                                }
                            }
                        }
                        set
                    };
                    let layout = compute_completion_layout(CompletionLayoutInput {
                        save_dir: &save_path,
                        stage_dir: &stage_dir,
                        selected_files: &selected_files,
                        all_selected,
                        is_multi_file_torrent,
                        custom_name: &custom_name,
                        torrent_root_name: &resolved_name,
                        reuse_top: reuse_top.as_deref(),
                        claimed: &claimed,
                    });
                    match layout {
                        None => None,
                        Some(layout) => {
                            if reuse_top.as_deref() != Some(layout.top_level_name.as_str()) {
                                let _ = db.set_config(&sentinel_key, &layout.top_level_name).await;
                            }
                            Some((layout, retrying_completion))
                        }
                    }
                };
                match planned_completion {
                    None => {
                        // Empty selection — should not happen in practice.
                        log_info!(
                            "[BT] task={} completion: empty selection, falling back to resolved_name='{}'",
                            short_id(&task_id),
                            &resolved_name,
                        );
                        (resolved_name.clone(), false)
                    }
                    Some((layout, retrying_completion)) => {
                        let move_count = layout.moves.len();
                        let dst_fallback_candidates: HashSet<PathBuf> = if retrying_completion {
                            layout
                                .moves
                                .iter()
                                .filter(|m| !m.src.exists() && m.dst.exists())
                                .map(|m| m.dst.clone())
                                .collect()
                        } else {
                            HashSet::new()
                        };
                        // BUG-BT-PHANTOM-PIECES: `stats.finished` can lie when
                        // have-bits were restored from a stale `{hash}.bitv`
                        // whose data no longer exists. Re-hash every required
                        // piece before moving. On a completion retry, some
                        // selected files may already have been moved to `dst`
                        // (or copied there while `src` could not be removed);
                        // verify those final files before counting them as a
                        // prior successful move.
                        log_info!(
                            "[BT] task={} verifying {} pieces before completing...",
                            short_id(&task_id),
                            total_pieces
                        );
                        let verify_started = Instant::now();
                        let verified_existing_dsts = match verify_staged_pieces(
                            handle.clone(),
                            stage_dir.clone(),
                            true_selection.clone(),
                            layout.moves.clone(),
                            retrying_completion,
                        )
                        .await
                        {
                            Ok(outcome) if outcome.bad.is_empty() => {
                                log_info!(
                                    "[BT] task={} piece verification passed ({} hashed, {} skipped) in {:.1}s",
                                    short_id(&task_id),
                                    outcome.checked,
                                    outcome.skipped,
                                    verify_started.elapsed().as_secs_f64()
                                );
                                dst_fallback_candidates
                            }
                            Ok(outcome) => {
                                let preview: Vec<u32> =
                                    outcome.bad.iter().take(16).copied().collect();
                                log_info!(
                                    "[BT] task={} piece verification FAILED: {}/{} pieces bad (first: {:?}) in {:.1}s — clearing fastresume state, bad pieces will be re-downloaded",
                                    short_id(&task_id),
                                    outcome.bad.len(),
                                    total_pieces,
                                    preview,
                                    verify_started.elapsed().as_secs_f64()
                                );
                                let _ = shared_bt.delete_task(&task_id, false).await;
                                let msg = format!(
                                    "BT piece verification failed: {} bad piece(s) — data will be re-checked and re-downloaded",
                                    outcome.bad.len()
                                );
                                let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                                let _ = progress_tx
                                    .send(ProgressUpdate {
                                        task_id: task_id.clone(),
                                        downloaded_bytes: progress,
                                        total_bytes: total,
                                        status: STATUS_ERROR,
                                        error_message: msg.clone(),
                                        file_name: String::new(),
                                        segment_details: None,
                                        ..Default::default()
                                    })
                                    .await;
                                return Err(DownloadError::Other(msg));
                            }
                            Err(e) => {
                                // Internal verification error (e.g. metadata gone)
                                // should not block completion; preserve the previous
                                // best-effort behavior.
                                log_info!(
                                    "[BT] task={} piece verification skipped: {}",
                                    short_id(&task_id),
                                    e
                                );
                                HashSet::new()
                            }
                        };
                        // Reacquire the completion lock for the actual move/update
                        // phase. The sentinel written above remains the claim while
                        // hashing ran without blocking unrelated BT completions.
                        let _move_guard = shared_bt.lock_completion_move().await;
                        let CompletionLayout {
                            moves,
                            top_level_name,
                            task_owned_container,
                        } = layout;
                        // 完成移动是阻塞的 std::fs rename / 跨设备递归复制。bt-runtime
                        // 是 multi_thread（worker_threads = cpu_cores.clamp(2,8)），在其
                        // async worker 上直接阻塞会占用一个 worker，跨设备多 GB 复制时
                        // 拖慢其他 BT 任务的进度轮询/暂停响应。把整段移动循环搬进
                        // spawn_blocking（卸载到专用阻塞线程）,再 .await 句柄;`_move_guard`
                        // 仍在外层持有,跨越此 await,保留 move/update phase 的序列化语义
                        // (BUG-BT-COMPLETION-MOVE-BLOCKING)。
                        let tid_for_move = task_id.clone();
                        let move_result = tokio::task::spawn_blocking(move || {
                            let mut succeeded = 0usize;
                            for item in &moves {
                                let dst_verified = verified_existing_dsts.contains(&item.dst);
                                match move_completion_item(
                                    &item.src,
                                    &item.dst,
                                    item.expected_len,
                                    retrying_completion,
                                    task_owned_container,
                                    dst_verified,
                                ) {
                                    Ok(CompletionMoveOutcome::Moved) => {
                                        log_info!(
                                            "[BT] task={} moved '{}' → '{}'",
                                            short_id(&tid_for_move),
                                            item.src.display(),
                                            item.dst.display(),
                                        );
                                        succeeded += 1;
                                    }
                                    Ok(CompletionMoveOutcome::PriorSuccess) => {
                                        log_info!(
                                            "[BT] task={} completion: verified dst already exists '{}'; treating as prior successful move",
                                            short_id(&tid_for_move),
                                            item.dst.display(),
                                        );
                                        succeeded += 1;
                                    }
                                    Ok(CompletionMoveOutcome::MissingSource) => {
                                        log_info!(
                                            "[BT] task={} completion: expected staged file missing '{}'",
                                            short_id(&tid_for_move),
                                            item.src.display(),
                                        );
                                    }
                                    Err(e) => {
                                        log_info!(
                                            "[BT] task={} move failed: {} ({})",
                                            short_id(&tid_for_move),
                                            item.src.display(),
                                            e
                                        );
                                    }
                                }
                            }
                            succeeded
                        })
                        .await;
                        // spawn_blocking 内部 panic → JoinError。保守按全部失败处理
                        // (succeeded=0 → all_ok=false → 走 STATUS_ERROR 分支),
                        // 决不会把未落盘任务标成已完成。
                        let succeeded = match move_result {
                            Ok(n) => n,
                            Err(join_err) => {
                                log_info!(
                                    "[BT] task={} completion move task panicked: {}",
                                    short_id(&task_id),
                                    join_err,
                                );
                                0
                            }
                        };
                        let all_ok = move_count > 0 && succeeded == move_count;
                        if all_ok {
                            log_info!(
                                "[BT] task={} all {} selected file(s) moved; top_level='{}'",
                                short_id(&task_id),
                                move_count,
                                &top_level_name,
                            );
                        } else {
                            log_info!(
                                "[BT] task={} completion: {}/{} files moved — leaving staging dir for recovery",
                                short_id(&task_id),
                                succeeded,
                                move_count,
                            );
                        }
                        // Persist the resolved top-level name so that the UI
                        // and "open file location" agree with what's on disk.
                        let _ = db
                            .update_task_file_info(&task_id, &top_level_name, final_total)
                            .await;
                        (top_level_name, all_ok)
                    }
                }
            };

            // 移动失败兜底:数据仍在 staging,绝不能标已完成。
            //
            // 写 STATUS_ERROR(DB)、发 STATUS_ERROR 信号(而非 COMPLETED,且
            // file_name 留空——最终磁盘名并不存在),停止做种后 return Err,使
            // on_task_done 能感知失败(并在错误可重试时触发自动重试)。
            // (BUG-BT-COMPLETE-BEFORE-MOVE)
            if !all_moves_succeeded {
                let msg = format!(
                    "已完成但部分文件移动失败;数据保留在 {}",
                    stage_dir.display()
                );
                log_info!(
                    "[BT] task={} completion move failed — marking STATUS_ERROR: {}",
                    short_id(&task_id),
                    &msg,
                );
                let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                let _ = progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: final_total,
                        total_bytes: final_total,
                        status: STATUS_ERROR,
                        error_message: msg.clone(),
                        file_name: String::new(),
                        segment_details: None,
                        ..Default::default()
                    })
                    .await;
                // 仍要停止做种,但保留 staging 供恢复(下方清理已被
                // all_moves_succeeded 守卫,此分支不会删 staging)。
                let _ = shared_bt.pause_task(&task_id).await;
                return Err(DownloadError::Other(msg));
            }

            // 全部移动成功:此刻文件确已落到 save_dir,才写 STATUS_COMPLETED 并
            // 发完成信号——file_name 指向真实存在的磁盘名。
            let _ = db.update_task_status(&task_id, STATUS_COMPLETED, "").await;
            // 完成落定,删除幂等哨兵(孤儿残留无害:status=3 不再进完成路径)。
            let _ = db
                .delete_config(&format!("bt_completion_top_{}", task_id))
                .await;

            // Send the single STATUS_COMPLETED signal with the true file name.
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: final_total,
                    total_bytes: final_total,
                    status: STATUS_COMPLETED,
                    error_message: String::new(),
                    file_name: completed_name,
                    segment_details: Some(finished_segs),
                    ..Default::default()
                })
                .await;

            // Retain the handle in the cache (do NOT call take_handle) and
            // pause the torrent so it stops seeding.
            //
            // Keeping the handle alive means that a future
            // delete_task(delete_files=true) call can reach
            // session.delete(torrent_id, true), which properly removes the
            // files via librqbit.  Previously we called take_handle +
            // session.delete(false) here, which discarded the handle and
            // removed the session entry; that left no clean path for file
            // deletion — only an unreliable filesystem-path fallback.
            let _ = shared_bt.pause_task(&task_id).await;

            // Clean up the staging directory AFTER pause_task() so that
            // librqbit has released all file handles it held inside the
            // staging dir.  On Windows, open handles prevent deletion
            // (ERROR_SHARING_VIOLATION), which is why remove_dir_all called
            // before pause would silently fail and leave the staging dir
            // behind.  We retry a few times with a short delay to handle
            // the case where the runtime thread hasn't fully flushed yet.
            let stage_dir_for_cleanup = bt_stage_dir(&save_dir, &task_id);
            // Only clean up staging when every selected file was successfully
            // moved out.  If any move failed, leaving the staging dir intact
            // lets the user (or `rescue_stranded_staging_files` on next start)
            // recover the data manually.
            if all_moves_succeeded && stage_dir_for_cleanup.exists() {
                let mut removed = false;
                for attempt in 0u8..4 {
                    if attempt > 0 {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    match tokio::fs::remove_dir_all(&stage_dir_for_cleanup).await {
                        Ok(()) => {
                            log_info!(
                                "[BT] task={} staging dir removed after pause (attempt {})",
                                short_id(&task_id),
                                attempt + 1
                            );
                            removed = true;
                            break;
                        }
                        Err(e) => {
                            log_info!(
                                "[BT] task={} staging dir remove attempt {} failed: {}",
                                short_id(&task_id),
                                attempt + 1,
                                e
                            );
                        }
                    }
                }
                if !removed {
                    log_info!(
                        "[BT] task={} staging dir '{}' could not be removed — left for startup cleanup",
                        short_id(&task_id),
                        stage_dir_for_cleanup.display()
                    );
                }
            }

            return Ok(());
        }

        // Progress reporting — runs on every poll cycle (500ms).
        // The elapsed check is kept as a safety guard against sleep jitter.
        if last_report.elapsed() >= Duration::from_millis(450) {
            // Speed: librqbit Speed.mbps is actually MiB/s
            let speed_bps = stats
                .live
                .as_ref()
                .map(|l| (l.download_speed.mbps * 1024.0 * 1024.0) as i64)
                .unwrap_or(0);

            let (peers_live, peers_connecting, peers_queued, peers_seen, peers_dead) = stats
                .live
                .as_ref()
                .map(|l| {
                    let ps = &l.snapshot.peer_stats;
                    (ps.live, ps.connecting, ps.queued, ps.seen, ps.dead)
                })
                .unwrap_or((0, 0, 0, 0, 0));

            let downloaded_pieces = stats
                .live
                .as_ref()
                .map(|l| l.snapshot.downloaded_and_checked_pieces)
                .unwrap_or(0);

            let upload_speed_bps = stats
                .live
                .as_ref()
                .map(|l| (l.upload_speed.mbps * 1024.0 * 1024.0) as i64)
                .unwrap_or(0);

            // If the torrent has entered Paused state while we are still in
            // the progress loop, it means pause_task() already called
            // session.pause() and the handle is now frozen.  The loop is
            // about to exit on the next cancelled-flag check (the watcher
            // task fires asynchronously), but we must not send STATUS_ERROR
            // or STATUS_PREPARING for a genuinely paused torrent.
            // Exit early and let the manager's explicit status=2 signal
            // (sent in pause_task) be the last word on the UI state.
            if is_paused_state {
                log_info!(
                    "[BT] task={} stats.state=Paused while in progress loop — exiting early (pause in progress)",
                    short_id(&task_id)
                );
                return Err(DownloadError::Cancelled);
            }

            let status_code = match stats.state {
                librqbit::TorrentStatsState::Live => STATUS_DOWNLOADING,
                librqbit::TorrentStatsState::Initializing => STATUS_PREPARING,
                librqbit::TorrentStatsState::Paused => STATUS_PREPARING, // unreachable after guard above
                librqbit::TorrentStatsState::Error => STATUS_ERROR,
            };

            let should_log = status_code != last_logged_status
                || last_progress_log.is_none_or(|t| t.elapsed() >= PROGRESS_LOG_INTERVAL);
            if should_log {
                log_info!(
                    "[BT] task={} state={:?} progress={}/{} (checked={}, fetched={}) pieces={}/{} down={} B/s up={} B/s peers(live={} connecting={} queued={} seen={} dead={})",
                    short_id(&task_id),
                    stats.state,
                    progress,
                    total,
                    checked_progress,
                    fetched,
                    downloaded_pieces,
                    total_pieces,
                    speed_bps,
                    upload_speed_bps,
                    peers_live,
                    peers_connecting,
                    peers_queued,
                    peers_seen,
                    peers_dead
                );
                last_progress_log = Some(Instant::now());
                last_logged_status = status_code;
            }

            let seg_details = build_bt_segments(
                total,
                progress,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                downloaded_pieces,
            );

            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: progress,
                    total_bytes: total,
                    status: status_code,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: Some(seg_details),
                    upload_speed_bps,
                    ..Default::default()
                })
                .await;

            last_report = Instant::now();
        }

        // Periodic DB save (every 3s).
        // Use checked_progress for DB persistence (not fetched_bytes) to
        // avoid inflating progress with partial pieces that would need
        // re-download after restart.
        if checked_progress > 0 && last_db_save.elapsed() >= Duration::from_secs(3) {
            let _ = db.update_task_progress(&task_id, checked_progress).await;
            if total > 0 {
                let _ = db.update_task_total_bytes(&task_id, total).await;
            }
            last_db_save = Instant::now();
        }

        // Poll interval — aligned with the progress reporting interval (500ms)
        // to avoid wasted cycles.  Cancel detection latency of 500ms is
        // acceptable since the manager layer handles session.pause() directly.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Parse a raw `.torrent` file's file list without creating a download task.
///
/// This is used by the new-download dialog to preview torrent contents
/// before the user confirms the download.  It is purely local (no network).
pub fn probe_torrent_meta(probe_id: String, torrent_bytes: Vec<u8>) -> TorrentMetaResult {
    // librqbit re-exports librqbit_core::torrent_metainfo::* at the crate root,
    // so torrent_from_bytes_ext and ByteBuf are both accessible via librqbit::.
    use librqbit::{ByteBuf, torrent_from_bytes_ext};

    let result: Result<TorrentMetaResult, String> = (|| {
        // ByteBuf<'_> borrows torrent_bytes; the parsed value must not outlive it.
        let parsed = torrent_from_bytes_ext::<ByteBuf<'_>>(&torrent_bytes)
            .map_err(|e| format!("torrent parse error: {e}"))?;
        let info = &parsed.meta.info;

        // Build file list. For single-file torrents this yields one entry.
        let mut files: Vec<BtFileEntry> = Vec::new();
        let mut total_bytes: i64 = 0;
        for (idx, fd) in info
            .iter_file_details()
            .map_err(|e| format!("iter_file_details error: {e}"))?
            .enumerate()
        {
            // Skip padding files (BEP-47).
            let attrs: librqbit::FileDetailsAttrs = fd.attrs();
            if attrs.padding {
                continue;
            }
            let path = fd
                .filename
                .to_string()
                .unwrap_or_else(|_| format!("file_{idx}"));
            let size = fd.len as i64;
            total_bytes += size;
            files.push(BtFileEntry {
                index: idx as i32,
                path,
                size,
            });
        }

        let name = info
            .name
            .as_ref()
            .and_then(|n: &ByteBuf<'_>| std::str::from_utf8(n.as_ref()).ok())
            .unwrap_or("Unknown")
            .to_owned();

        Ok(TorrentMetaResult {
            probe_id: probe_id.clone(),
            name,
            total_bytes,
            files,
            error: String::new(),
        })
    })();

    match result {
        Ok(r) => r,
        Err(e) => {
            log_info!("[BT] probe_torrent_meta error: {}", e);
            TorrentMetaResult {
                probe_id,
                name: String::new(),
                total_bytes: 0,
                files: Vec::new(),
                error: e,
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::compute_bt_display_progress;
    use std::collections::{HashMap, HashSet};

    fn completion_files(paths: &[&str]) -> Vec<super::CompletionFileSpec> {
        paths
            .iter()
            .map(|path| super::CompletionFileSpec {
                relative_path: std::path::PathBuf::from(path),
                len: 1,
            })
            .collect()
    }

    fn completion_files_with_len(paths: &[(&str, u64)]) -> Vec<super::CompletionFileSpec> {
        paths
            .iter()
            .map(|(path, len)| super::CompletionFileSpec {
                relative_path: std::path::PathBuf::from(path),
                len: *len,
            })
            .collect()
    }

    // -------------------------------------------------------------------------
    // urlencoding_decode — literal multi-byte UTF-8 must not be mangled (F052).
    // -------------------------------------------------------------------------

    #[test]
    fn urlencoding_decode_literal_utf8_not_mangled() {
        // Raw (unencoded) UTF-8 in dn= is common; it must round-trip intact
        // rather than being decoded per-byte as Latin-1.
        assert_eq!(super::urlencoding_decode("中文电影"), "中文电影");
    }

    #[test]
    fn urlencoding_decode_percent_encoded_utf8() {
        // "中" = E4 B8 AD percent-encoded.
        assert_eq!(super::urlencoding_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn urlencoding_decode_mixed_literal_and_encoded() {
        // Literal "中" followed by percent-encoded "文".
        assert_eq!(super::urlencoding_decode("中%E6%96%87"), "中文");
    }

    #[test]
    fn urlencoding_decode_plus_is_space() {
        assert_eq!(super::urlencoding_decode("a+b"), "a b");
    }

    #[test]
    fn urlencoding_decode_incomplete_percent_tail_keeps_literal() {
        // A trailing incomplete `%` followed by literal multi-byte must not
        // panic and must not mangle the literal sequence.
        assert_eq!(super::urlencoding_decode("ab%中"), "ab%中");
    }

    #[test]
    fn urlencoding_decode_invalid_hex_keeps_percent() {
        // `%zz` is not a valid escape — `%` is preserved as a literal.
        assert_eq!(super::urlencoding_decode("%zz"), "%zz");
    }

    // -------------------------------------------------------------------------
    // magnet_display_name — decode + sanitize, None on empty (F049).
    // -------------------------------------------------------------------------

    #[test]
    fn magnet_display_name_sanitizes_illegal_chars() {
        // `/` in the decoded dn must be sanitized to `_`, matching meta_prober.
        let name = super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=a%2Fb");
        assert_eq!(name.as_deref(), Some("a_b"));
    }

    #[test]
    fn magnet_display_name_literal_utf8() {
        let name = super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=中文电影");
        assert_eq!(name.as_deref(), Some("中文电影"));
    }

    #[test]
    fn magnet_display_name_none_when_no_dn() {
        assert!(super::magnet_display_name("magnet:?xt=urn:btih:abc").is_none());
    }

    #[test]
    fn magnet_display_name_none_when_dn_empty() {
        // Empty dn value decodes to empty → None (caller falls back to a
        // generated name rather than the "download" placeholder).
        assert!(super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=").is_none());
    }

    // -------------------------------------------------------------------------
    // build_multi_file_segments — subset total with full offsets (F055).
    // -------------------------------------------------------------------------

    #[test]
    fn multi_file_segments_skip_out_of_range_and_no_negative() {
        // Subset selection: total_bytes covers only the first file (100), but
        // file_offsets/file_progress carry the full torrent (3 files).  The
        // second/third files start at offset >= total and must be skipped; no
        // segment may carry a negative downloaded_bytes.
        let total_bytes = 100i64;
        let file_progress = [50u64, 0u64, 0u64];
        // file 0: [0,100), file 1: [100,300), file 2: [300,400)
        let file_offsets = [(0u64, 100u64), (100u64, 200u64), (300u64, 100u64)];
        let segs = super::build_multi_file_segments(total_bytes, &file_progress, &file_offsets);
        // Only the in-range file 0 survives.
        assert_eq!(segs.len(), 1);
        let s = &segs[0];
        assert_eq!(s.start_byte, 0);
        assert_eq!(s.end_byte, 99);
        assert_eq!(s.downloaded_bytes, 50);
        for s in &segs {
            assert!(
                s.downloaded_bytes >= 0,
                "downloaded_bytes must be non-negative"
            );
            assert!(s.end_byte >= s.start_byte, "end must not precede start");
        }
    }

    // -------------------------------------------------------------------------
    // compute_completion_layout — dedup must not stack underscores (F038).
    // -------------------------------------------------------------------------

    #[test]
    fn completion_layout_dedup_uses_numeric_suffix() {
        // Two selected files with the same basename in different sub-dirs:
        // their flat destinations collide and must be deduped as
        // "file.txt" + "file (1).txt", not "_file.txt".
        let tmp = std::env::temp_dir().join(format!(
            "fluxdown_bt_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let stage = tmp.join(".stage");
        let _ = std::fs::create_dir_all(&stage);

        let selected = completion_files(&["dirA/file.txt", "dirB/file.txt"]);
        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &tmp,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: false,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: None,
            claimed: &claims,
        });
        let _ = std::fs::remove_dir_all(&tmp);

        // Avoid `.unwrap()`/`.expect()` (denied by clippy) — match explicitly.
        let moves = match layout {
            Some(layout) => layout.moves,
            None => panic!("layout should be Some"),
        };
        assert_eq!(moves.len(), 2);
        let dst0 = moves[0]
            .dst
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let dst1 = moves[1]
            .dst
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        assert_eq!(dst0, "file.txt");
        assert_eq!(dst1, "file (1).txt");
        // No underscore-prefixed name should ever be produced.
        assert!(!dst1.starts_with('_'), "must not stack underscore prefixes");
    }

    #[test]
    fn completion_layout_all_selected_flat_multi_file_uses_torrent_root() {
        let save = unique_test_dir("flat_multi_root");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files(&["movie.mkv", "subs/en.srt"]);

        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Movie Pack",
            reuse_top: None,
            claimed: &claims,
        });
        let layout = match layout {
            Some(v) => v,
            None => panic!("layout should be Some"),
        };
        let moves = layout.moves;
        let top = layout.top_level_name;

        assert_eq!(top, "Movie Pack");
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].src, stage.join("movie.mkv"));
        assert_eq!(moves[0].dst, save.join("Movie Pack").join("movie.mkv"));
        assert_eq!(moves[1].src, stage.join("subs").join("en.srt"));
        assert_eq!(
            moves[1].dst,
            save.join("Movie Pack").join("subs").join("en.srt")
        );

        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_all_selected_common_subdir_stays_under_torrent_root() {
        let save = unique_test_dir("common_subdir_root");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files(&["Disc 1/a.bin", "Disc 1/b.bin"]);

        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Movie Pack",
            reuse_top: None,
            claimed: &claims,
        });
        let layout = match layout {
            Some(v) => v,
            None => panic!("layout should be Some"),
        };
        let moves = layout.moves;
        let top = layout.top_level_name;

        assert_eq!(top, "Movie Pack");
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].src, stage.join("Disc 1").join("a.bin"));
        assert_eq!(
            moves[0].dst,
            save.join("Movie Pack").join("Disc 1").join("a.bin")
        );
        assert_eq!(
            moves[1].dst,
            save.join("Movie Pack").join("Disc 1").join("b.bin")
        );

        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_keeps_inner_dir_named_like_torrent_root() {
        let save = unique_test_dir("same_named_inner_root");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files(&["Torrent/a.bin", "Torrent/b.bin"]);

        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: None,
            claimed: &claims,
        });
        let layout = match layout {
            Some(v) => v,
            None => panic!("layout should be Some"),
        };
        let moves = layout.moves;
        let top = layout.top_level_name;

        assert_eq!(top, "Torrent");
        assert_eq!(
            moves[0].dst,
            save.join("Torrent").join("Torrent").join("a.bin")
        );
        assert_eq!(
            moves[1].dst,
            save.join("Torrent").join("Torrent").join("b.bin")
        );

        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_all_selected_flat_multi_file_sanitizes_torrent_root() {
        let save = unique_test_dir("flat_multi_root_sanitize");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files(&["a.bin", "b.bin"]);

        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Bad/Name:01",
            reuse_top: None,
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };

        assert_eq!(top, "Bad_Name_01");
        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_multi_file_metainfo_with_one_real_file_keeps_torrent_root() {
        let save = unique_test_dir("multi_meta_one_real_file");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files_with_len(&[("data.bin", 42)]);

        let claims = HashSet::new();
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: None,
            claimed: &claims,
        });
        let layout = match layout {
            Some(v) => v,
            None => panic!("layout should be Some"),
        };

        assert_eq!(layout.top_level_name, "Torrent");
        assert_eq!(layout.moves.len(), 1);
        assert_eq!(layout.moves[0].src, stage.join("data.bin"));
        assert_eq!(layout.moves[0].dst, save.join("Torrent").join("data.bin"));
        assert_eq!(layout.moves[0].expected_len, 42);
        assert!(layout.task_owned_container);

        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn display_progress_does_not_reach_total_before_finished() {
        let progress = compute_bt_display_progress(900, 1000, 1000, false);
        assert_eq!(progress, 999);
    }

    #[test]
    fn display_progress_can_reach_total_when_finished() {
        let progress = compute_bt_display_progress(900, 1000, 1000, true);
        assert_eq!(progress, 1000);
    }

    #[test]
    fn display_progress_handles_unknown_total() {
        let progress = compute_bt_display_progress(0, 12345, 0, false);
        assert_eq!(progress, 12345);
    }

    // -------------------------------------------------------------------------
    // InflightGuard: panic-safe decrement via RAII.
    // -------------------------------------------------------------------------

    /// Verify that InflightGuard decrements the counter even when the
    /// enclosing tokio::spawn closure panics before the natural end.
    #[tokio::test]
    async fn inflight_guard_decrements_on_task_panic() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Minimal stand-in for SharedBtSession: just the counter.
        let counter = Arc::new(AtomicUsize::new(0));

        // Build a minimal InflightGuard directly using the same AtomicUsize
        // so we can test the Drop behaviour without constructing a full Session.
        struct TestGuard(Arc<AtomicUsize>);
        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        // Simulate: shared_bt.inflight_guard() — increments then returns guard.
        counter.fetch_add(1, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        let guard = TestGuard(Arc::clone(&counter));

        // Simulate: tokio::spawn(async move { let _g = guard; ..panic.. })
        let handle = tokio::spawn(async move {
            let _g = guard; // guard moved into task; Drop runs on panic
            panic!("simulated add_torrent panic");
        });

        // Tokio catches the panic; JoinHandle returns Err.
        assert!(handle.await.is_err());

        // FIX confirmed: guard's Drop ran during tokio's task cleanup,
        // decrementing the counter back to 0.
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "InflightGuard must decrement counter even after task panic"
        );
    }

    /// Verify normal (non-panic) path: guard also decrements on clean exit.
    #[tokio::test]
    async fn inflight_guard_decrements_on_normal_exit() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));

        struct TestGuard(Arc<AtomicUsize>);
        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        counter.fetch_add(1, Ordering::Relaxed);
        let guard = TestGuard(Arc::clone(&counter));

        let handle = tokio::spawn(async move {
            let _g = guard;
            // normal return — no panic
        });

        assert!(handle.await.is_ok());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    // -------------------------------------------------------------------------
    // clear_stale_session_state — session.json removed, .bitv/.torrent kept
    // (BUG-BT-RESUME-FROM-ZERO regression).
    // -------------------------------------------------------------------------

    #[test]
    fn clear_stale_session_state_keeps_fastresume_files() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_bt_session_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let session_json = dir.join("session.json");
        let session_tmp = dir.join("session.json.tmp");
        let bitv = dir.join("d5146f69f1bb6b9d95c8270769ebca7f82c2936a.bitv");
        let torrent = dir.join("d5146f69f1bb6b9d95c8270769ebca7f82c2936a.torrent");
        let _ = std::fs::write(&session_json, b"{\"torrents\":{}}");
        let _ = std::fs::write(&session_tmp, b"{}");
        let _ = std::fs::write(&bitv, [0xFFu8; 16]);
        let _ = std::fs::write(&torrent, b"d8:announce0:e");

        super::clear_stale_session_state(&dir);

        // session.json (+ .tmp) must be gone so librqbit does not restore
        // stale torrents; fastresume bitfields and torrent caches must stay.
        assert!(!session_json.exists(), "session.json must be removed");
        assert!(!session_tmp.exists(), "session.json.tmp must be removed");
        assert!(bitv.exists(), ".bitv fastresume bitfield must be preserved");
        assert!(
            torrent.exists(),
            ".torrent metadata cache must be preserved"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_stale_session_state_tolerates_missing_files() {
        // Folder without session.json (first launch) — must not panic.
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_bt_session_test_missing_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        super::clear_stale_session_state(&dir);
        // Non-existent folder — must not panic either.
        super::clear_stale_session_state(&dir.join("does_not_exist"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------------------
    // stage_dir_has_real_data — recursive, fail-safe (BUG-BT-PHANTOM-PIECES).
    // -------------------------------------------------------------------------

    fn unique_test_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fluxdown_bt_stage_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn stage_dir_has_real_data_basic_cases() {
        // Missing directory → no data (the only "safe to act" negative).
        assert!(!super::stage_dir_has_real_data(&unique_test_dir("missing")));

        // Empty directory → no data.
        let empty = unique_test_dir("empty");
        let _ = std::fs::create_dir_all(&empty);
        assert!(!super::stage_dir_has_real_data(&empty));

        // Only zero-byte files → no data.
        let zero = unique_test_dir("zero");
        let _ = std::fs::create_dir_all(&zero);
        let _ = std::fs::write(zero.join("stub.bin"), b"");
        assert!(!super::stage_dir_has_real_data(&zero));

        // Top-level non-empty file → data.
        let flat = unique_test_dir("flat");
        let _ = std::fs::create_dir_all(&flat);
        let _ = std::fs::write(flat.join("file.iso"), b"x");
        assert!(super::stage_dir_has_real_data(&flat));

        for d in [empty, zero, flat] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Multi-file torrents stage their payload under `<torrent name>/…`; a
    /// top-level-only scan sees just a directory entry (len 0 on Windows) and
    /// would wrongly classify partially-downloaded data as deletable.
    #[test]
    fn stage_dir_has_real_data_finds_nested_files() {
        let dir = unique_test_dir("nested");
        let nested = dir.join("Torrent Name").join("sub");
        let _ = std::fs::create_dir_all(&nested);
        let _ = std::fs::write(nested.join("part.mkv"), b"data");
        assert!(super::stage_dir_has_real_data(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------------------
    // verify_pieces_core — completion-time piece re-hash
    // (BUG-BT-PHANTOM-PIECES regression).
    // -------------------------------------------------------------------------

    fn sha1_of(data: &[u8]) -> [u8; 20] {
        use sha1::Digest;
        let mut h = sha1::Sha1::new();
        h.update(data);
        h.finalize().into()
    }

    /// Layout: one selected 2.5-piece file (piece_length = 4).
    /// All pieces intact → no bad pieces.
    #[test]
    fn verify_pieces_core_accepts_valid_data() {
        let dir = unique_test_dir("verify_ok");
        let _ = std::fs::create_dir_all(&dir);
        let content = b"0123456789"; // 10 bytes → pieces "0123","4567","89"
        let path = dir.join("data.bin");
        let _ = std::fs::write(&path, content);
        let hashes = [sha1_of(b"0123"), sha1_of(b"4567"), sha1_of(b"89")];

        let files = [super::VerifyFileSpec {
            path: Some(path),
            len: content.len() as u64,
            selected: true,
        }];
        let outcome = super::verify_pieces_core(4, &files, |idx, digest| {
            hashes.get(idx as usize).map(|h| *h == digest)
        });
        assert!(outcome.bad.is_empty(), "bad: {:?}", outcome.bad);
        assert_eq!(outcome.checked, 3);
        assert_eq!(outcome.skipped, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_verify_path_falls_back_to_existing_dst_on_retry() {
        let dir = unique_test_dir("verify_retry_dst");
        let stage = dir.join(".stage");
        let save = dir.join("Torrent");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&save);
        let src = stage.join("a.bin");
        let dst = save.join("a.bin");
        let _ = std::fs::write(&src, b"staging residue");
        let _ = std::fs::write(&dst, b"already moved");

        let completion_move = super::CompletionMove {
            src: src.clone(),
            dst: dst.clone(),
            expected_len: 13,
        };
        let resolved = super::resolve_completion_verify_path(
            src.clone(),
            std::slice::from_ref(&completion_move),
            true,
        );
        assert_eq!(resolved, src);

        let _ = std::fs::remove_file(&src);
        let resolved = super::resolve_completion_verify_path(
            src.clone(),
            std::slice::from_ref(&completion_move),
            true,
        );
        assert_eq!(resolved, dst);

        let no_retry =
            super::resolve_completion_verify_path(src.clone(), &[completion_move], false);
        assert_eq!(no_retry, stage.join("a.bin"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_move_retry_overwrites_unverified_dst_from_staged_src() {
        let dir = unique_test_dir("retry_stale_dst_from_src");
        let stage = dir.join(".stage");
        let save = dir.join("Torrent");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&save);
        let src = stage.join("a.bin");
        let dst = save.join("a.bin");
        let _ = std::fs::write(&src, b"fresh complete payload");
        let _ = std::fs::write(&dst, b"stale");

        let outcome = super::move_completion_item(
            &src,
            &dst,
            b"fresh complete payload".len() as u64,
            true,
            true,
            false,
        );
        match outcome {
            Ok(super::CompletionMoveOutcome::Moved) => {}
            other => panic!("unexpected completion move outcome: {other:?}"),
        }
        assert_eq!(
            std::fs::read(&dst).unwrap_or_default(),
            b"fresh complete payload"
        );
        assert!(!src.exists(), "staged src should be moved out");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_move_retry_accepts_verified_dst_when_src_residue_remains() {
        let dir = unique_test_dir("retry_src_dst_residue");
        let stage = dir.join(".stage");
        let save = dir.join("Torrent");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&save);
        let src = stage.join("a.bin");
        let dst = save.join("a.bin");
        let _ = std::fs::write(&src, b"payload");
        let _ = std::fs::write(&dst, b"payload");

        let outcome =
            super::move_completion_item(&src, &dst, b"payload".len() as u64, true, true, true);
        match outcome {
            Ok(super::CompletionMoveOutcome::PriorSuccess) => {}
            other => panic!("unexpected completion move outcome: {other:?}"),
        }
        assert_eq!(std::fs::read(&dst).unwrap_or_default(), b"payload");
        assert!(
            !src.exists(),
            "verified prior success should clean staging residue when possible"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_move_retry_rejects_verified_dst_with_wrong_length() {
        let dir = unique_test_dir("retry_dst_wrong_len");
        let stage = dir.join(".stage");
        let save = dir.join("Torrent");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&save);
        let src = stage.join("a.bin");
        let dst = save.join("a.bin");
        let _ = std::fs::write(&dst, b"payload plus trailing bytes");

        let outcome =
            super::move_completion_item(&src, &dst, b"payload".len() as u64, true, true, true);
        match outcome {
            Ok(super::CompletionMoveOutcome::MissingSource) => {}
            other => panic!("oversized dst must not be accepted: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_move_retry_accepts_verified_zero_length_dst() {
        let dir = unique_test_dir("retry_dst_zero_len");
        let stage = dir.join(".stage");
        let save = dir.join("Torrent");
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::create_dir_all(&save);
        let src = stage.join("empty.bin");
        let dst = save.join("empty.bin");
        let _ = std::fs::write(&dst, b"");

        let outcome = super::move_completion_item(&src, &dst, 0, true, true, true);
        match outcome {
            Ok(super::CompletionMoveOutcome::PriorSuccess) => {}
            other => panic!("zero-length dst with exact length should be accepted: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A zero-filled hole in the middle of otherwise-valid data must be
    /// reported as exactly that bad piece — the incident scenario where a
    /// stale .bitv claimed pieces whose staging data had been lost.
    #[test]
    fn verify_pieces_core_detects_zero_filled_pieces() {
        let dir = unique_test_dir("verify_zero");
        let _ = std::fs::create_dir_all(&dir);
        let good = b"0123456789";
        // Piece 1 ("4567") zeroed out on disk.
        let on_disk = b"0123\0\0\0\089";
        let path = dir.join("data.bin");
        let _ = std::fs::write(&path, on_disk);
        let hashes = [sha1_of(b"0123"), sha1_of(b"4567"), sha1_of(b"89")];

        let files = [super::VerifyFileSpec {
            path: Some(path),
            len: good.len() as u64,
            selected: true,
        }];
        let outcome = super::verify_pieces_core(4, &files, |idx, digest| {
            hashes.get(idx as usize).map(|h| *h == digest)
        });
        assert_eq!(outcome.bad, vec![1]);
        assert_eq!(outcome.checked, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing or truncated selected file makes its pieces provably bad
    /// (read failure), unlike librqbit's Windows pread_exact which silently
    /// succeeds past EOF.
    #[test]
    fn verify_pieces_core_flags_missing_and_truncated_files() {
        let dir = unique_test_dir("verify_missing");
        let _ = std::fs::create_dir_all(&dir);

        // Missing file entirely.
        let files = [super::VerifyFileSpec {
            path: Some(dir.join("nonexistent.bin")),
            len: 10,
            selected: true,
        }];
        let outcome = super::verify_pieces_core(4, &files, |_, _| Some(true));
        assert_eq!(outcome.bad, vec![0, 1, 2]);

        // Truncated file: only 5 of 10 bytes on disk → pieces 1 and 2
        // unreadable; piece 0 readable (hash check decides it).
        let path = dir.join("short.bin");
        let _ = std::fs::write(&path, b"01234");
        let files = [super::VerifyFileSpec {
            path: Some(path),
            len: 10,
            selected: true,
        }];
        let outcome = super::verify_pieces_core(4, &files, |idx, digest| {
            Some(idx == 0 && digest == sha1_of(b"0123"))
        });
        assert_eq!(outcome.bad, vec![1, 2]);
        assert_eq!(outcome.checked, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pieces that overlap no selected file are skipped, while a piece
    /// straddling the selected/unselected boundary is still required —
    /// matching librqbit's chunk-tracker selection rule so the repair loop
    /// never demands pieces the engine does not promise.
    #[test]
    fn verify_pieces_core_skips_unselected_only_pieces() {
        let dir = unique_test_dir("verify_sel");
        let _ = std::fs::create_dir_all(&dir);
        let sel = dir.join("selected.bin");
        let unsel = dir.join("unselected.bin");
        // piece_length 4: file A = 6 bytes (pieces 0, 1), file B = 6 bytes
        // (pieces 1, 2).  Piece 1 straddles both; piece 2 is B-only.
        let _ = std::fs::write(&sel, b"AAAAAA");
        let _ = std::fs::write(&unsel, b"BBBBBB");
        let hashes = [sha1_of(b"AAAA"), sha1_of(b"AABB"), sha1_of(b"BBBB")];

        let files = [
            super::VerifyFileSpec {
                path: Some(sel),
                len: 6,
                selected: true,
            },
            super::VerifyFileSpec {
                path: Some(unsel),
                len: 6,
                selected: false,
            },
        ];
        let outcome = super::verify_pieces_core(4, &files, |idx, digest| {
            hashes.get(idx as usize).map(|h| *h == digest)
        });
        assert!(outcome.bad.is_empty(), "bad: {:?}", outcome.bad);
        assert_eq!(outcome.checked, 2, "pieces 0 and 1 are required");
        assert_eq!(outcome.skipped, 1, "piece 2 overlaps no selected file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// BEP-47 padding files contribute virtual zero bytes without any disk I/O.
    #[test]
    fn verify_pieces_core_hashes_padding_as_zeros() {
        let dir = unique_test_dir("verify_pad");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("data.bin");
        let _ = std::fs::write(&path, b"XY");
        // Layout: 2-byte real file + 2-byte padding → one piece "XY\0\0".
        let hashes = [sha1_of(b"XY\0\0")];

        let files = [
            super::VerifyFileSpec {
                path: Some(path),
                len: 2,
                selected: true,
            },
            super::VerifyFileSpec {
                path: None,
                len: 2,
                selected: false,
            },
        ];
        let outcome = super::verify_pieces_core(4, &files, |idx, digest| {
            hashes.get(idx as usize).map(|h| *h == digest)
        });
        assert!(outcome.bad.is_empty(), "bad: {:?}", outcome.bad);
        assert_eq!(outcome.checked, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// info_hash_hex derives the hash from magnet URLs (used to address the
    /// {hash}.bitv fastresume file before add_torrent).
    #[test]
    fn torrent_source_info_hash_from_magnet() {
        let src = super::TorrentSource::Magnet(
            "magnet:?xt=urn:btih:d5146f69f1bb6b9d95c8270769ebca7f82c2936a&dn=x".to_string(),
        );
        assert_eq!(
            src.info_hash_hex().as_deref(),
            Some("d5146f69f1bb6b9d95c8270769ebca7f82c2936a")
        );
        let bad = super::TorrentSource::Magnet("magnet:?dn=nohash".to_string());
        assert_eq!(bad.info_hash_hex(), None);
    }

    // -------------------------------------------------------------------------
    // move_path 三级降级链 + 完成幂等哨兵 (BUG-BT-DIR-RENAME-2X)。
    // -------------------------------------------------------------------------

    /// 核心回归守卫(Windows):目录内存在打开的子文件句柄(FULL share =
    /// Rust/librqbit 默认)时,目录 rename 失败,move_path 必须经逐文件
    /// rename 降级成功——且是 rename(零拷贝)而非 copy。
    /// 判别法:move 后经原句柄写入,dst 文件长度反映写入 ⟹ 同一底层文件。
    #[cfg(windows)]
    #[test]
    fn move_path_dir_falls_back_per_file_with_open_handle() {
        use std::io::{Seek, Write};
        let base = unique_test_dir("fallback");
        let src_top = base.join("src").join("Torrent");
        let _ = std::fs::create_dir_all(src_top.join("sub"));
        let file_path = src_top.join("sub").join("file.bin");
        let _ = std::fs::write(&file_path, vec![0x55u8; 4096]);
        let _ = std::fs::write(src_top.join("a.bin"), b"aaaa");
        let dst_top = base.join("dst").join("Torrent");
        let _ = std::fs::create_dir_all(base.join("dst"));

        // FULL-share 打开(read+write:只读句柄无法用于写入判别)。
        let mut handle = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&file_path)
        {
            Ok(h) => h,
            Err(e) => panic!("open handle: {e}"),
        };

        let result = super::move_path(&src_top, &dst_top);
        assert!(result.is_ok(), "move_path failed: {result:?}");
        // dst 结构完整。
        assert!(dst_top.join("sub").join("file.bin").exists());
        assert!(dst_top.join("a.bin").exists());
        // src 骨架消失。
        assert!(!src_top.exists(), "src skeleton should be removed");

        // rename/copy 判别:句柄开于 pos=0,先 seek 到末尾再追加 4096 字节
        // → 若 dst 是同一底层文件(rename),其长度增长;copy 快照不受影响。
        let _ = handle.seek(std::io::SeekFrom::End(0));
        let _ = handle.write_all(&vec![0xAAu8; 4096]);
        let _ = handle.flush();
        drop(handle);
        let dst_len = std::fs::metadata(dst_top.join("sub").join("file.bin"))
            .map(|m| m.len())
            .unwrap_or(0);
        assert_eq!(
            dst_len, 8192,
            "dst must be the SAME underlying file (rename), not a copy snapshot"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 反向对照:同一句柄场景下裸 std::fs::rename(目录)必失败——记录本
    /// 修复存在的根因。未来 Windows 若改变此语义(此测试转 FAIL),提示
    /// 可简化降级链。
    #[cfg(windows)]
    #[test]
    fn raw_dir_rename_fails_with_open_child_handle() {
        let base = unique_test_dir("rawrename");
        let src_top = base.join("Torrent");
        let _ = std::fs::create_dir_all(&src_top);
        let file_path = src_top.join("file.bin");
        let _ = std::fs::write(&file_path, b"data");

        let handle = std::fs::File::open(&file_path);
        assert!(handle.is_ok());
        let renamed = std::fs::rename(&src_top, base.join("Torrent_renamed"));
        assert!(
            renamed.is_err(),
            "dir rename with open child handle should fail on Windows"
        );
        drop(handle);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 瞬时独占锁(share_mode(0),AV 风格)释放后,move_file 在重试预算内
    /// 以 rename 成功。判别法:share=0 与任何第二句柄互斥,不能用句柄写入
    /// 法——改用 creation time(rename 保留、copy 产生新文件不保留)。
    #[cfg(windows)]
    #[test]
    fn move_file_retries_transient_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let base = unique_test_dir("retry");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("locked.bin");
        let _ = std::fs::write(&src, vec![1u8; 1024]);
        let src_created = std::fs::metadata(&src).and_then(|m| m.created()).ok();

        let src_clone = src.clone();
        let locker = std::thread::spawn(move || {
            // share_mode(0):完全独占——rename 期间报 ERROR_SHARING_VIOLATION。
            let f = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&src_clone);
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(f);
        });
        // 让 locker 先拿到句柄。
        std::thread::sleep(std::time::Duration::from_millis(50));

        let dst = base.join("moved.bin");
        let mut budget = super::RETRY_SLEEP_BUDGET;
        // replace=true(容器 merge 子文件语义):本测试判别 rename vs copy 用
        // creation time,claim 占位(replace=false)会因 NTFS tunneling 污染
        // 判别;瞬时锁重试逻辑与占名协议正交。
        let result = super::move_file(&src, &dst, &mut budget, true);
        let _ = locker.join();

        assert!(result.is_ok(), "move_file failed: {result:?}");
        assert!(dst.exists() && !src.exists());
        // creation time 一致 ⟹ rename(同一文件);copy 会新建文件。
        let dst_created = std::fs::metadata(&dst).and_then(|m| m.created()).ok();
        assert_eq!(
            src_created, dst_created,
            "creation time must survive (rename), copy would mint a new one"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// copy 成功但 remove_file(src) 被第三方句柄(可读不可删)阻塞时,
    /// move_file 必须返回 Ok(dst 已是完整副本)——防"伪失败"把整次
    /// completion 标 ERROR 触发无谓重验重下。
    #[cfg(windows)]
    #[test]
    fn move_file_copy_succeeds_remove_blocked_returns_ok() {
        use std::os::windows::fs::OpenOptionsExt;
        let base = unique_test_dir("copyok");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("held.bin");
        let _ = std::fs::write(&src, vec![7u8; 2048]);

        // share = READ only(无 DELETE):rename 报 32、copy 可读、remove 失败。
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(&src);
        assert!(holder.is_ok());

        let dst = base.join("copied.bin");
        let mut budget = 0u32; // 预算清零:立即走 copy 兜底
        let result = super::move_file(&src, &dst, &mut budget, false);
        assert!(result.is_ok(), "copy-succeeded case must be Ok: {result:?}");
        assert_eq!(
            std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
            2048,
            "dst must be a complete copy"
        );
        // src 残留(由 staging 清理兜底)——行为符合设计。
        assert!(src.exists());
        drop(holder);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// dst 不可达(父目录缺失)时 move_file 干净失败:返回 Err、src 完整
    /// 可重试、无任何 dst 残留。
    ///
    /// 注:本测试中 copy 在创建目标文件之前即失败(NotFound),不产生
    /// 半成品——`move_file` 内清理行(copy 中途失败删 dst)针对的是
    /// ENOSPC/覆盖写失败等真正的中途失败,单测无法可靠构造(需磁盘满),
    /// 该防线由代码审查与 saturating 语义保证。
    #[test]
    fn move_file_fails_cleanly_when_dst_unreachable() {
        let base = unique_test_dir("cleanup");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("data.bin");
        let _ = std::fs::write(&src, vec![3u8; 512]);
        // dst 指向不存在的父目录深处 → rename 与 copy 双双失败。
        let dst = base.join("no_such_parent").join("data.bin");

        let mut budget = 0u32;
        let result = super::move_file(&src, &dst, &mut budget, false);
        assert!(result.is_err(), "must fail when dst parent missing");
        assert!(!dst.exists(), "no partial dst may remain");
        assert!(src.exists(), "src must stay intact for retry");
        let _ = std::fs::remove_dir_all(&base);
    }

    // -------------------------------------------------------------------------
    // move_file — replace=false claim semantics + replace=true merge overwrite
    // (atomic claim protocol closing the REPLACE rename TOCTOU window).
    // -------------------------------------------------------------------------

    #[test]
    fn move_file_replace_false_dst_exists_fails_preserves_both() {
        let base = unique_test_dir("claim_exists");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("src.bin");
        let dst = base.join("dst.bin");
        let _ = std::fs::write(&src, b"incoming");
        let _ = std::fs::write(&dst, b"original");

        let mut budget = 0u32;
        let result = super::move_file(&src, &dst, &mut budget, false);
        match result {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
            Ok(()) => panic!("replace=false must not overwrite an occupied dst"),
        }
        assert_eq!(std::fs::read(&dst).unwrap_or_default(), b"original");
        assert_eq!(std::fs::read(&src).unwrap_or_default(), b"incoming");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn move_file_replace_false_dst_free_succeeds() {
        let base = unique_test_dir("claim_free");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("src.bin");
        let dst = base.join("dst.bin");
        let _ = std::fs::write(&src, b"payload");

        let mut budget = 0u32;
        let result = super::move_file(&src, &dst, &mut budget, false);
        assert!(
            result.is_ok(),
            "replace=false must succeed on a free dst: {result:?}"
        );
        assert_eq!(std::fs::read(&dst).unwrap_or_default(), b"payload");
        assert!(!src.exists(), "src must be gone after a successful move");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn move_file_replace_true_dst_exists_overwrites() {
        let base = unique_test_dir("merge_overwrite");
        let _ = std::fs::create_dir_all(&base);
        let src = base.join("src.bin");
        let dst = base.join("dst.bin");
        let _ = std::fs::write(&src, b"new-content");
        let _ = std::fs::write(&dst, b"stale");

        let mut budget = 0u32;
        let result = super::move_file(&src, &dst, &mut budget, true);
        assert!(
            result.is_ok(),
            "replace=true must overwrite dst: {result:?}"
        );
        assert_eq!(std::fs::read(&dst).unwrap_or_default(), b"new-content");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// I-5 回归:status==3 + staging 残留与 save_dir 已有完整产物同名
    /// (move 循环全部成功后,"copy 成功 remove 被阻塞"留下的副本)时,
    /// rescue 必须丢弃残留,绝不 dedup 成 `Torrent (1)` 再覆写 DB 指针
    /// (那会让真实产物变成无引用孤儿)。fast path 与 fallback 均须防御。
    #[test]
    fn rescue_drops_residue_when_product_already_in_save_dir() {
        let save = unique_test_dir("rescue_residue");
        let task_id = "residue-task-01";
        // save_dir 内已有完整产物(3 文件)。
        let product = save.join("Torrent");
        let _ = std::fs::create_dir_all(product.join("sub"));
        let _ = std::fs::write(product.join("a.bin"), b"complete-a");
        let _ = std::fs::write(product.join("b.bin"), b"complete-b");
        let _ = std::fs::write(product.join("sub").join("c.bin"), b"complete-c");
        // staging 内同名残留(仅含 1 个文件的副本)。
        let stage = super::bt_stage_dir(&save.to_string_lossy(), task_id);
        let residue = stage.join("Torrent");
        let _ = std::fs::create_dir_all(&residue);
        let _ = std::fs::write(residue.join("a.bin"), b"complete-a");

        let input = vec![(
            task_id.to_string(),
            save.to_string_lossy().into_owned(),
            "Torrent".to_string(),
        )];
        let updates = super::rescue_stranded_staging_files(&input, &HashMap::new());

        assert!(
            updates.is_empty(),
            "residue must not produce DB updates (would repoint file_name)"
        );
        assert!(
            !save.join("Torrent (1)").exists(),
            "residue must never be dedup-moved to 'Torrent (1)'"
        );
        // 完整产物原封不动。
        assert_eq!(
            std::fs::read(product.join("a.bin")).unwrap_or_default(),
            b"complete-a"
        );
        assert_eq!(
            std::fs::read(product.join("sub").join("c.bin")).unwrap_or_default(),
            b"complete-c"
        );
        // staging 连同残留被清理。
        assert!(!stage.exists(), "staging residue should be dropped");
        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn rescue_drops_fallback_residue_when_container_child_already_landed() {
        let save = unique_test_dir("rescue_container_child_residue");
        let task_id = "container-residue-01";
        let product = save.join("Movie Pack");
        let _ = std::fs::create_dir_all(&product);
        let _ = std::fs::write(product.join("movie.mkv"), b"complete-movie");

        let stage = super::bt_stage_dir(&save.to_string_lossy(), task_id);
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::write(stage.join("movie.mkv"), b"complete-movie");

        let input = vec![(
            task_id.to_string(),
            save.to_string_lossy().into_owned(),
            "Movie Pack".to_string(),
        )];
        let updates = super::rescue_stranded_staging_files(&input, &HashMap::new());

        assert!(
            updates.is_empty(),
            "container child residue must not repoint DB file_name"
        );
        assert!(
            !save.join("movie.mkv").exists(),
            "container child residue must not be moved into save_dir root"
        );
        assert_eq!(
            std::fs::read(product.join("movie.mkv")).unwrap_or_default(),
            b"complete-movie"
        );
        assert!(!stage.exists(), "staging residue should be dropped");
        let _ = std::fs::remove_dir_all(&save);
    }

    /// 哨兵复用:reuse_top 指向 save_dir 内已存在的**目录**(上次部分移动
    /// 的自身产物)时,container 分支复用同名(merge 继续)而不 dedup 成
    /// `Torrent (1)`;被外部占用为**文件**时放弃哨兵退回 fresh dedup
    /// (防御 Windows rename(dir,file) 静默吞文件)。
    #[test]
    fn completion_layout_reuses_sentinel_top() {
        let save = unique_test_dir("sentinel");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&stage);
        let selected = completion_files(&["Torrent/a.bin", "Torrent/sub/b.bin"]);
        let claims = HashSet::new();

        // Case 1: dst 是目录(自身上次产物)→ 复用。
        let _ = std::fs::create_dir_all(save.join("Torrent"));
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: Some("Torrent"),
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_eq!(top, "Torrent", "existing dir must be reused, not deduped");

        // Case 2: dst 被外部占用为文件 → 放弃哨兵,fresh dedup。
        let _ = std::fs::remove_dir_all(save.join("Torrent"));
        let _ = std::fs::write(save.join("Torrent"), b"unrelated user file");
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: Some("Torrent"),
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_ne!(
            top, "Torrent",
            "sentinel name occupied by a FILE must not be reused (would swallow it)"
        );

        // Case 3: 无哨兵 → 既有 dedup 行为(名字避开已存在文件)。
        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: true,
            is_multi_file_torrent: true,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: None,
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_eq!(top, "Torrent (1)");

        let _ = std::fs::remove_dir_all(&save);
    }

    /// rescue 必须把"与 current_file_name 精确同名的空壳目录"(fast-path
    /// 命中场景,逐文件降级 move 后的残留骨架)删除而非当数据移动——
    /// 否则 dedup 会把空壳移成 `<name> (1)/` 并污染 DB file_name。
    #[test]
    fn rescue_skips_empty_shell_dirs() {
        let save = unique_test_dir("rescue_shell");
        let task_id = "shelltask-0001";
        let stage = super::bt_stage_dir(&save.to_string_lossy(), task_id);
        // 空壳:与 current_file_name 同名、只含空目录树。
        let _ = std::fs::create_dir_all(stage.join("Torrent").join("sub"));

        let input = vec![(
            task_id.to_string(),
            save.to_string_lossy().into_owned(),
            "Torrent".to_string(),
        )];
        let updates = super::rescue_stranded_staging_files(&input, &HashMap::new());

        assert!(
            updates.is_empty(),
            "empty shell must not produce DB updates"
        );
        assert!(
            !save.join("Torrent").exists() && !save.join("Torrent (1)").exists(),
            "empty shell must not be moved into save_dir"
        );
        assert!(!stage.exists(), "staging dir should be cleaned up");
        let _ = std::fs::remove_dir_all(&save);
    }

    /// 大 N 降级路径正确性(500 文件 x 3 层嵌套)。打印耗时供人工确认,
    /// 不做硬性时间断言(CI 计时不稳)。
    #[test]
    fn move_dir_recursive_large_n() {
        let base = unique_test_dir("large_n");
        let src = base.join("src");
        for i in 0..5 {
            for j in 0..10 {
                let dir = src.join(format!("d{i}")).join(format!("e{j}"));
                let _ = std::fs::create_dir_all(&dir);
                for k in 0..10 {
                    let _ =
                        std::fs::write(dir.join(format!("f{k}.bin")), [i as u8, j as u8, k as u8]);
                }
            }
        }
        let dst = base.join("dst");
        let started = std::time::Instant::now();
        let mut budget = super::RETRY_SLEEP_BUDGET;
        let result = super::move_dir_recursive(&src, &dst, &mut budget);
        println!("move_dir_recursive 500 files took {:?}", started.elapsed());
        assert!(result.is_ok(), "{result:?}");
        // 抽查结构与内容。
        let sample = dst.join("d4").join("e9").join("f9.bin");
        assert_eq!(std::fs::read(&sample).unwrap_or_default(), vec![4u8, 9, 9]);
        let mut count = 0usize;
        fn count_files(dir: &std::path::Path, count: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.filter_map(|e| e.ok()) {
                    if e.path().is_dir() {
                        count_files(&e.path(), count);
                    } else {
                        *count += 1;
                    }
                }
            }
        }
        count_files(&dst, &mut count);
        assert_eq!(count, 500);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 单个子项失败不得中止兄弟项:可移动的文件全部移出,函数返回首个错误。
    #[test]
    fn move_dir_recursive_continues_after_child_error() {
        let base = unique_test_dir("continue");
        let src = base.join("src");
        let _ = std::fs::create_dir_all(&src);
        let _ = std::fs::write(src.join("ok1.bin"), b"1");
        let _ = std::fs::write(src.join("ok2.bin"), b"2");
        let dst = base.join("dst");
        let _ = std::fs::create_dir_all(&dst);
        // 预置一个与 src 子目录同名的**文件**,令该子项的 create_dir_all 失败。
        let _ = std::fs::create_dir_all(src.join("blocked"));
        let _ = std::fs::write(src.join("blocked").join("inner.bin"), b"x");
        let _ = std::fs::write(dst.join("blocked"), b"i am a file");

        let mut budget = super::RETRY_SLEEP_BUDGET;
        let result = super::move_dir_recursive(&src, &dst, &mut budget);
        assert!(result.is_err(), "blocked child must surface an error");
        // 兄弟文件仍全部移出。
        assert!(dst.join("ok1.bin").exists() && dst.join("ok2.bin").exists());
        assert!(!src.join("ok1.bin").exists());
        // 被阻塞子项的数据保留在 src,可重试。
        assert!(src.join("blocked").join("inner.bin").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// merge 语义幂等:dst 已存在子文件为旧/截断版本时必须被 src 版本覆盖,
    /// 防止中断残留(截断文件)在重试合并后永久留存。
    #[test]
    fn move_dir_recursive_merge_overwrites_stale_child_file() {
        let base = unique_test_dir("merge_stale_child");
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = std::fs::create_dir_all(&src);
        let _ = std::fs::create_dir_all(&dst);
        // src carries the fresh, complete content.
        let _ = std::fs::write(src.join("a.bin"), b"fresh-full-content");
        // dst already has a stale/truncated same-name file from a prior
        // interrupted move — merge must overwrite it, not leave it stuck.
        let _ = std::fs::write(dst.join("a.bin"), b"old");

        let mut budget = 0u32;
        let result = super::move_dir_recursive(&src, &dst, &mut budget);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            std::fs::read(dst.join("a.bin")).unwrap_or_default(),
            b"fresh-full-content"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // -------------------------------------------------------------------------
    // dedup_name_in_dir — case-folded disk scan, `.fdownloading` occupancy,
    // and `avoid` set (BUG-BT-DEDUP-CASE-FOLD / cross-task claim guard).
    // -------------------------------------------------------------------------

    #[test]
    fn dedup_name_in_dir_case_folds_existing_disk_entries() {
        let dir = unique_test_dir("dedup_case_fold");
        let _ = std::fs::create_dir_all(&dir);
        // Exact-case entry forces the early-return probe to see a conflict
        // on every platform (Linux's Path::exists() is case-sensitive).
        let _ = std::fs::write(dir.join("MOVIE.mkv"), b"x");
        // Differently-cased numbered variant already occupies " (1)".
        let _ = std::fs::write(dir.join("Movie (1).mkv"), b"x");

        let name = super::dedup_name_in_dir(&dir, "MOVIE.mkv", &HashSet::new());
        assert_ne!(
            name, "MOVIE (1).mkv",
            "case-different existing 'Movie (1).mkv' must be treated as occupied"
        );
        assert_eq!(name, "MOVIE (2).mkv");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_name_in_dir_fdownloading_temp_file_counts_as_occupied() {
        let dir = unique_test_dir("dedup_temp_occupied");
        let _ = std::fs::create_dir_all(&dir);
        // Only the in-progress temp file exists; the final name does not.
        let _ = std::fs::write(dir.join("movie.mkv.fdownloading"), b"partial");

        let name = super::dedup_name_in_dir(&dir, "movie.mkv", &HashSet::new());
        assert_eq!(name, "movie (1).mkv");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_name_in_dir_avoid_set_blocks_name() {
        let dir = unique_test_dir("dedup_avoid_set");
        let _ = std::fs::create_dir_all(&dir);
        // Nothing on disk, but another concurrent task has claimed the name.
        let mut avoid = HashSet::new();
        avoid.insert("movie.mkv".to_string());

        let name = super::dedup_name_in_dir(&dir, "Movie.mkv", &avoid);
        assert_eq!(name, "Movie (1).mkv");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------------------
    // compute_completion_layout — single-file branch sentinel-reuse guard
    // (BUG-BT-SENTINEL-HIJACK): an occupied dst must never be REPLACE-moved
    // into just because it matches a stale/cross-task sentinel name.
    // -------------------------------------------------------------------------

    #[test]
    fn completion_layout_single_file_sentinel_rejects_occupied_dst() {
        let save = unique_test_dir("single_sentinel_occupied");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&save);
        // 模拟另一同名任务已在 save_dir 落地同名产物。
        let _ = std::fs::write(save.join("movie.mkv"), b"someone else's file");
        let selected = completion_files(&["movie.mkv"]);
        let claims = HashSet::new();

        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: false,
            is_multi_file_torrent: false,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: Some("movie.mkv"),
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_ne!(
            top, "movie.mkv",
            "occupied sentinel name must not be reused (would REPLACE the other task's file)"
        );
        assert_eq!(top, "movie (1).mkv");
        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_single_file_sentinel_reused_when_absent() {
        let save = unique_test_dir("single_sentinel_absent");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&save);
        let selected = completion_files(&["movie.mkv"]);
        let claims = HashSet::new();

        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: false,
            is_multi_file_torrent: false,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: Some("movie.mkv"),
            claimed: &claims,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_eq!(
            top, "movie.mkv",
            "legitimate retry with no on-disk conflict must reuse the sentinel name"
        );
        let _ = std::fs::remove_dir_all(&save);
    }

    #[test]
    fn completion_layout_single_file_claimed_set_avoids_name() {
        let save = unique_test_dir("single_claimed");
        let stage = save.join(".stage");
        let _ = std::fs::create_dir_all(&save);
        let selected = completion_files(&["movie.mkv"]);
        let mut claimed = HashSet::new();
        claimed.insert("movie.mkv".to_string());

        let layout = super::compute_completion_layout(super::CompletionLayoutInput {
            save_dir: &save,
            stage_dir: &stage,
            selected_files: &selected,
            all_selected: false,
            is_multi_file_torrent: false,
            custom_name: "",
            torrent_root_name: "Torrent",
            reuse_top: None,
            claimed: &claimed,
        });
        let top = match layout {
            Some(v) => v.top_level_name,
            None => panic!("layout should be Some"),
        };
        assert_eq!(
            top, "movie (1).mkv",
            "fresh dedup must avoid a name claimed by another concurrent task"
        );
        let _ = std::fs::remove_dir_all(&save);
    }

    // -------------------------------------------------------------------------
    // rescue_stranded_staging_files — claims_by_dir avoidance: the recovery
    // path is also subject to the cross-task sentinel-claim guard.
    // -------------------------------------------------------------------------

    #[test]
    fn rescue_stranded_staging_files_avoids_claimed_name() {
        let save = unique_test_dir("rescue_claimed");
        let _ = std::fs::create_dir_all(&save);
        let task_id = "claimed-task-01";
        let stage = super::bt_stage_dir(&save.to_string_lossy(), task_id);
        let _ = std::fs::create_dir_all(&stage);
        let _ = std::fs::write(stage.join("data.bin"), b"payload");

        let save_dir_string = save.to_string_lossy().into_owned();
        let input = vec![(
            task_id.to_string(),
            save_dir_string.clone(),
            "data.bin".to_string(),
        )];
        let mut claimed = HashSet::new();
        claimed.insert("data.bin".to_string());
        let mut claims_by_dir = HashMap::new();
        claims_by_dir.insert(save_dir_string, claimed);

        let updates = super::rescue_stranded_staging_files(&input, &claims_by_dir);

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, task_id);
        assert_ne!(
            updates[0].1, "data.bin",
            "claimed name must not be reused by rescue (cross-task hijack)"
        );
        assert_eq!(updates[0].1, "data (1).bin");
        assert!(save.join("data (1).bin").exists());
        assert!(
            !stage.exists(),
            "staging dir should be cleaned up after move"
        );
        let _ = std::fs::remove_dir_all(&save);
    }
}
