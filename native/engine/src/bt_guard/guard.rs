//! `BtGuard`：线程安全入口。热路径（`admit` / `on_event`）无 IO；持久化由宿主调用 `flush`。

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use chrono::Datelike;
use serde_json::json;

use super::cloud::{CloudDocument, CloudReject, build_cloud_layer};
use super::config::{BtGuardConfig, RuleMode};
use super::filter_file::{MAX_FILTER_ENTRIES_PER_SOURCE, parse_filter_file};
use super::ip::{IpGroup, is_lan, normalize_ip};
use super::layers::{DecideInput, Decided, Decision, Tables, decide};
use super::model::{
    Entry, EvidenceItem, FilterAction, GuardAction, GuardStats, InfoHash, Layer, LayerCounts,
    PeerInsight, Rule, Strength,
};
use super::ranges::{IpRange, RangeError};
use super::state::{CachedDecision, CloudState, Conn, FileStatus, STRIKE_DECAY_SECS, State};
use crate::db::BtGuardEntryOp;

/// 管理类 API 的错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GuardError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// 启动加载失败且无法确认库内最大条目 id：为避免覆盖库内条目，拒绝产生需要落库的新条目。
    #[error("bt_guard store unavailable: {0}")]
    StoreUnavailable(String),
}

impl From<RangeError> for GuardError {
    fn from(e: RangeError) -> Self {
        Self::InvalidArgument(e.0)
    }
}

/// 手动添加条目的入参（`allow` 动作进白名单层，其余进 manual 层）。
#[derive(Clone, Debug)]
pub struct NewEntry {
    pub cidr: String,
    pub action: FilterAction,
    /// `None` = 永久。
    pub duration_secs: Option<i64>,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct EntryFilter {
    pub layer: Option<Layer>,
    pub rule: Option<String>,
    pub query: Option<String>,
    /// 只返回 CIDR 覆盖该 IP 的条目（含 cloud / file 的宽网段）；与其它条件叠加。
    pub contains_ip: Option<IpAddr>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudStatus {
    pub sequence: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub entries: usize,
    pub client_rules: usize,
    pub stale: bool,
    pub last_error: Option<String>,
    pub rejected_reason: Option<String>,
    pub attribution: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudApplySummary {
    pub entries: usize,
    pub client_rules: usize,
    pub added: usize,
    pub removed: usize,
    pub dropped_wide: usize,
    pub dropped_invalid: usize,
    pub dropped_expired: usize,
    pub dropped_invalid_rules: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterApplySummary {
    /// 分解后的 CIDR 条目数。
    pub entries: usize,
    pub dropped_wide: usize,
    pub invalid_lines: usize,
    pub skipped_by_level: usize,
    pub truncated: bool,
    /// 超过单来源条目上限（见 `MAX_FILTER_ENTRIES_PER_SOURCE`）而被丢弃的条目数。
    pub dropped_over_cap: usize,
    /// 正文哈希与上次相同：跳过了整层替换与写库。
    pub unchanged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSourceStatus {
    pub url: String,
    pub ok: bool,
    pub entries: usize,
    pub updated_at: i64,
    pub error: Option<String>,
}

struct DayStats {
    day: i64,
    /// 当日（本地日）对应的 UTC 区间 `[start, end)`；区间内无需再调用 chrono::Local。
    start: i64,
    end: i64,
    stats: GuardStats,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// 状态锁守卫：守卫释放时若期间标记了「宿主有事可做」，通知唤醒钩子
/// （`notify_one` 不阻塞，持锁通知安全；被唤醒的任务稍后取锁即可）。
pub(super) struct StateGuard<'a> {
    guard: MutexGuard<'a, State>,
    waker: &'a Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl std::ops::Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.guard
    }
}

impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.guard
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        if !std::mem::take(&mut self.guard.wake) {
            return;
        }
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            waker.notify_one();
        }
    }
}

pub struct BtGuard {
    pub(super) config: RwLock<Arc<BtGuardConfig>>,
    pub(super) tables: RwLock<Tables>,
    /// 配置 / 表 / 云有效期变化时自增，使连接上缓存的判定失效。
    pub(super) epoch: AtomicU64,
    /// 云文档过期时刻（0 = 从未应用），供无需状态锁的 `admit` 读取。
    pub(super) cloud_valid_until: AtomicI64,
    pub(super) next_id: AtomicI64,
    stats: Mutex<DayStats>,
    pub(super) state: Mutex<State>,
    /// 宿主唤醒钩子：有待办（新 action / 待落库的事件、条目、strike）时 `notify_one`。
    waker: Mutex<Option<Arc<tokio::sync::Notify>>>,
    clock: Clock,
    /// `config.enabled` 的无锁镜像，热路径先查它。
    pub(super) enabled: AtomicBool,
    /// 启动加载失败且无法确认库内最大条目 id：禁止落库写入。
    pub(super) write_blocked: Mutex<Option<String>>,
    /// 最近一次启动加载失败且尚未重新加载成功：账本 / strike 不落库（内存状态不是库内状态的延续）。
    pub(super) load_failed: AtomicBool,
}

/// `now` 所在本地日：`(日序号, 当日起点 UTC 秒, 次日起点 UTC 秒)`。
/// 夏令时空洞等取不到本地零点时退化为 `[now, now+1)`，下次调用自然重算。
pub(super) fn local_day_range(now: i64) -> (i64, i64, i64) {
    use chrono::TimeZone;
    let fallback = (0, now, now.saturating_add(1));
    let Some(dt) = chrono::DateTime::from_timestamp(now, 0) else {
        return fallback;
    };
    let date = dt.with_timezone(&chrono::Local).date_naive();
    let midnight = |d: chrono::NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .and_then(|t| chrono::Local.from_local_datetime(&t).earliest())
            .map(|t| t.timestamp())
    };
    let start = midnight(date).unwrap_or(now).min(now);
    let end = date
        .succ_opt()
        .and_then(midnight)
        .unwrap_or_else(|| now.saturating_add(86_400))
        .max(now.saturating_add(1));
    (i64::from(date.num_days_from_ce()), start, end)
}

