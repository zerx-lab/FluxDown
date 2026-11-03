//! 分类保存目录路由（外部捕获的「跟随自定义分类」）。
//!
//! 宿主（daemon）对未替用户选定保存目录的建任务请求附带路由规则。引擎在三个时刻按同一规则
//! 定落点，中间过程（`.fdownloading` 临时文件、HLS 顺序写入、DASH / 轨对的 sidecar 与
//! ffmpeg 转封装）一律留在当时的目录，不受影响：
//!
//! 1. 建任务：按初始名（显式名 / URL 末段；解析插件任务只看显式名）选初始目录；
//! 2. 开始写盘前（启动序幕定名之后、「文件已存在」判断与去重之前）：按探测得到的服务器
//!    文件名、按类型推断的名字、插件 resolve 改的名字重选目录，只改任务记录、不移动文件，
//!    多数「名字到开始下载才知道」的任务（脚本地址、查询参数里的类型）因此直接写进分类目录；
//! 3. 完成前（插件 `finalize` 之后、写状态 3 之前）：按最终文件名复核——跳过探测的任务、
//!    按实际响应精修的名、插件替换的产物、HLS 转封装失败留下的 `.ts` 都在此时才确定——
//!    命中另一个分类目录时把产物移过去（同卷 rename，跨卷可中断复制后删除源）。
//!
//! HLS / DASH 的成品是 MP4，前两步按 `<stem>.mp4` 预判（[`output_name`]）。
//!
//! 归档插件（`contributes.archive`）：第 2、3 步选中分类目录（命中的分类或「其他」兜底）时，
//! 按同一个成品名询问插件，合法的单层子目录名叠加在分类目录之下并写回任务路由
//! （[`CategoryRoute::archive`]）；之后续传、重启与完成收尾都沿用，插件不再参与。只改目录、
//! 不改文件名；插件失败只记任务活动，文件留在分类目录（`archive_target`）。
//!
//! 不参与路由：BT / eD2K（做种与共享要求文件原地不动）、任务组成员（组以基准目录为根，成员
//! 保持组内结构）、带子路径的单条目清单与 `out=子目录/名字`（相对结构由来源决定）。
//!
//! 匹配语义与 agent `category_dir.rs`（确认表单预填）逐条对齐，两端共用夹具
//! `fixtures/category_route_cases.json`。`url` 模式按通配符（`*` 任意串、`?` 单字符、
//! 不分大小写、锚定完整匹配整个网址）匹配来源网址；网址规则固定优先于扩展名 / 正则
//! 分类（不看列表位置），网址规则之间与文件名规则之间各自按列表顺序取首条命中。
//!
//! Signed rule packs can additionally match a bounded local file header at completion.

use std::path::Path;

use serde::{Deserialize, Serialize};

use tokio_util::sync::CancellationToken;

use crate::downloader::{DownloadError, DownloadParams, claim_final_name};
use crate::events::EngineEvent;
use crate::logger::{log_info, log_warn};

/// 任务的分类目录路由（持久化于 `tasks.category_route`，JSON；空串 = 不路由）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CategoryRoute {
    /// 路由未命中时的落点：建任务时解析出的保存目录。任务组与带子路径的清单条目以它为根。
    pub base_dir: String,
    /// 可见普通分类，按显示顺序。
    pub rules: Vec<CategoryRule>,
    /// 「其他」分类的目录；空 = 不兜底。
    pub other_dir: String,
    /// 归档插件选定的子目录；选定后续传、重启与完成收尾都沿用，不再询问插件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveRoute>,
    /// Source metadata is persisted alongside the authorized route so resume does not
    /// need to rerun the resolver or infer a title from a member filename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_metadata: Option<crate::model::ArchiveMetadata>,
}

/// Persisted archive destination chosen for a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveRoute {
    /// Category-root target used by existing `folderName` callbacks.
    pub category_dir: String,
    /// Legacy single-component result, retained for existing plugins.
    pub folder_name: String,
    /// New nested destination relative to the authorized task base directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<String>,
    /// Authorized task base directory for modern `directories` results.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_dir: String,
}

/// [`CategoryRoute::rules`] 的单条普通分类。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CategoryRule {
    /// `regex` = 按正则匹配文件名；`url` = 按通配符匹配来源网址；其余按扩展名。
    pub match_mode: String,
    pub extensions: Vec<String>,
    pub regex_pattern: String,
    /// `url` 模式的通配符模式（[`wildcard_matches`]）。
    pub url_pattern: String,
    /// 空 = 不改变落点，只阻止「其他」兜底。
    pub save_dir: String,
}

