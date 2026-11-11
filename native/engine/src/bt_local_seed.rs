//! 本机文件做种：把用户磁盘上已有的文件 / 文件夹登记为「已完成 + 做种」的 BT 任务。
//!
//! 两种来源，殊途同归（见 [`crate::download_manager::DownloadManager::create_local_seed`]）：
//! - 只给内容路径：由 [`build_torrent`] 现场为其生成种子（对大文件是分钟级的哈希，
//!   必须在 actor 之外调用）；
//! - 同时给出现成的 `.torrent`：只校验它与所选路径的形态匹配，哈希由续种路径异步校验。
//!
//! 内容路径的布局与完成态任务一致：任务 `save_dir` = 路径的父目录、`file_name` = 路径末段，
//! 续种时单文件落在 `save_dir`、多文件以 `save_dir/<file_name>` 为根。

use std::path::Path;

use thiserror::Error;

/// 本机做种的准备 / 登记错误。
#[derive(Debug, Error)]
pub enum LocalSeedError {
    /// 参数不合法（路径不存在、种子与路径不匹配、BT 被禁用…），调用方应原样回显。
    #[error("{0}")]
    InvalidArgument(String),
    /// 生成种子或落库失败。
    #[error("{0}")]
    Operation(String),
    /// 同一个种子（info hash）已被另一任务持有，未重复创建。
    #[error("duplicate torrent: already managed by task {task_id}")]
    Duplicate {
        /// 已持有该种子的任务 id。
        task_id: String,
        /// 该任务的显示名（可能为空）。
        name: String,
    },
}

/// 通过校验、可直接落库的本机做种计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSeedPlan {
    /// `.torrent` 字节。
    pub torrent_bytes: Vec<u8>,
    /// 任务保存目录（内容路径的父目录）。
    pub save_dir: String,
    /// 任务文件名（内容路径末段，与种子 `name` 一致）。
    pub file_name: String,
    /// 种子内全部文件总大小。
    pub total_bytes: i64,
}

/// 本机做种任务的登记选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalSeedOptions {
    /// 任务级分享率 / 时长 / 不活跃限制全部「不限制」（永不因限制停止做种）。
    pub unlimited: bool,
    /// 以超级做种模式（BEP 16）做种。
    pub super_seeding: bool,
    /// 信任磁盘数据与种子一致：首次挂载只核对文件长度、不做哈希校验（数据不符时上传的
    /// 片会被对端拒收）。刚为这份数据生成的种子同样不必再算一遍。
    pub skip_check: bool,
}

/// 内容路径的末段名与父目录。
fn split_content_path(path: &Path) -> Result<(String, String), LocalSeedError> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| LocalSeedError::InvalidArgument("path has no file name".to_owned()))?;
    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .filter(|parent| !parent.is_empty())
        .ok_or_else(|| {
            LocalSeedError::InvalidArgument("path has no parent directory".to_owned())
        })?;
    Ok((parent, name))
}

fn ensure_content_path(path: &Path) -> Result<(), LocalSeedError> {
    if !path.is_absolute() {
        return Err(LocalSeedError::InvalidArgument(
            "path must be absolute".to_owned(),
        ));
    }
    if !path.exists() {
        return Err(LocalSeedError::InvalidArgument(
            "path does not exist".to_owned(),
        ));
    }
    Ok(())
}
/// 任务的分享材料：info hash、展示名与可直接发给别人的磁力链接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentShare {
    /// 小写十六进制 info hash（v1 / 混合种子为 SHA-1，纯 v2 种子为截断的 SHA-256）。
    pub info_hash: String,
    /// v2 info hash（SHA-256，小写十六进制）；v1 种子为空。
    pub info_hash_v2: String,
    /// 种子名。
    pub name: String,
    /// 磁力链接：种子自带 tracker + 本机会话会追加的 tracker（自定义 / 内置公共 / 订阅，
    /// 对齐 qBittorrent「自动追加 tracker」），让对方不只靠 DHT 就能找到本机。
    pub magnet: String,
}