pub(super) fn system_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 封禁时长：`base_hours × 4^(strikes-1)`，封顶 `max_days`。
pub fn ban_duration_secs(base_hours: u32, max_days: u32, strikes: u32) -> i64 {
    let exp = strikes.saturating_sub(1).min(32);
    let hours = u64::from(base_hours)
        .saturating_mul(4u64.saturating_pow(exp))
        .min(u64::from(max_days) * 24);
    i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX)
}

/// 新建条目的字段（id / 创建时间由 `new_entry` 补）。`Arc<str>` 让同一批条目共享字符串。
struct EntrySpec {
    layer: Layer,
    range: IpRange,
    action: FilterAction,
    rule: Arc<str>,
    reason: Arc<str>,
    source: Arc<str>,
    expires_at: Option<i64>,
}

impl BtGuard {
    pub fn new(config: BtGuardConfig) -> Self {
        Self::with_clock(config, Arc::new(system_now))
    }

    /// 注入时钟（unix 秒），测试用。
    pub fn with_clock(config: BtGuardConfig, clock: Clock) -> Self {
        let enabled = config.enabled;
        Self {
            config: RwLock::new(Arc::new(config.sanitized())),
            tables: RwLock::new(Tables::default()),
            epoch: AtomicU64::new(0),
            cloud_valid_until: AtomicI64::new(0),
            next_id: AtomicI64::new(1),
            stats: Mutex::new(DayStats {
                day: i64::MIN,
                start: 0,
                end: 0,
                stats: GuardStats::default(),
            }),
            state: Mutex::new(State::default()),
            waker: Mutex::new(None),
            clock,
            enabled: AtomicBool::new(enabled),
            write_blocked: Mutex::new(None),
            load_failed: AtomicBool::new(false),
        }
    }

    /// 最近一次 [`Self::load`] 失败且之后没有成功过：宿主应在合适时机（BT 会话启动）重试。
    pub fn load_failed(&self) -> bool {
        self.load_failed.load(Ordering::Acquire)
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    pub(super) fn lock(&self) -> StateGuard<'_> {
        StateGuard {
            guard: self.state.lock().unwrap_or_else(PoisonError::into_inner),
            waker: &self.waker,
        }
    }