impl CategoryRoute {
    /// 文件名命中的分类目录；未命中 / 命中分类未配置目录时为 `None`（留在当前目录）。
    ///
    /// 网址规则固定优先（不看列表位置）：先按列表顺序取首个命中且配置了目录的 `url`
    /// 规则，命中即定落点；扩展名 / 正则分类只处理未被网址规则认领的任务。文件名不含
    /// `.` 时用 `url` 路径末段（百分号解码后含 `.`）匹配；`source` 是网址规则匹配的来源
    /// 地址（`origin_url` 非空取它，否则取任务链接），与文件名推断用的 `url` 分开；文件名为空时扩展名 / 正则规则
    /// 不命中。文件名规则按顺序取首个配置了目录且命中的普通分类；所有普通分类都不
    /// 命中时取「其他」的目录。
    ///
    /// # Examples
    ///
    /// ```
    /// use fluxdown_engine::category_route::{CategoryRoute, CategoryRule};
    ///
    /// let route = CategoryRoute {
    ///     rules: vec![CategoryRule {
    ///         match_mode: "extension".into(),
    ///         extensions: vec!["jpg".into()],
    ///         save_dir: "/pictures".into(),
    ///         ..Default::default()
    ///     }],
    ///     ..Default::default()
    /// };
    /// assert_eq!(route.save_dir_for("a.JPG", "", ""), Some("/pictures"));
    /// assert_eq!(route.save_dir_for("a.zip", "", ""), None);
    /// ```
    #[must_use]
    pub fn save_dir_for(&self, file_name: &str, url: &str, source: &str) -> Option<&str> {
        // 网址规则固定优先（不看列表位置）：先按列表顺序取首个命中且配置了目录的网址
        // 规则，命中即定落点；扩展名 / 正则分类只处理未被网址规则认领的任务。
        if let Some(rule) = self.rules.iter().find(|rule| {
            rule.match_mode == "url"
                && !rule.save_dir.is_empty()
                && wildcard_matches(&rule.url_pattern, source)
        }) {
            return Some(rule.save_dir.as_str());
        }
        let url_name;
        let name = if file_name.contains('.') {
            file_name
        } else {
            url_name = file_name_from_url(url);
            url_name.as_deref().unwrap_or(file_name)
        };
        // 文件名未知时只有网址规则参与：扩展名 / 正则与「其他」兜底保持原语义（不路由）。
        if name.is_empty() {
            return None;
        }
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| !rule.save_dir.is_empty() && rule.matches(name, source))
        {
            return Some(rule.save_dir.as_str());
        }
        (!self.other_dir.is_empty() && !self.rules.iter().any(|rule| rule.matches(name, source)))
            .then_some(self.other_dir.as_str())
    }

    /// 解析 `tasks.category_route`；空串为 `None`，损坏的值记日志后按不路由处理。
    pub(crate) fn from_column(task_id: &str, text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        match serde_json::from_str::<CategoryRoute>(text) {
            Ok(mut route) => {
                if route.archive.as_ref().is_some_and(|archive| {
                    if archive.directories.is_empty() {
                        !valid_archive_folder(&archive.folder_name)
                    } else {
                        !valid_archive_directories(&archive.directories)
                            || archive.base_dir != route.base_dir
                    }
                }) {
                    log_warn!(
                        "[category-route] task {task_id}: ignoring unsafe stored archive destination"
                    );
                    route.archive = None;
                }
                Some(route)
            }
            Err(error) => {
                log_warn!("[category-route] task {task_id}: ignoring malformed route: {error}");
                None
            }
        }
    }

    /// 编码为 `tasks.category_route` 列值。
    pub(crate) fn to_column(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
/// Persist source metadata in the existing route JSON, including route-less tasks.
/// A route-less JSON object has an empty base directory and therefore cannot redirect.
#[cfg(feature = "plugins")]
pub(crate) async fn persist_archive_metadata(
    db: &crate::db::Db,
    task_id: &str,
    metadata: &crate::model::ArchiveMetadata,
) -> Result<(), crate::db::DbError> {
    let column = db.load_task_category_route(task_id).await?;
    let mut route = match CategoryRoute::from_column(task_id, &column) {
        Some(route) => route,
        None if column.is_empty() => CategoryRoute::default(),
        None => return Ok(()),
    };
    route.archive_metadata = Some(metadata.clone());
    let column = route.to_column().map_err(|error| {
        crate::db::DbError::InvalidConfig(format!("category route serialization failed: {error}"))
    })?;
    db.set_task_category_route(task_id, &column).await
}

/// Route a complete group at one root. Modern callback destinations use the authorized fallback
/// directory regardless of category matches; legacy folderName results keep category-child behavior.
pub(crate) async fn archive_group_root(
    fallback: &str,
    group_name: &str,
    item_names: &[(&str, &str)],
    source_url: &str,
    metadata: Option<&crate::model::ArchiveMetadata>,
    route: &CategoryRoute,
    interceptor: &dyn crate::intercept::TaskInterceptor,
) -> String {
    let mut categories = item_names
        .iter()
        .map(|(name, url)| route.save_dir_for(name, url, source_url).map(str::to_owned));
    let category_dir = categories
        .next()
        .flatten()
        .filter(|dir| categories.all(|category| category.as_deref() == Some(dir.as_str())));
    let result = if interceptor.has_archivers() {
        match interceptor
            .archive_directories(crate::model::ArchiveContext {
                file_name: group_name.to_owned(),
                metadata: metadata.cloned(),
                is_group: true,
                header_hex: None,
            })
            .await
        {
            Ok(Some(result)) if valid_archive_directories(&result.components) => Some(result),
            Ok(Some(_)) => {
                log_warn!("[category-route] group archive plugin returned unsafe directories");
                None
            }
            Ok(None) => None,
            Err(error) => {
                log_warn!("[category-route] group archive plugin failed open: {error}");
                None
            }
        }
    } else {
        None
    };
    if let Some(result) = result
        && (!result.legacy || category_dir.is_some())
    {
        let base = if result.legacy {
            category_dir.as_deref().unwrap_or(fallback)
        } else if route.base_dir.is_empty() {
            fallback
        } else {
            &route.base_dir
        };
        let mut components = result.components;
        if result.legacy {
            let clean_group_name = if group_name.is_empty() {
                String::new()
            } else {
                crate::naming::sanitize_filename(group_name)
            };
            if !clean_group_name.is_empty()
                && !clean_group_name.eq_ignore_ascii_case(&components[0])
            {
                components.push(clean_group_name);
            }
        }
        if valid_archive_directories(&components) {
            let archive = ArchiveRoute {
                category_dir: base.to_owned(),
                folder_name: String::new(),
                directories: components,
                base_dir: base.to_owned(),
            };
            match ensure_archive_target(&archive).await {
                Ok(target) => return target,
                Err(error) => {
                    log_warn!("[category-route] group archive destination rejected: {error:#}")
                }
            }
        }
    }
    category_dir.map_or_else(
        || fallback.to_owned(),
        |directory| {
            if group_name.is_empty() {
                directory
            } else {
                Path::new(&directory)
                    .join(crate::naming::sanitize_filename(group_name))
                    .to_string_lossy()
                    .into_owned()
            }
        },
    )
}

impl CategoryRule {
    /// `extension` / `regex` 匹配文件名，`url` 匹配来源网址；规则按顺序求值，
    /// 同一规则只按自己的模式参与。
    fn matches(&self, name: &str, url: &str) -> bool {
        match self.match_mode.as_str() {
            "url" => wildcard_matches(&self.url_pattern, url),
            "regex" => {
                !self.regex_pattern.is_empty()
                    && regex::RegexBuilder::new(&self.regex_pattern)
                        .case_insensitive(true)
                        .build()
                        .is_ok_and(|regex| regex.is_match(name))
            }
            _ => {
                let Some((_, extension)) = name.rsplit_once('.') else {
                    return false;
                };
                !extension.is_empty()
                    && self.extensions.iter().any(|candidate| {
                        candidate
                            .trim_start_matches('.')
                            .eq_ignore_ascii_case(extension)
                    })
            }
        }
    }
}

/// 通配符网址匹配：`*` 匹配任意长度任意字符、`?` 匹配恰好一个字符，其余字面；不分
/// 大小写；模式须完整匹配整个网址（锚定，非子串）。空 / 非法模式不命中。与正则模式
/// 一致即时编译；agent `category_dir.rs` 与 Web `categoryRoute.ts` 镜像同一语义。
#[must_use]
pub fn wildcard_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let mut source = String::with_capacity(pattern.len() + 2);
    source.push('^');
    for ch in pattern.chars() {
        match ch {
            '*' => source.push_str(".*"),
            '?' => source.push('.'),
            other => source.push_str(&regex::escape(&other.to_string())),
        }
    }
    source.push('$');
    regex::RegexBuilder::new(&source)
        .case_insensitive(true)
        .build()
        .is_ok_and(|regex| regex.is_match(url))
}