/// 由 `.torrent` 字节生成分享材料。`extra_trackers` 为会话追加的 tracker 列表。
/// 磁力链接的 `xt` 随种子版本：v1 = `btih`，纯 v2 = `btmh`，混合种子两个都带（BEP 52）。
///
/// # Errors
/// 种子无法解析。
pub fn torrent_share(
    torrent_bytes: &[u8],
    extra_trackers: &[String],
) -> Result<TorrentShare, LocalSeedError> {
    let parsed = librqbit::torrent_from_bytes(torrent_bytes).map_err(|error| {
        LocalSeedError::InvalidArgument(format!("torrent parse error: {error}"))
    })?;
    let info_hash = parsed.info_hash.as_string();
    let info_hash_v2 = parsed
        .info_hash_v2
        .map(|hash| hash.as_string())
        .unwrap_or_default();
    let has_v1 = parsed
        .info
        .data
        .version()
        .map_or(true, librqbit::TorrentVersion::has_v1);
    let name = parsed
        .info
        .data
        .name
        .as_ref()
        .map(|name| String::from_utf8_lossy(name.as_ref()).into_owned())
        .unwrap_or_default();
    let mut xt = Vec::with_capacity(2);
    if has_v1 || info_hash_v2.is_empty() {
        xt.push(format!("xt=urn:btih:{info_hash}"));
    }
    if !info_hash_v2.is_empty() {
        xt.push(format!("xt=urn:btmh:1220{info_hash_v2}"));
    }
    let mut magnet = format!("magnet:?{}", xt.join("&"));
    if !name.is_empty() {
        magnet.push_str("&dn=");
        magnet.extend(url::form_urlencoded::byte_serialize(name.as_bytes()));
    }
    let own: Vec<String> = parsed
        .iter_announce()
        .map(|tracker| String::from_utf8_lossy(tracker.as_ref()).into_owned())
        .filter(|tracker| !tracker.is_empty())
        .collect();
    let magnet = crate::bt_downloader::magnet_with_trackers(&magnet, &own);
    let magnet = crate::bt_downloader::magnet_with_trackers(&magnet, extra_trackers);
    Ok(TorrentShare {
        info_hash,
        info_hash_v2,
        name,
        magnet,
    })
}

/// 生成的种子格式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TorrentFormat {
    /// BEP 3（兼容性最好）。
    #[default]
    V1,
    /// 只有 BEP 52 v2（逐文件 merkle 树，老客户端不认）。
    V2,
    /// v1 + v2 混合：两代客户端都能下载，文件按分片对齐（插入 padding 文件）。
    Hybrid,
}

impl TorrentFormat {
    fn to_librqbit(self) -> librqbit::TorrentVersion {
        match self {
            Self::V1 => librqbit::TorrentVersion::V1,
            Self::V2 => librqbit::TorrentVersion::V2,
            Self::Hybrid => librqbit::TorrentVersion::Hybrid,
        }
    }
}

/// 生成种子的选项（对齐 qBittorrent「制作 Torrent」）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TorrentBuildOptions {
    /// tracker，按顺序；空串分隔层（BEP 12，同 qBittorrent 的空行分层），不含空串即同一层。
    pub trackers: Vec<String>,
    /// 种子格式。
    pub version: TorrentFormat,
    /// 私有种子（BEP 27）：只经种子里的 tracker 找对端。
    pub private: bool,
    /// 分片大小（字节，2 的幂，16 KiB..=128 MiB）；`None` = 按内容大小自动选择。
    pub piece_length: Option<u32>,
    /// HTTP 源（BEP 19 `url-list`）。
    pub web_seeds: Vec<String>,
    /// 注释；空 = 不写。
    pub comment: String,
    /// info 字典里的来源标记（改变 info hash）；空 = 不写。
    pub source: String,
    /// `created by`；空 = 不写。
    pub created_by: String,
}

/// [`TorrentBuildOptions::validate`] 拒绝的选项。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidBuildOption {
    /// 分片大小不是 16 KiB..=128 MiB 之间的 2 的幂。
    #[error("piece length {0} must be a power of two from 16 KiB to 128 MiB")]
    PieceLength(u32),
    /// HTTP 源不是带主机名的 http / https 地址，或超过 4096 字节。
    #[error("web seed is not an http(s) URL: {0}")]
    WebSeed(String),
    /// 去重后的 HTTP 源超过 64 个。
    #[error("at most {MAX_WEB_SEEDS} web seeds are allowed")]
    TooManyWebSeeds,
}

/// 一个种子最多写入的 HTTP 源（与 librqbit 相同）。
const MAX_WEB_SEEDS: usize = 64;
/// HTTP 源地址的最大字节数（与 librqbit 相同）。
const MAX_WEB_SEED_URL_LEN: usize = 4096;
/// 分片大小下限 / 上限（BEP 52 块大小 / libtorrent 上限）。
const PIECE_LENGTH_RANGE: std::ops::RangeInclusive<u32> = 16 * 1024..=128 * 1024 * 1024;