    /// 设置（或覆盖）唤醒钩子。触发点：新增了此前不在队列里的 action，或新增待落库的
    /// 条目操作 / 事件 / strike，或 `Disconnected` 使账本变脏，或失败的 flush 被还原。
    /// 每块上传的账本累加不通知（保持空闲静默）。未设置时无额外开销。
    pub fn set_waker(&self, waker: Arc<tokio::sync::Notify>) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
    }

    /// 会话关闭时清空连接态（不动账本 / 条目 / strike），避免泄漏。
    pub fn clear_connections(&self) {
        self.lock().conns.clear();
    }

    /// 启动加载失败且库内最大条目 id 未知时拒绝产生新条目（见 [`Self::load`]）。
    pub(super) fn check_writable(&self) -> Result<(), GuardError> {
        match &*self
            .write_blocked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            Some(reason) => Err(GuardError::StoreUnavailable(reason.clone())),
            None => Ok(()),
        }
    }

    pub fn config(&self) -> Arc<BtGuardConfig> {
        Arc::clone(&self.config.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub(super) fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::AcqRel)
    }

    pub(super) fn stats_bump(&self, now: i64, f: impl FnOnce(&mut GuardStats)) {
        let mut s = self.stats.lock().unwrap_or_else(PoisonError::into_inner);
        if !(s.start..s.end).contains(&now) {
            let (day, start, end) = local_day_range(now);
            s.start = start;
            s.end = end;
            if s.day != day {
                s.day = day;
                s.stats = GuardStats::default();
            }
        }
        f(&mut s.stats);
    }

    /// 今日（本地日）统计。
    pub fn stats(&self) -> GuardStats {
        let now = self.now();
        let s = self.stats.lock().unwrap_or_else(PoisonError::into_inner);
        if (s.start..s.end).contains(&now) {
            s.stats
        } else {
            GuardStats::default()
        }
    }

    pub(super) fn tables_snapshot(&self) -> Tables {
        self.tables
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn layer_entries(&self, layer: Layer) -> Vec<Entry> {
        self.tables
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .layer(layer)
            .entries()
            .to_vec()
    }

    /// 整层替换（调用方持有状态锁，保证读-改-写不交错）。
    pub(super) fn set_layer(&self, layer: Layer, entries: Vec<Entry>) {
        let mut tables = self.tables.write().unwrap_or_else(PoisonError::into_inner);
        *tables = tables.with_layer(layer, entries);
        drop(tables);
        self.bump_epoch();
    }

    pub(super) fn decide_input(&self, cfg: &BtGuardConfig, now: i64) -> DecideInput {
        let valid_until = self.cloud_valid_until.load(Ordering::Acquire);
        DecideInput {
            now,
            cloud_mode: cfg.cloud_mode,
            cloud_active: cfg.cloud_enabled && valid_until > now,
            cloud_valid_until: valid_until,
        }
    }

    pub(super) fn decide_ip(&self, cfg: &BtGuardConfig, ip: IpAddr, now: i64) -> Decided {
        let tables = self.tables.read().unwrap_or_else(PoisonError::into_inner);
        decide(&tables, ip, self.decide_input(cfg, now))
    }

    /// 连接前判定：`false` = 拒绝（封禁条目命中），计入「拦截连接数」。
    pub fn admit(&self, ip: IpAddr) -> bool {
        if !self.enabled.load(Ordering::Acquire) {
            return true;
        }
        let cfg = self.config();
        if !cfg.enabled {
            return true;
        }
        let ip = normalize_ip(ip);
        if cfg.skip_lan && is_lan(ip) {
            return true;
        }
        let now = self.now();
        let decided = self.decide_ip(&cfg, ip, now);
        if matches!(
            decided.decision,
            Decision::Act {
                action: FilterAction::Ban,
                ..
            }
        ) {
            self.stats_bump(now, |s| s.blocked_connections += 1);
            false
        } else {
            true
        }
    }

    /// 不计数的封禁查询，供 `disconnect_peers_where` 的闭包使用。
    pub fn is_banned(&self, ip: IpAddr) -> bool {
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }
        let cfg = self.config();
        if !cfg.enabled {
            return false;
        }
        let ip = normalize_ip(ip);
        if cfg.skip_lan && is_lan(ip) {
            return false;
        }
        matches!(
            self.decide_ip(&cfg, ip, self.now()).decision,
            Decision::Act {
                action: FilterAction::Ban,
                ..
            }
        )
    }

    /// 热更新配置。变更的过滤文件订阅（被移除的 URL）立即丢弃其条目。
    pub fn set_config(&self, config: BtGuardConfig) {
        let config = Arc::new(config.sanitized());
        let mut st = self.lock();
        let old = {
            let mut guard = self.config.write().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *guard, Arc::clone(&config))
        };
        self.enabled.store(config.enabled, Ordering::Release);
        if !config.report_enabled {
            st.evidence.clear();
            st.reported.clear();
        }
        if old.cloud_mode != config.cloud_mode
            || old.cloud_enabled != config.cloud_enabled
            || old.enabled != config.enabled
        {
            st.push_action(GuardAction::DisconnectBanned);
        }
        let keep: HashSet<String> = config.filter_url_list().into_iter().collect();
        let stale_urls: Vec<String> = st
            .files
            .keys()
            .filter(|u| !keep.contains(*u))
            .cloned()
            .collect();
        if !stale_urls.is_empty() {
            let mut entries = self.layer_entries(Layer::File);
            entries.retain(|e| !stale_urls.iter().any(|u| u.as_str() == &*e.source));
            self.set_layer(Layer::File, entries);
            for url in stale_urls {
                st.files.remove(&url);
                st.pending.entry_ops.push(BtGuardEntryOp::ReplaceLayer {
                    layer: Layer::File.as_str().to_owned(),
                    source: Some(url),
                    rows: Vec::new(),
                });
            }
        }
        st.push_action(GuardAction::Changed);
        drop(st);
        self.bump_epoch();
    }

    /// 取走待宿主执行的副作用。
    pub fn take_actions(&self) -> Vec<GuardAction> {
        std::mem::take(&mut self.lock().actions)
    }

    /// 弹出待上报证据。
    pub fn drain_evidence(&self) -> Vec<EvidenceItem> {
        self.lock().evidence.drain(..).collect()
    }

    pub fn pending_evidence(&self) -> usize {
        self.lock().evidence.len()
    }

    /// 是否有待落库内容（宿主在事件驱动点判断是否需要 `flush`）。
    pub fn needs_flush(&self) -> bool {
        !self.lock().pending.is_empty()
    }

    // ---------------------------------------------------------------- 条目管理

    fn new_entry(&self, spec: EntrySpec, now: i64) -> Entry {
        Entry {
            id: self.alloc_id(),
            layer: spec.layer,
            range: spec.range,
            action: spec.action,
            rule: spec.rule,
            reason: spec.reason,
            evidence: None,
            source: spec.source,
            created_at: now,
            expires_at: spec.expires_at,
        }
    }

    /// 把任意 IP / 组文本规范化为事件表 `ip_group` 列的形式：
    /// IP（含 v4-mapped / NAT64）按当前 `ipv6_prefix` 归组；带 `/len` 的 v6 组文本原样掩码。
    pub fn normalize_group(&self, text: &str) -> Result<String, GuardError> {
        let text = text.trim();
        if text.contains('/') {
            return text
                .parse::<IpGroup>()
                .map(|g| g.to_string())
                .map_err(GuardError::InvalidArgument);
        }
        let ip = text
            .parse::<IpAddr>()
            .map_err(|e| GuardError::InvalidArgument(format!("{text:?}: {e}")))?;
        Ok(IpGroup::of(normalize_ip(ip), self.config().ipv6_prefix).to_string())
    }

    /// 手动添加：`allow` → 白名单层，其余 → manual 层。主机位被掩掉，规范化为 CIDR。
    /// 同层、同规范 CIDR、同动作的条目已存在时更新其 `expires_at` / `reason` 并返回该条目（id 不变）。
    pub fn add_entry(&self, new: NewEntry) -> Result<Entry, GuardError> {
        self.check_writable()?;
        if let Some(d) = new.duration_secs
            && d <= 0
        {
            return Err(GuardError::InvalidArgument(
                "durationSecs must be positive".to_owned(),
            ));
        }
        let range = IpRange::parse_cidr(&new.cidr)?;
        let now = self.now();
        let layer = if new.action == FilterAction::Allow {
            Layer::Allow
        } else {
            Layer::Manual
        };
        let expires_at = new.duration_secs.map(|d| now.saturating_add(d));
        let mut st = self.lock();
        let mut entries = self.layer_entries(layer);
        let (entry, updated) = if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.range == range && e.action == new.action)
        {
            existing.expires_at = expires_at;
            existing.reason = Arc::from(new.reason.as_str());
            (existing.clone(), true)
        } else {
            let created = self.new_entry(
                EntrySpec {
                    layer,
                    range,
                    action: new.action,
                    rule: Arc::from("manual"),
                    reason: Arc::from(new.reason.as_str()),
                    source: Arc::from("user"),
                    expires_at,
                },
                now,
            );
            entries.push(created.clone());
            (created, false)
        };
        self.set_layer(layer, entries);
        st.pending
            .entry_ops
            .push(BtGuardEntryOp::Upsert(entry.to_row()));
        st.record_event(
            now,
            "entry_added",
            None,
            None,
            "",
            "",
            new.action.as_str(),
            json!({"cidr": entry.cidr(), "layer": layer.as_str(), "updated": updated}).to_string(),
        );
        st.push_action(GuardAction::Changed);
        if new.action == FilterAction::Ban {
            st.push_action(GuardAction::DisconnectBanned);
        }
        Ok(entry)
    }

    /// 按 id 删除（任意层，除 cloud）。含 cloud 层条目时整个请求被拒绝、不删任何条目；
    /// 不存在的 id 忽略。返回实际删除数。
    pub fn remove_entries(&self, ids: &[i64]) -> Result<usize, GuardError> {
        let wanted: HashSet<i64> = ids.iter().copied().collect();
        let now = self.now();
        let mut st = self.lock();
        for entry in self.layer_entries(Layer::Cloud) {
            if wanted.contains(&entry.id) {
                return Err(GuardError::InvalidArgument(format!(
                    "entry {} belongs to the cloud layer and cannot be removed; add an allow entry instead",
                    entry.id
                )));
            }
        }
        let mut removed = 0usize;
        for layer in [Layer::Allow, Layer::Manual, Layer::Auto, Layer::File] {
            let entries = self.layer_entries(layer);
            if !entries.iter().any(|e| wanted.contains(&e.id)) {
                continue;
            }
            let (gone, keep): (Vec<Entry>, Vec<Entry>) =
                entries.into_iter().partition(|e| wanted.contains(&e.id));
            removed += gone.len();
            self.set_layer(layer, keep);
            for entry in gone {
                st.pending.entry_ops.push(BtGuardEntryOp::Delete(entry.id));
                st.record_event(
                    now,
                    "entry_removed",
                    None,
                    None,
                    &entry.rule,
                    "",
                    entry.action.as_str(),
                    json!({"cidr": entry.cidr(), "layer": layer.as_str()}).to_string(),
                );
            }
        }
        if removed > 0 {
            // 解封是落库点：连带写全部脏账本。
            st.pending.urgent = true;
            st.push_action(GuardAction::Changed);
        }
        Ok(removed)
    }

    /// 内存中的条目分页（按 id 降序），只含未过期条目。返回 `(total, items)`。
    pub fn list_entries(&self, filter: &EntryFilter) -> (usize, Vec<Entry>) {
        let now = self.now();
        let needle = filter
            .query
            .as_deref()
            .filter(|q| !q.is_empty())
            .map(str::to_lowercase);
        let tables = self.tables_snapshot();
        let layers: Vec<Layer> = match filter.layer {
            Some(l) => vec![l],
            None => Layer::ALL.to_vec(),
        };
        let contains_ip = filter.contains_ip.map(normalize_ip);
        let mut matched: Vec<&Entry> = Vec::new();
        for layer in &layers {
            for entry in tables.layer(*layer).entries() {
                if !entry.is_active(now) {
                    continue;
                }
                if let Some(ip) = contains_ip
                    && !entry.range.contains(ip)
                {
                    continue;
                }
                if let Some(rule) = &filter.rule
                    && &*entry.rule != rule.as_str()
                {
                    continue;
                }
                if let Some(needle) = &needle
                    && !(entry.cidr().to_lowercase().contains(needle)
                        || entry.reason.to_lowercase().contains(needle)
                        || entry.source.to_lowercase().contains(needle))
                {
                    continue;
                }
                matched.push(entry);
            }
        }
        matched.sort_by_key(|e| std::cmp::Reverse(e.id));
        let total = matched.len();
        let items = matched
            .into_iter()
            .skip(filter.offset)
            .take(filter.limit)
            .cloned()
            .collect();
        (total, items)
    }

    pub fn counts(&self) -> LayerCounts {
        let now = self.now();
        let tables = self.tables_snapshot();
        let n = |layer: Layer| {
            tables
                .layer(layer)
                .entries()
                .iter()
                .filter(|e| e.is_active(now))
                .count()
        };
        LayerCounts {
            allow: n(Layer::Allow),
            manual: n(Layer::Manual),
            auto: n(Layer::Auto),
            cloud: n(Layer::Cloud),
            file: n(Layer::File),
        }
    }

    /// 导出：每行 `cidr # layer rule reason expires`（expires 为 unix 秒或 `never`）。
    pub fn export(&self, layer: Option<Layer>) -> String {
        let (_, items) = self.list_entries(&EntryFilter {
            layer,
            rule: None,
            query: None,
            contains_ip: None,
            offset: 0,
            limit: usize::MAX,
        });
        let mut items = items;
        items.sort_by_key(|e| e.id);
        let mut out = String::new();
        for e in items {
            let clean = |s: &str| s.replace(['\r', '\n'], " ");
            let expires = e
                .expires_at
                .map_or_else(|| "never".to_owned(), |t| t.to_string());
            out.push_str(&format!(
                "{} # {} {} {} {}\n",
                e.cidr(),
                e.layer.as_str(),
                clean(&e.rule),
                clean(&e.reason),
                expires
            ));
        }
        out
    }

    // ---------------------------------------------------------------- 云名单

    /// 应用已验签的云名单 payload（整层替换）。被拒绝时保留上一份并记录原因。
    pub fn apply_cloud(&self, doc: &CloudDocument) -> Result<CloudApplySummary, CloudReject> {
        let now = self.now();
        let mut st = self.lock();
        let previous_entries = self.layer_entries(Layer::Cloud);
        let previous: HashSet<(IpRange, FilterAction)> = previous_entries
            .iter()
            .map(|e| (e.range, e.action))
            .collect();
        let build =
            match build_cloud_layer(doc, now, st.cloud.sequence, &previous, st.cloud.valid_until) {
                Ok(b) => b,
                Err(reject) => {
                    crate::log_warn!("[BtGuard] cloud list rejected: {reject}");
                    st.cloud.rejected_reason = Some(reject.to_string());
                    st.record_event(
                        now,
                        "cloud_rejected",
                        None,
                        None,
                        "cloud",
                        "",
                        "",
                        json!({"sequence": doc.sequence, "reason": reject.to_string()}).to_string(),
                    );
                    st.push_action(GuardAction::Changed);
                    return Err(reject);
                }
            };
        let source: Arc<str> = Arc::from(format!("cloud#{}#{}", doc.sequence, doc.expires_at));
        let rule: Arc<str> = Arc::from("cloud");
        let mut entries = Vec::with_capacity(build.entries.len());
        let mut has_ban = false;
        for built in &build.entries {
            has_ban |= built.action == FilterAction::Ban;
            let mut entry = self.new_entry(
                EntrySpec {
                    layer: Layer::Cloud,
                    range: built.range,
                    action: built.action,
                    rule: Arc::clone(&rule),
                    reason: Arc::from(built.reason.as_str()),
                    source: Arc::clone(&source),
                    expires_at: Some(built.expires_at),
                },
                now,
            );
            entry.evidence = Some(Arc::from(
                json!({"category": built.category}).to_string().as_str(),
            ));
            entries.push(entry);
        }
        let new_keys: HashSet<(IpRange, FilterAction)> =
            entries.iter().map(|e| (e.range, e.action)).collect();
        let added = new_keys.difference(&previous).count();
        let removed = previous.difference(&new_keys).count();
        let rows = entries.iter().map(Entry::to_row).collect();
        let count = entries.len();
        self.set_layer(Layer::Cloud, entries);
        st.pending.entry_ops.push(BtGuardEntryOp::ReplaceLayer {
            layer: Layer::Cloud.as_str().to_owned(),
            source: None,
            rows,
        });
        st.cloud = CloudState {
            sequence: doc.sequence,
            updated_at: now,
            valid_until: doc.expires_at,
            entries: count,
            client_rules: build.client_rules.clone(),
            attribution: doc.attribution.clone(),
            last_error: None,
            rejected_reason: None,
        };
        st.pending.cloud_meta = super::store::cloud_meta_json(&st.cloud);
        self.cloud_valid_until
            .store(doc.expires_at, Ordering::Release);
        self.bump_epoch();
        st.record_event(
            now,
            "cloud_applied",
            None,
            None,
            "cloud",
            "",
            "",
            json!({
                "sequence": doc.sequence, "entries": count, "added": added, "removed": removed,
                "droppedWide": build.dropped_wide, "droppedInvalid": build.dropped_invalid,
            })
            .to_string(),
        );
        crate::log_info!(
            "[BtGuard] cloud list applied: seq={} entries={} +{} -{} wide-dropped={}",
            doc.sequence,
            count,
            added,
            removed,
            build.dropped_wide
        );
        st.push_action(GuardAction::Changed);
        if has_ban {
            st.push_action(GuardAction::DisconnectBanned);
        }
        Ok(CloudApplySummary {
            entries: count,
            client_rules: build.client_rules.len(),
            added,
            removed,
            dropped_wide: build.dropped_wide,
            dropped_invalid: build.dropped_invalid,
            dropped_expired: build.dropped_expired,
            dropped_invalid_rules: build.dropped_invalid_rules,
        })
    }

    /// 记录一次云名单拉取 / 验签失败（`lastError`）。
    pub fn note_cloud_error(&self, message: &str) {
        let mut st = self.lock();
        st.cloud.last_error = Some(message.to_owned());
        st.push_action(GuardAction::Changed);
    }

    pub fn cloud_status(&self) -> CloudStatus {
        let now = self.now();
        let st = self.lock();
        let c = &st.cloud;
        CloudStatus {
            sequence: c.sequence,
            updated_at: c.updated_at,
            expires_at: c.valid_until,
            entries: c.entries,
            client_rules: c.client_rules.len(),
            stale: c.valid_until != 0 && now >= c.valid_until,
            last_error: c.last_error.clone(),
            rejected_reason: c.rejected_reason.clone(),
            attribution: c.attribution.clone(),
        }
    }

    /// 当前已应用文档的 sequence（0 = 无）。
    pub fn cloud_sequence(&self) -> i64 {
        self.lock().cloud.sequence
    }

    // ---------------------------------------------------------------- 过滤文件

    /// 应用一个过滤文件订阅（整来源替换）。
    ///
    /// 正文哈希与上次相同（且库内条目仍完整）时跳过整层替换与写库；拆分后单来源条目数超过
    /// `MAX_FILTER_ENTRIES_PER_SOURCE` 的部分被丢弃，并记入该来源的状态错误信息。
    pub fn apply_filter_file(&self, url: &str, text: &str) -> FilterApplySummary {
        use sha2::{Digest, Sha256};
        let now = self.now();
        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        {
            let mut st = self.lock();
            let current = self
                .layer_entries(Layer::File)
                .iter()
                .filter(|e| &*e.source == url)
                .count();
            if let Some(status) = st.files.get_mut(url)
                && status.hash == Some(hash)
                && status.entries == current
                && let Some(last) = status.last.clone()
            {
                status.ok = true;
                status.updated_at = now;
                status.error.clone_from(&status.truncation);
                return FilterApplySummary {
                    unchanged: true,
                    ..last
                };
            }
        }
        let parsed = parse_filter_file(text);
        let source: Arc<str> = Arc::from(url);
        let rule: Arc<str> = Arc::from("file");
        let reason: Arc<str> = Arc::from("");
        let mut fresh: Vec<Entry> = Vec::new();
        let mut over_cap = 0usize;
        for range in &parsed.ranges {
            for block in range.to_cidr_ranges() {
                if fresh.len() >= MAX_FILTER_ENTRIES_PER_SOURCE {
                    over_cap += 1;
                    continue;
                }
                fresh.push(self.new_entry(
                    EntrySpec {
                        layer: Layer::File,
                        range: block,
                        action: FilterAction::Ban,
                        rule: Arc::clone(&rule),
                        reason: Arc::clone(&reason),
                        source: Arc::clone(&source),
                        expires_at: None,
                    },
                    now,
                ));
            }
        }
        let truncation = (over_cap > 0).then(|| {
            format!(
                "truncated: {over_cap} entries over the {MAX_FILTER_ENTRIES_PER_SOURCE} per-source cap were dropped"
            )
        });
        if let Some(note) = &truncation {
            crate::log_warn!("[BtGuard] filter source {url}: {note}");
        }
        let mut st = self.lock();
        let mut entries = self.layer_entries(Layer::File);
        entries.retain(|e| &*e.source != url);
        let rows = fresh.iter().map(Entry::to_row).collect();
        let count = fresh.len();
        entries.extend(fresh);
        self.set_layer(Layer::File, entries);
        st.pending.entry_ops.push(BtGuardEntryOp::ReplaceLayer {
            layer: Layer::File.as_str().to_owned(),
            source: Some(url.to_owned()),
            rows,
        });
        if parsed.dropped_wide > 0 {
            crate::log_warn!(
                "[BtGuard] filter source dropped {} over-wide ranges",
                parsed.dropped_wide
            );
        }
        let summary = FilterApplySummary {
            entries: count,
            dropped_wide: parsed.dropped_wide,
            invalid_lines: parsed.invalid_lines,
            skipped_by_level: parsed.skipped_by_level,
            truncated: parsed.truncated,
            dropped_over_cap: over_cap,
            unchanged: false,
        };
        st.files.insert(
            url.to_owned(),
            FileStatus {
                ok: true,
                entries: count,
                updated_at: now,
                error: truncation.clone(),
                hash: Some(hash),
                truncation,
                last: Some(summary.clone()),
            },
        );
        st.record_event(
            now,
            "filter_applied",
            None,
            None,
            "file",
            "",
            "",
            json!({
                "url": url, "entries": count, "droppedWide": parsed.dropped_wide,
                "invalidLines": parsed.invalid_lines, "droppedOverCap": over_cap,
            })
            .to_string(),
        );
        st.push_action(GuardAction::Changed);
        if count > 0 {
            st.push_action(GuardAction::DisconnectBanned);
        }
        summary
    }

    /// 过滤文件订阅拉取失败：保留旧条目，只更新状态。
    pub fn note_filter_error(&self, url: &str, message: &str) {
        let mut st = self.lock();
        let entries = self
            .layer_entries(Layer::File)
            .iter()
            .filter(|e| &*e.source == url)
            .count();
        let slot = st.files.entry(url.to_owned()).or_insert(FileStatus {
            ok: false,
            entries,
            updated_at: 0,
            error: None,
            hash: None,
            truncation: None,
            last: None,
        });
        slot.ok = false;
        slot.error = Some(message.to_owned());
        st.push_action(GuardAction::Changed);
    }

    /// 配置中的订阅 URL 及其状态（尚未拉取的 URL `ok = false`、无错误）。
    pub fn filter_sources(&self) -> Vec<FilterSourceStatus> {
        let cfg = self.config();
        let st = self.lock();
        cfg.filter_url_list()
            .into_iter()
            .map(|url| match st.files.get(&url) {
                Some(s) => FilterSourceStatus {
                    url,
                    ok: s.ok,
                    entries: s.entries,
                    updated_at: s.updated_at,
                    error: s.error.clone(),
                },
                None => FilterSourceStatus {
                    url,
                    ok: false,
                    entries: 0,
                    updated_at: 0,
                    error: None,
                },
            })
            .collect()
    }

    // ---------------------------------------------------------------- 查询

    /// 当前连接的研判（`BtPeerDto` 扩展字段）。`addr` 为对端地址（可为 mapped）。
    pub fn peer_insight(
        &self,
        info_hash: &InfoHash,
        addr: std::net::SocketAddr,
    ) -> Option<PeerInsight> {
        let ip = normalize_ip(addr.ip());
        let key = (*info_hash, std::net::SocketAddr::new(ip, addr.port()));
        let st = self.lock();
        let conn = st.conns.get(&key)?;
        let suspicion = conn
            .hits
            .iter()
            .map(|(r, _)| r.strength())
            .max()
            .map_or("none", Strength::as_str);
        let mut rules: Vec<&'static str> = conn.hits.iter().map(|(r, _)| r.as_str()).collect();
        rules.dedup();
        let action = match conn.verdict {
            super::model::Verdict::Disconnect => "banned",
            super::model::Verdict::Choke => "choke",
            super::model::Verdict::Throttle => "throttle",
            super::model::Verdict::Allow => "allow",
        };
        Some(PeerInsight {
            ip_group: conn.group.to_string(),
            group_delivered_bytes: st
                .ledger(info_hash, &conn.group)
                .map_or(0, |l| l.delivered_bytes),
            suspicion,
            rules,
            action,
            observe_only: !conn.hits.is_empty() && conn.hits.iter().all(|(_, enforced)| !enforced),
        })
    }

    /// 某种子下账本的对端组数（每种子上限 4096）。
    pub fn ledger_group_count(&self, info_hash: &InfoHash) -> usize {
        self.lock().ledgers.get(info_hash).map_or(0, HashMap::len)
    }

    /// 某 (种子, 组) 累计送达字节；无账本为 `None`。
    pub fn group_delivered_bytes(&self, info_hash: &InfoHash, group: &IpGroup) -> Option<u64> {
        self.lock()
            .ledger(info_hash, group)
            .map(|l| l.delivered_bytes)
    }

    /// 某 (种子, 组) 的 dup 字节；无账本为 `None`。
    pub fn group_dup_bytes(&self, info_hash: &InfoHash, group: &IpGroup) -> Option<u64> {
        self.lock().ledger(info_hash, group).map(|l| l.dup_bytes)
    }

    /// 某分组当前有效 strike 数（含 30 天衰减）。
    pub fn strikes_of(&self, group: &IpGroup) -> u32 {
        let now = self.now();
        self.lock()
            .strikes
            .get(group)
            .filter(|s| now - s.decay_from() < STRIKE_DECAY_SECS)
            .map_or(0, |s| s.strikes)
    }

    // 以下供 events.rs 使用
    pub(super) fn refresh_decision(&self, cfg: &BtGuardConfig, conn: &mut Conn, now: i64) {
        let epoch = self.epoch.load(Ordering::Acquire);
        let c = conn.decision;
        if c.valid && c.epoch == epoch && now < c.decided.valid_until {
            return;
        }
        let decided = self.decide_ip(cfg, conn.ip, now);
        conn.decision = CachedDecision {
            epoch,
            decided,
            valid: true,
        };
    }
}

pub(super) fn mode_of(cfg: &BtGuardConfig, rule: Rule) -> RuleMode {
    match rule {
        Rule::Excessive => cfg.rule_excessive,
        Rule::Rerequest => cfg.rule_rerequest,
        Rule::Rewind => cfg.rule_rewind,
        Rule::Lag => cfg.rule_lag,
        Rule::Fingerprint => cfg.rule_fingerprint,
        Rule::Cloud => cfg.cloud_mode,
    }
}