/// 网址规则匹配的来源地址：捕获来源（`origin_url`，如 thunder 原链接）非空优先，否则任务链接。
/// 建任务、开始写盘前、完成前三个时刻必须用同一口径，落点才不会来回跳。
pub(crate) fn source_address<'a>(origin_url: &'a str, url: &'a str) -> &'a str {
    if origin_url.is_empty() {
        url
    } else {
        origin_url
    }
}

/// URL 路径末段（百分号解码）且含 `.` 时作为文件名。
fn file_name_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let decoded = percent_decode(last);
    decoded.contains('.').then_some(decoded)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(byte) = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 完成前按最终文件名复核落点（见模块文档第 2 步）；产物已移入分类目录且文件名因目标目录
/// 占名而改变时返回 `(新文件名, 大小)`，供完成帧上报。任何失败都只记日志与任务活动、产物
/// 留在原目录，绝不让已下载完成的任务失败。
pub(crate) async fn relocate_completed(p: &DownloadParams) -> Option<(String, i64)> {
    let mut route = match p.db.load_task_category_route(&p.task_id).await {
        Ok(text) => CategoryRoute::from_column(&p.task_id, &text)?,
        Err(error) => {
            crate::logger::report_warning("category-route", "load task route", &error);
            return None;
        }
    };
    if route.base_dir.is_empty() {
        return None;
    }
    let row = match p.db.load_task_by_id(&p.task_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(error) => {
            crate::logger::report_warning("category-route", "load completed task", &error);
            return None;
        }
    };
    if !row.group_id.is_empty() {
        return None;
    }
    let source = source_address(&row.origin_url, &row.url);
    let category_dir = route
        .save_dir_for(&row.file_name, &row.url, source)
        .map(str::to_owned);
    let notes = crate::intercept::ActivityNotes::of(p);
    let mut target = category_dir
        .clone()
        .unwrap_or_else(|| route.base_dir.clone());
    let mut route_changed = false;
    if let Some(mut archive) = route.archive.clone() {
        if archive.directories.is_empty()
            && let Some(category_dir) = category_dir.as_ref()
            && archive.category_dir != *category_dir
        {
            archive.category_dir.clone_from(category_dir);
            route_changed = true;
        }
        target = archive_target(&archive);
        route.archive = Some(archive);
    } else if p.interceptor.has_archivers() {
        match p
            .interceptor
            .archive_directories(crate::model::ArchiveContext {
                file_name: row.file_name.clone(),
                metadata: route.archive_metadata.clone(),
                is_group: false,
                header_hex: match read_archive_header_hex(
                    &Path::new(&row.save_dir).join(&row.file_name),
                )
                .await
                {
                    Ok(header) => header,
                    Err(error) => {
                        crate::logger::report_warning(
                            "category-route",
                            "read archive classification header",
                            &error,
                        );
                        None
                    }
                },
            })
            .await
        {
            Ok(Some(result))
                if valid_archive_directories(&result.components)
                    && (!result.legacy || category_dir.is_some()) =>
            {
                let modern = !result.legacy;
                let archive = ArchiveRoute {
                    category_dir: category_dir
                        .clone()
                        .unwrap_or_else(|| route.base_dir.clone()),
                    folder_name: if modern {
                        String::new()
                    } else {
                        result.components[0].clone()
                    },
                    directories: if modern {
                        result.components
                    } else {
                        Vec::new()
                    },
                    base_dir: if modern {
                        route.base_dir.clone()
                    } else {
                        String::new()
                    },
                };
                target = archive_target(&archive);
                route.archive = Some(archive);
                route_changed = true;
            }
            Ok(Some(_)) => {
                notes.add("忽略不安全或不适用的归档目录".to_owned()).await;
            }
            Ok(None) => {}
            Err(error) => notes.add(format!("归档插件未能识别文件名：{error}")).await,
        }
    }
    if route_changed && let Err(error) = persist_route(p, &route).await {
        crate::logger::report_error("category-route", "persist archive route", &error);
        return None;
    }
    if let Some(archive) = route.archive.as_ref() {
        match ensure_archive_target(archive).await {
            Ok(verified) => target = verified,
            Err(error) => {
                log_warn!(
                    "[category-route] task {} rejected archive destination: {error}",
                    p.task_id
                );
                return None;
            }
        }
    }
    if same_dir(&target, &row.save_dir).await {
        return None;
    }
    let src = Path::new(&row.save_dir).join(&row.file_name);
    let notes = crate::intercept::ActivityNotes::of(p);
    // mux 降级留下独立音轨（DASH / HLS 音频组 / 轨对）时产物是两个文件：不拆散，整体
    // 留在原目录。无法确认是否存在时同样保守处理。
    let sidecar = crate::dash_downloader::build_audio_path(&src);
    if tokio::fs::try_exists(&sidecar).await.unwrap_or(true) {
        notes
            .add("存在独立音轨文件，未移入分类目录".to_string())
            .await;
        return None;
    }
    let target_dir = Path::new(&target);
    // 全局「覆盖」对目标目录同样生效；逐任务覆盖答复只绑定询问时的目录与名字，不外推。
    let overwrite = match p.overwrite {
        crate::file_exists::OverwritePolicy::Any => crate::file_exists::OverwritePolicy::Any,
        _ => crate::file_exists::OverwritePolicy::Never,
    };
    let chosen = match move_into(
        &src,
        target_dir,
        &row.file_name,
        &overwrite,
        &p.cancel_token,
    )
    .await
    {
        Ok(chosen) => chosen,
        Err(error) => {
            log_warn!(
                "[category-route] task {} could not move into {}: {error:#}",
                p.task_id,
                target_dir.display()
            );
            notes
                .add(format!(
                    "未能移入分类目录 {target}（{error}），文件保留在原目录"
                ))
                .await;
            return None;
        }
    };
    let moved = target_dir.join(&chosen);
    if let Err(error) = p.db.set_task_location(&p.task_id, &target, &chosen).await {
        crate::logger::report_error("category-route", "persist relocated task", &error);
        // 库里仍是旧位置：把文件移回，保证任务记录与磁盘一致。
        let never = crate::file_exists::OverwritePolicy::Never;
        let uncancellable = CancellationToken::new();
        let restore = move_into(
            &moved,
            Path::new(&row.save_dir),
            &row.file_name,
            &never,
            &uncancellable,
        );
        if let Err(back) = restore.await {
            crate::logger::report_error("category-route", "restore relocated file", &back);
        }
        return None;
    }
    log_info!(
        "[category-route] task {} moved into category dir {}",
        p.task_id,
        target_dir.display()
    );
    notes.add(format!("已按分类移至 {target}")).await;
    publish_location(p).await;
    let size = match tokio::fs::metadata(&moved).await {
        Ok(meta) => i64::try_from(meta.len()).unwrap_or(i64::MAX),
        Err(error) => {
            crate::logger::report_warning("category-route", "stat relocated file", &error);
            row.total_bytes
        }
    };
    (chosen != row.file_name).then_some((chosen, size))
}