impl TorrentBuildOptions {
    /// 生成前检查选项，让调用方能指出是哪个字段不对（哈希可能要几分钟，不能等到最后才报）。
    /// 空白的 HTTP 源条目忽略。
    ///
    /// ```
    /// use fluxdown_engine::bt_local_seed::{InvalidBuildOption, TorrentBuildOptions};
    /// let mut options = TorrentBuildOptions {
    ///     piece_length: Some(1 << 20),
    ///     web_seeds: vec!["https://mirror.example/a/".into(), " ".into()],
    ///     ..Default::default()
    /// };
    /// assert_eq!(options.validate(), Ok(()));
    /// options.piece_length = Some(3 << 20);
    /// assert_eq!(options.validate(), Err(InvalidBuildOption::PieceLength(3 << 20)));
    /// options.piece_length = None;
    /// options.web_seeds.push("ftp://mirror.example/a".into());
    /// assert!(matches!(options.validate(), Err(InvalidBuildOption::WebSeed(_))));
    /// ```
    ///
    /// # Errors
    /// 第一个不合法的选项。
    pub fn validate(&self) -> Result<(), InvalidBuildOption> {
        if let Some(len) = self.piece_length
            && !(len.is_power_of_two() && PIECE_LENGTH_RANGE.contains(&len))
        {
            return Err(InvalidBuildOption::PieceLength(len));
        }
        let mut seeds: Vec<&str> = Vec::new();
        for seed in self.web_seeds.iter().map(|seed| seed.trim()) {
            if seed.is_empty() || seeds.contains(&seed) {
                continue;
            }
            let usable = seed.len() <= MAX_WEB_SEED_URL_LEN
                && url::Url::parse(seed).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host().is_some()
                });
            if !usable {
                return Err(InvalidBuildOption::WebSeed(seed.to_owned()));
            }
            seeds.push(seed);
        }
        if seeds.len() > MAX_WEB_SEEDS {
            return Err(InvalidBuildOption::TooManyWebSeeds);
        }
        Ok(())
    }
}

/// 按空串把 tracker 列表切成层；去掉首尾空白，丢弃空层。
fn tracker_tiers(trackers: &[String]) -> Vec<Vec<String>> {
    let mut tiers: Vec<Vec<String>> = vec![Vec::new()];
    for tracker in trackers.iter().map(|tracker| tracker.trim()) {
        if tracker.is_empty() {
            if tiers.last().is_some_and(|tier| !tier.is_empty()) {
                tiers.push(Vec::new());
            }
        } else if let Some(tier) = tiers.last_mut() {
            tier.push(tracker.to_owned());
        }
    }
    tiers.retain(|tier| !tier.is_empty());
    tiers
}

/// 为 `path`（文件或文件夹）生成 `.torrent` 字节。没有 tracker 即无 tracker 种子
/// （靠 DHT / PEX 发现）。哈希在阻塞线程上做，耗时与数据量成正比，不得在 actor 上等待。
/// 创建时间写当前时间（qBittorrent / libtorrent 的默认）。
///
/// # Errors
/// 路径不存在 / 非绝对路径、选项不合法、读盘或编码失败。
pub async fn build_torrent(
    path: &Path,
    options: &TorrentBuildOptions,
) -> Result<Vec<u8>, LocalSeedError> {
    ensure_content_path(path)?;
    options
        .validate()
        .map_err(|error| LocalSeedError::InvalidArgument(error.to_string()))?;
    let (_, name) = split_content_path(path)?;
    let creation_date = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok());
    let created = librqbit::create_torrent(
        path,
        librqbit::CreateTorrentOptions {
            name: Some(&name),
            tracker_tiers: tracker_tiers(&options.trackers),
            piece_length: options.piece_length,
            version: options.version.to_librqbit(),
            private: options.private,
            web_seeds: options
                .web_seeds
                .iter()
                .map(|seed| seed.trim())
                .filter(|seed| !seed.is_empty())
                .map(str::to_owned)
                .collect(),
            source: non_blank(&options.source),
            comment: non_blank(&options.comment),
            created_by: Some(options.created_by.trim()),
            creation_date,
            ..Default::default()
        },
        &librqbit::spawn_utils::BlockingSpawner::new(1),
    )
    .await
    .map_err(|error| LocalSeedError::Operation(format!("create torrent: {error:#}")))?;
    created
        .as_bytes()
        .map(|bytes| bytes.to_vec())
        .map_err(|error| LocalSeedError::Operation(format!("encode torrent: {error:#}")))
}