/// 新位置随完整任务事实广播（进度帧不携带保存目录）；与整表快照互斥，见 `Db::lock_event_order`。
async fn publish_location(p: &DownloadParams) {
    let _order = p.db.lock_event_order().await;
    match p.db.load_task_by_id(&p.task_id).await {
        Ok(Some(task)) => p.sink.emit(EngineEvent::TaskChanged(Box::new(task))),
        Ok(None) => {}
        Err(error) => {
            crate::logger::report_warning("category-route", "load relocated task", &error)
        }
    }
}

/// 分类判断用的成品名：HLS 一律转封装为 MP4，DASH 的空名 / `.mpd` 名合流为 MP4（与启动
/// 序幕的命名归一化一致），按 `<stem>.mp4` 预判；其余返回 `None`（用原名）。
pub(crate) fn output_name(file_name: &str, url: &str) -> Option<String> {
    let hls = crate::hls_downloader::is_hls_url(url);
    let dash = crate::dash_downloader::is_dash_url(url)
        && (file_name.is_empty() || file_name.to_ascii_lowercase().ends_with(".mpd"));
    if !hls && !dash {
        return None;
    }
    let base = if file_name.is_empty() {
        crate::naming::extract_from_url(url)?
    } else {
        file_name.to_owned()
    };
    let stem = Path::new(&base).file_stem()?.to_str()?;
    Some(format!("{stem}.mp4"))
}

/// 开始写盘前按当前最佳文件名重选目录（见模块文档）：启动序幕定名（探测得到的服务器
/// 文件名、按类型推断的名字、插件 resolve 改的名字）之后、「文件已存在」判断与去重
/// 之前调用，只改任务记录，不移动任何文件。HLS 按转封装后的 `.mp4` 预判；未命中任何
/// 分类时回到基准目录。路由选中分类目录（命中的分类或「其他」兜底）时经
/// [`archive_target`] 叠加归档插件选定的子目录，并持久化选择以便续传与完成收尾复用。
pub(crate) async fn route_before_start(p: &mut DownloadParams) -> Result<(), crate::db::DbError> {
    if p.file_name.is_empty() {
        return Ok(());
    }
    let column = p.db.load_task_category_route(&p.task_id).await?;
    let Some(mut route) = CategoryRoute::from_column(&p.task_id, &column) else {
        return Ok(());
    };
    if route.base_dir.is_empty() {
        return Ok(());
    }
    let Some(row) = p.db.load_task_by_id(&p.task_id).await? else {
        return Ok(());
    };
    let source = source_address(&row.origin_url, &row.url);
    let output = output_name(&p.file_name, &p.url);
    let name = output.as_deref().unwrap_or(&p.file_name);
    let category_dir = route.save_dir_for(name, &p.url, source).map(str::to_owned);
    let mut target = category_dir
        .clone()
        .unwrap_or_else(|| route.base_dir.clone());
    let mut route_changed = false;
    if let Some(archive) = route.archive.as_mut() {
        if archive.directories.is_empty()
            && let Some(category_dir) = category_dir.as_ref()
            && archive.category_dir != *category_dir
        {
            archive.category_dir.clone_from(category_dir);
            route_changed = true;
        }
        target = archive_target(archive);
    } else if p.interceptor.has_archivers() {
        match p
            .interceptor
            .archive_directories(crate::model::ArchiveContext {
                file_name: name.to_owned(),
                metadata: route.archive_metadata.clone(),
                is_group: !row.group_id.is_empty(),
                header_hex: None,
            })
            .await
        {
            Ok(Some(result))
                if valid_archive_directories(&result.components)
                    && (!result.legacy || category_dir.is_some()) =>
            {
                let modern = !result.legacy;
                let archive = ArchiveRoute {
                    category_dir: category_dir
                        .clone()
                        .unwrap_or_else(|| route.base_dir.clone()),
                    folder_name: if modern {
                        String::new()
                    } else {
                        result.components[0].clone()
                    },
                    directories: if modern {
                        result.components
                    } else {
                        Vec::new()
                    },
                    base_dir: if modern {
                        route.base_dir.clone()
                    } else {
                        String::new()
                    },
                };
                target = archive_target(&archive);
                route.archive = Some(archive);
                route_changed = true;
                crate::intercept::ActivityNotes::of(p)
                    .add(format!("归档插件选定目录「{target}」"))
                    .await;
            }
            Ok(Some(_)) => log_warn!(
                "[category-route] task {} ignored unsafe or inapplicable archive directories",
                p.task_id
            ),
            Ok(None) => {}
            Err(error) => {
                log_warn!(
                    "[category-route] task {} archive plugin failed open: {error}",
                    p.task_id
                );
                crate::intercept::ActivityNotes::of(p)
                    .add(format!("归档插件未选定目录，文件留在原目录（{error}）"))
                    .await;
            }
        }
    }
    if route_changed {
        persist_route(p, &route).await?;
    }
    if let Some(archive) = route.archive.as_ref() {
        match ensure_archive_target(archive).await {
            Ok(verified) => target = verified,
            Err(error) => {
                log_warn!(
                    "[category-route] task {} rejected archive destination: {error}",
                    p.task_id
                );
                return Ok(());
            }
        }
    }
    if same_dir(&target, &p.save_dir).await {
        return Ok(());
    }
    p.db.set_task_location(&p.task_id, &target, &p.file_name)
        .await?;
    log_info!(
        "[category-route] task {} starts in category dir {target}",
        p.task_id
    );
    p.save_dir = target;
    publish_location(p).await;
    Ok(())
}

async fn persist_route(
    p: &DownloadParams,
    route: &CategoryRoute,
) -> Result<(), crate::db::DbError> {
    let column = route.to_column().map_err(|error| {
        crate::db::DbError::InvalidConfig(format!("category route serialization failed: {error}"))
    })?;
    p.db.set_task_category_route(&p.task_id, &column).await
}

/// Normalize new nested output and legacy `folderName` into validated path components.
#[cfg(any(feature = "plugins", test))]
pub(crate) fn normalize_archive_directories(
    directories: &[String],
    legacy_folder: &str,
) -> Option<Vec<String>> {
    if directories.is_empty() {
        return valid_archive_folder(legacy_folder).then(|| vec![legacy_folder.to_owned()]);
    }
    valid_archive_directories(directories).then(|| directories.to_vec())
}

fn valid_archive_directories(directories: &[String]) -> bool {
    !directories.is_empty()
        && directories.len() <= 8
        && directories.iter().all(|part| valid_archive_folder(part))
        && directories.iter().map(String::len).sum::<usize>() + directories.len() - 1 <= 180
}