/// 去掉首尾空白后非空的文本。
fn non_blank(value: &str) -> Option<&str> {
    Some(value.trim()).filter(|value| !value.is_empty())
}

/// 校验 `torrent_bytes` 与内容路径 `path` 形态匹配并产出落库计划。
///
/// 文件夹可对应任意多文件种子；单个文件必须对应「只含该文件、名字相同」的单文件种子，
/// 否则续种校验必然失败，不如在这里就给出明确原因。
///
/// # Errors
/// 路径不可用、种子无法解析、种子与路径不匹配。
pub fn plan_local_seed(
    path: &Path,
    torrent_bytes: Vec<u8>,
) -> Result<LocalSeedPlan, LocalSeedError> {
    ensure_content_path(path)?;
    let (save_dir, file_name) = split_content_path(path)?;
    let meta = crate::bt_downloader::probe_torrent_meta(String::new(), torrent_bytes.clone());
    if !meta.error.is_empty() {
        return Err(LocalSeedError::InvalidArgument(meta.error));
    }
    if path.is_file() {
        let single = meta.files.len() == 1
            && meta.name == file_name
            && meta
                .files
                .first()
                .is_some_and(|file| file.path == file_name);
        if !single {
            return Err(LocalSeedError::InvalidArgument(
                "the torrent does not describe the selected file".to_owned(),
            ));
        }
    } else if meta.files.is_empty() {
        return Err(LocalSeedError::InvalidArgument(
            "the torrent contains no files".to_owned(),
        ));
    }
    Ok(LocalSeedPlan {
        torrent_bytes,
        save_dir,
        file_name,
        total_bytes: meta.total_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-local-seed-{tag}-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn built_torrent_matches_its_own_file_and_folder() {
        let root = temp_dir("plan");
        let file = root.join("one.bin");
        std::fs::write(&file, vec![7u8; 100_000]).expect("file");
        let folder = root.join("pack");
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join("a.bin"), vec![1u8; 40_000]).expect("a");
        std::fs::write(folder.join("b.bin"), vec![2u8; 40_000]).expect("b");

        let bytes = build_torrent(&file, &TorrentBuildOptions::default())
            .await
            .expect("file torrent");
        let plan = plan_local_seed(&file, bytes).expect("file plan");
        assert_eq!(plan.file_name, "one.bin");
        assert_eq!(plan.total_bytes, 100_000);
        assert_eq!(plan.save_dir, root.to_string_lossy());

        let bytes = build_torrent(&folder, &with_trackers(&["udp://t.example:80/announce"]))
            .await
            .expect("folder torrent");
        let plan = plan_local_seed(&folder, bytes.clone()).expect("folder plan");
        assert_eq!(plan.file_name, "pack");
        assert_eq!(plan.total_bytes, 80_000);

        // 多文件种子不能配到单个文件上；单文件种子配到别名文件同样拒绝。
        assert!(matches!(
            plan_local_seed(&file, bytes),
            Err(LocalSeedError::InvalidArgument(_))
        ));
        let other = root.join("other.bin");
        std::fs::write(&other, vec![7u8; 100_000]).expect("other");
        let single = build_torrent(&file, &TorrentBuildOptions::default())
            .await
            .expect("torrent");
        assert!(matches!(
            plan_local_seed(&other, single),
            Err(LocalSeedError::InvalidArgument(_))
        ));
        assert!(matches!(
            plan_local_seed(&root.join("missing"), Vec::new()),
            Err(LocalSeedError::InvalidArgument(_))
        ));
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn share_magnet_carries_hash_name_own_and_extra_trackers_once() {
        let root = temp_dir("share");
        let file = root.join("my file.bin");
        std::fs::write(&file, vec![9u8; 50_000]).expect("file");
        let own = "udp://own.example:6969/announce".to_owned();
        let bytes = build_torrent(&file, &with_trackers(&[own.as_str()]))
            .await
            .expect("torrent");
        let extra = vec![own.clone(), "https://extra.example/announce".to_owned()];
        let share = torrent_share(&bytes, &extra).expect("share");

        let magnet = librqbit::Magnet::parse(&share.magnet).expect("parsable magnet");
        assert_eq!(
            magnet.as_id20().map(|id| id.as_string()),
            Some(share.info_hash.clone())
        );
        assert_eq!(share.name, "my file.bin");
        // 自带 tracker 与追加 tracker 合并且不重复。
        assert_eq!(
            magnet.trackers,
            vec![own, "https://extra.example/announce".to_owned()]
        );
        assert!(torrent_share(b"nope", &[]).is_err());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    fn with_trackers(trackers: &[&str]) -> TorrentBuildOptions {
        TorrentBuildOptions {
            trackers: trackers.iter().map(|&tracker| tracker.to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn blank_entries_separate_tracker_tiers() {
        let lines = |lines: &[&str]| {
            lines
                .iter()
                .map(|&line| line.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tracker_tiers(&lines(&[
                "",
                " udp://a ",
                "udp://b",
                "",
                "",
                "http://c",
                "  "
            ])),
            vec![
                vec!["udp://a".to_owned(), "udp://b".to_owned()],
                vec!["http://c".to_owned()],
            ]
        );
        assert!(tracker_tiers(&lines(&["", "  "])).is_empty());
    }

    /// 每种格式生成的种子：选项原样写进种子，分享磁力的 `xt` 随版本（v1 = btih，
    /// 纯 v2 = btmh，混合 = 两个），且能用来本机做种（计划通过）。
    #[tokio::test(flavor = "multi_thread")]
    async fn built_torrents_carry_the_options_in_every_format() {
        let root = temp_dir("formats");
        let folder = root.join("pack");
        std::fs::create_dir_all(folder.join("sub")).expect("folder");
        std::fs::write(folder.join("a.bin"), vec![1u8; 40_000]).expect("a");
        std::fs::write(folder.join("sub/b.bin"), vec![2u8; 70_000]).expect("b");
        for format in [TorrentFormat::V1, TorrentFormat::V2, TorrentFormat::Hybrid] {
            let version = format.to_librqbit();
            let options = TorrentBuildOptions {
                trackers: ["udp://t1/announce", "", "udp://t2/announce"]
                    .map(str::to_owned)
                    .to_vec(),
                version: format,
                private: true,
                piece_length: Some(16 * 1024),
                web_seeds: vec!["https://mirror.example/files/".to_owned()],
                comment: "说明".to_owned(),
                source: "SITE".to_owned(),
                created_by: "FluxDown test".to_owned(),
            };
            let bytes = build_torrent(&folder, &options).await.expect("torrent");
            let parsed = librqbit::torrent_from_bytes(&bytes).expect("parse");
            let info = &parsed.info.data;
            assert_eq!(info.version().expect("version"), version);
            assert!(info.private);
            assert_eq!(info.piece_length, 16 * 1024);
            assert_eq!(
                info.source.as_ref().map(|source| source.as_ref()),
                Some(&b"SITE"[..])
            );
            assert_eq!(
                parsed.comment.as_ref().map(|comment| comment.as_ref()),
                Some("说明".as_bytes())
            );
            assert_eq!(
                parsed.created_by.as_ref().map(|by| by.as_ref()),
                Some(&b"FluxDown test"[..])
            );
            assert!(parsed.creation_date.is_some());
            assert_eq!(parsed.url_list, options.web_seeds);
            let tiers: Vec<Vec<String>> = parsed
                .announce_tiers()
                .into_iter()
                .map(|tier| {
                    tier.into_iter()
                        .map(|url| String::from_utf8_lossy(url.as_ref()).into_owned())
                        .collect()
                })
                .collect();
            assert_eq!(
                tiers,
                vec![
                    vec!["udp://t1/announce".to_owned()],
                    vec!["udp://t2/announce".to_owned()],
                ]
            );

            let share = torrent_share(&bytes, &[]).expect("share");
            let magnet = librqbit::Magnet::parse(&share.magnet).expect("parsable magnet");
            assert_eq!(
                magnet.as_id20().map(|id| id.as_string()),
                version.has_v1().then(|| share.info_hash.clone()),
                "{version:?}"
            );
            assert_eq!(
                magnet.as_id32().map(|id| id.as_string()),
                version.has_v2().then(|| share.info_hash_v2.clone()),
                "{version:?}"
            );
            assert_eq!(share.info_hash_v2.is_empty(), !version.has_v2());
            let plan = plan_local_seed(&folder, bytes).expect("plan");
            assert_eq!(plan.total_bytes, 110_000);
        }
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