fn archive_target(archive: &ArchiveRoute) -> String {
    let (base, parts): (&str, &[String]) = if !archive.directories.is_empty() {
        (&archive.base_dir, &archive.directories)
    } else {
        (
            &archive.category_dir,
            std::slice::from_ref(&archive.folder_name),
        )
    };
    parts
        .iter()
        .fold(Path::new(base).to_path_buf(), |path, part| path.join(part))
        .to_string_lossy()
        .into_owned()
}
pub(crate) fn valid_archive_folder(folder: &str) -> bool {
    !folder.is_empty()
        && folder.len() <= 180
        && folder != "."
        && folder != ".."
        && !folder.contains(['/', '\\', ':'])
        && !folder.chars().any(char::is_control)
        && crate::naming::sanitize_filename(folder) == folder
}

/// 两个目录是否同一处：字面相同，或都存在且规范化后相同（大小写不敏感的文件系统、
/// 符号链接、`..`）。同一目录内「移动」会被占名协议当成冲突而编号改名，必须先排除。
async fn same_dir(a: &str, b: &str) -> bool {
    if Path::new(a) == Path::new(b) {
        return true;
    }
    match (
        tokio::fs::canonicalize(a).await,
        tokio::fs::canonicalize(b).await,
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// 把 `src` 移到 `dir/name`（占名冲突按编号改名，`overwrite` 授权时替换同名旧文件），
/// 返回实际文件名。同卷 rename；跨卷先复制为 `dir` 内的临时文件（保留修改时间，可被
/// 暂停 / 删除打断）再占名，成功后删除源。
async fn move_into(
    src: &Path,
    dir: &Path,
    name: &str,
    overwrite: &crate::file_exists::OverwritePolicy,
    cancel: &CancellationToken,
) -> Result<String, DownloadError> {
    crate::output::ensure_parent(&dir.join(name)).await?;
    let avoid = std::collections::HashSet::new();
    match claim_final_name(src, dir, name, overwrite, &avoid).await {
        Err(DownloadError::Io(error)) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
        result => return result,
    }
    let staging = dir.join(format!(".{name}.fdmoving"));
    if let Err(error) = copy_cancellable(src, &staging, cancel).await {
        remove_staging(&staging).await;
        return Err(error);
    }
    let chosen = match claim_final_name(&staging, dir, name, overwrite, &avoid).await {
        Ok(chosen) => chosen,
        Err(error) => {
            remove_staging(&staging).await;
            return Err(error);
        }
    };
    if let Err(error) = tokio::fs::remove_file(src).await {
        crate::logger::report_warning("category-route", "remove moved source", &error);
    }
    Ok(chosen)
}
async fn ensure_archive_target(archive: &ArchiveRoute) -> std::io::Result<String> {
    let base = if archive.directories.is_empty() {
        Path::new(&archive.category_dir)
    } else {
        Path::new(&archive.base_dir)
    };
    tokio::fs::create_dir_all(base).await?;
    let canonical_base = tokio::fs::canonicalize(base).await?;
    let parts: &[String] = if archive.directories.is_empty() {
        std::slice::from_ref(&archive.folder_name)
    } else {
        &archive.directories
    };
    let mut path = base.to_path_buf();
    for part in parts {
        path.push(part);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "archive destination contains a symbolic link",
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "archive destination component is not a directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match tokio::fs::create_dir(&path).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Another task may create the same author/collection directory.
                        let metadata = tokio::fs::symlink_metadata(&path).await?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "archive destination changed to a non-directory or symbolic link",
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
        if !tokio::fs::canonicalize(&path)
            .await?
            .starts_with(&canonical_base)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "archive destination escapes its authorized base directory",
            ));
        }
    }
    Ok(path.to_string_lossy().into_owned())
}

/// 分块复制并逐块检查取消；完成后同步落盘并带上源文件的修改时间（「使用服务器文件时间」）。
async fn copy_cancellable(
    src: &Path,
    dst: &Path,
    cancel: &CancellationToken,
) -> Result<(), DownloadError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut reader = tokio::fs::File::open(src).await?;
    let modified = reader.metadata().await?.modified().ok();
    let mut writer = tokio::fs::File::create(dst).await?;
    let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
    loop {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).await?;
    }
    writer.flush().await?;
    let file = writer.into_std().await;
    tokio::task::spawn_blocking(move || {
        if let Some(modified) = modified {
            file.set_modified(modified)?;
        }
        file.sync_all()
    })
    .await
    .map_err(std::io::Error::other)??;
    Ok(())
}

/// 跨卷复制的分块大小。
const COPY_CHUNK_BYTES: usize = 1024 * 1024;

async fn remove_staging(staging: &Path) {
    if let Err(error) = tokio::fs::remove_file(staging).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("category-route", "remove move staging", &error);
    }
}

/// Read at most 4 KiB from the finished file for signed rule matching.
async fn read_archive_header_hex(path: &Path) -> Result<Option<String>, std::io::Error> {
    use tokio::io::AsyncReadExt;

    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::with_capacity(4096);
    file.take(4096).read_to_end(&mut bytes).await?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut header = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(header, "{byte:02x}").map_err(std::io::Error::other)?;
    }
    Ok(Some(header))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::ensure_archive_target;
    use super::{
        ArchiveRoute, CategoryRoute, normalize_archive_directories, valid_archive_directories,
        valid_archive_folder,
    };

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        route: CategoryRoute,
        file_name: String,
        url: String,
        expected: Option<String>,
    }

    #[test]
    fn shared_cases() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("fixtures/category_route_cases.json"))
                .expect("category route fixture");
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                case.route
                    .save_dir_for(&case.file_name, &case.url, &case.url),
                case.expected.as_deref(),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn url_rules_match_the_source_address_not_the_decoded_link() {
        use super::{CategoryRule, source_address};
        let route = CategoryRoute {
            rules: vec![CategoryRule {
                match_mode: "url".into(),
                url_pattern: "thunder://*".into(),
                save_dir: "/thunder".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let decoded = "https://cdn.test/files/a.zip";
        let origin = "thunder://QUFodHRwczovL2Nkbi50ZXN0L2ZpbGVzL2EuemlwWlo=";
        // 建任务 / 开始写盘前 / 完成前三个时刻都传 `source_address(origin, url)`。
        let source = source_address(origin, decoded);
        assert_eq!(source, origin);
        assert_eq!(
            route.save_dir_for("a.zip", decoded, source),
            Some("/thunder")
        );
        // 无来源地址时回落任务链接，不命中 thunder 规则。
        assert_eq!(
            route.save_dir_for("a.zip", decoded, source_address("", decoded)),
            None
        );
    }

    #[test]
    fn wildcard_url_semantics() {
        use super::wildcard_matches;
        assert!(wildcard_matches(
            "https://example.com/*",
            "https://example.com/a/b.zip?token=1"
        ));
        assert!(wildcard_matches(
            "HTTPS://X.TEST/file?.zip",
            "https://x.test/fileA.zip"
        ));
        assert!(!wildcard_matches(
            "https://x.test/file?.zip",
            "https://x.test/file12.zip"
        ));
        // 锚定：子串出现不算命中。
        assert!(!wildcard_matches("x.test/*", "https://x.test/a"));
        assert!(!wildcard_matches(
            "https://*.example.com/*",
            "https://example.com/a"
        ));
        assert!(!wildcard_matches("", "https://x.test/a"));
        assert!(!wildcard_matches("   ", "https://x.test/a"));
    }

    #[test]
    fn column_round_trips_and_tolerates_corruption() {
        let route = CategoryRoute {
            base_dir: "/downloads".into(),
            other_dir: "/other".into(),
            archive_metadata: Some(crate::model::ArchiveMetadata {
                platform: "bilibili".into(),
                media_kind: "series".into(),
                series_id: "42".into(),
                series_title: "Series".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let text = route.to_column().expect("encode route");
        assert_eq!(CategoryRoute::from_column("t", &text), Some(route));
        assert_eq!(CategoryRoute::from_column("t", ""), None);
        assert_eq!(CategoryRoute::from_column("t", "{"), None);
    }

    #[test]
    fn streaming_outputs_are_classified_as_mp4() {
        use super::output_name;
        assert_eq!(
            output_name("clip.ts", "https://cdn.test/live/index.m3u8").as_deref(),
            Some("clip.mp4")
        );
        assert_eq!(
            output_name("", "https://cdn.test/live/index.m3u8").as_deref(),
            Some("index.mp4")
        );
        assert_eq!(
            output_name("manifest.mpd", "https://cdn.test/v/manifest.mpd").as_deref(),
            Some("manifest.mp4")
        );
        // DASH 的显式非 .mpd 名原样使用；普通链接不预判。
        assert_eq!(
            output_name("movie.mkv", "https://cdn.test/v/manifest.mpd"),
            None
        );
        assert_eq!(output_name("a.ts", "https://cdn.test/a.ts"), None);
    }
    #[test]
    fn archive_folder_is_one_safe_filesystem_component() {
        assert!(valid_archive_folder("The Expanse"));
        assert!(valid_archive_folder("季节 2"));
        assert!(!valid_archive_folder(""));
        assert!(!valid_archive_folder("."));
        assert!(!valid_archive_folder(".."));
        assert!(!valid_archive_folder("../outside"));
        assert!(!valid_archive_folder("a\\b"));
        assert!(!valid_archive_folder("a:b"));
        assert!(!valid_archive_folder("bad\u{0000}name"));
    }

    #[test]
    fn archive_directories_are_bounded_safe_components() {
        let safe = vec!["视频".to_owned(), "哔哩哔哩".to_owned(), "作者".to_owned()];
        assert!(valid_archive_directories(&safe));
        assert!(normalize_archive_directories(&safe, "").is_some());
        assert!(normalize_archive_directories(&[], "Legacy").is_some());
        assert!(normalize_archive_directories(&["..".to_owned()], "").is_none());
        assert!(normalize_archive_directories(&vec!["x".into(); 9], "").is_none());
        assert!(normalize_archive_directories(&["x".repeat(181)], "").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn archive_destinations_can_be_created_concurrently() {
        let root = std::env::temp_dir().join(format!(
            "fluxdown-archive-parallel-{}",
            uuid::Uuid::new_v4()
        ));
        let archive = ArchiveRoute {
            base_dir: root.to_string_lossy().into_owned(),
            category_dir: String::new(),
            folder_name: String::new(),
            directories: vec!["视频".into(), "作者".into(), "合集".into()],
        };
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let archive = archive.clone();
            jobs.spawn(async move { super::ensure_archive_target(&archive).await });
        }
        let expected = root.join("视频/作者/合集");
        while let Some(result) = jobs.join_next().await {
            let path = result
                .expect("creation task joins")
                .expect("shared path creation succeeds");
            assert_eq!(std::path::Path::new(&path), expected.as_path());
        }
        assert!(
            tokio::fs::metadata(&expected)
                .await
                .expect("created directory")
                .is_dir()
        );
        tokio::fs::remove_dir_all(root)
            .await
            .expect("remove shared destination");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn archive_destination_rejects_symlink_escape() {
        let root =
            std::env::temp_dir().join(format!("fluxdown-archive-symlink-{}", uuid::Uuid::new_v4()));
        let base = root.join("authorized");
        let outside = root.join("outside");
        tokio::fs::create_dir_all(&base).await.expect("create base");
        tokio::fs::create_dir_all(&outside)
            .await
            .expect("create outside");
        std::os::unix::fs::symlink(&outside, base.join("link")).expect("create symlink");
        let archive = ArchiveRoute {
            category_dir: base.to_string_lossy().into_owned(),
            folder_name: String::new(),
            directories: vec!["link".into(), "escape".into()],
            base_dir: base.to_string_lossy().into_owned(),
        };
        assert!(ensure_archive_target(&archive).await.is_err());
        assert!(!outside.join("escape").exists());
        tokio::fs::remove_dir_all(root).await.expect("cleanup");
    }

    #[test]
    fn stored_archive_route_rejects_path_components() {
        let route = CategoryRoute {
            archive: Some(ArchiveRoute {
                category_dir: "/downloads/video".into(),
                folder_name: "../outside".into(),
                directories: Vec::new(),
                base_dir: String::new(),
            }),
            ..Default::default()
        };
        let text = route.to_column().expect("encode route");
        let restored = CategoryRoute::from_column("unsafe", &text).expect("valid route data");
        assert!(restored.archive.is_none());
    }

    #[tokio::test]
    async fn move_keeps_existing_files_and_cross_volume_copy_keeps_mtime() {
        let root =
            std::env::temp_dir().join(format!("fluxdown_route_move_{}", uuid::Uuid::new_v4()));
        let source_dir = root.join("base");
        let target_dir = root.join("pictures");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir");
        tokio::fs::create_dir_all(&target_dir)
            .await
            .expect("target dir");
        tokio::fs::write(target_dir.join("a.jpg"), b"existing")
            .await
            .expect("existing file");
        let source = source_dir.join("a.jpg");
        tokio::fs::write(&source, b"payload")
            .await
            .expect("source file");
        let never = crate::file_exists::OverwritePolicy::Never;
        let cancel = tokio_util::sync::CancellationToken::new();

        let chosen = super::move_into(&source, &target_dir, "a.jpg", &never, &cancel)
            .await
            .expect("move");
        assert_eq!(chosen, "a (1).jpg");
        assert_eq!(
            tokio::fs::read(target_dir.join("a.jpg"))
                .await
                .expect("kept"),
            b"existing"
        );
        assert_eq!(
            tokio::fs::read(target_dir.join(&chosen))
                .await
                .expect("moved"),
            b"payload"
        );
        assert!(!tokio::fs::try_exists(&source).await.expect("stat source"));

        // 跨卷分支：分块复制带上源文件的修改时间；已取消时中止复制。
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let copy_src = target_dir.join(&chosen);
        std::fs::File::options()
            .write(true)
            .open(&copy_src)
            .and_then(|file| file.set_modified(old))
            .expect("set source mtime");
        let copy_dst = source_dir.join("copy.jpg");
        super::copy_cancellable(&copy_src, &copy_dst, &cancel)
            .await
            .expect("copy");
        assert_eq!(
            tokio::fs::read(&copy_dst).await.expect("copied"),
            b"payload"
        );
        let copied = tokio::fs::metadata(&copy_dst).await.expect("stat copy");
        assert_eq!(copied.modified().expect("mtime"), old);
        cancel.cancel();
        let cancelled =
            super::copy_cancellable(&copy_src, &source_dir.join("x.jpg"), &cancel).await;
        assert!(matches!(cancelled, Err(super::DownloadError::Cancelled)));

        if let Err(error) = tokio::fs::remove_dir_all(&root).await {
            tracing::warn!(%error, "test directory cleanup failed");
        }
    }

    #[tokio::test]
    async fn groups_route_only_when_every_member_has_the_same_category() {
        let route = CategoryRoute {
            rules: vec![
                super::CategoryRule {
                    extensions: vec!["mp4".into()],
                    save_dir: "/video".into(),
                    ..Default::default()
                },
                super::CategoryRule {
                    extensions: vec!["srt".into()],
                    save_dir: "/subtitles".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let interceptor = crate::intercept::NoopInterceptor;
        let common = super::archive_group_root(
            "/base/group",
            "group",
            &[
                ("a.mp4", "https://host/a.mp4"),
                ("b.mp4", "https://host/b.mp4"),
            ],
            "https://host/list",
            None,
            &route,
            &interceptor,
        )
        .await;
        assert_eq!(
            std::path::Path::new(&common),
            std::path::Path::new("/video").join("group")
        );
        let mixed = super::archive_group_root(
            "/base/group",
            "group",
            &[
                ("a.mp4", "https://host/a.mp4"),
                ("b.srt", "https://host/b.srt"),
            ],
            "https://host/list",
            None,
            &route,
            &interceptor,
        )
        .await;
        assert_eq!(mixed, "/base/group");
    }
    #[tokio::test]
    async fn archive_header_read_is_bounded_to_four_kibibytes() {
        let path = std::env::temp_dir().join(format!(
            "fluxdown-archive-header-{}.bin",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::write(&path, vec![0xab; 4096 + 128])
            .await
            .expect("write header fixture");
        let header = super::read_archive_header_hex(&path)
            .await
            .expect("read bounded archive header")
            .expect("nonempty file header");
        assert_eq!(header.len(), 4096 * 2);
        assert!(
            header
                .chars()
                .all(|character| character == 'a' || character == 'b')
        );
        tokio::fs::remove_file(path)
            .await
            .expect("remove header fixture");
    }
}
