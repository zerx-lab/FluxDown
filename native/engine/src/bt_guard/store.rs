//! 持久化胶水：显式 `flush`（宿主在判定 / 处置 / 解封、Disconnected、会话关闭时调用）与启动 `load`。
//! 无周期定时器；热路径只改内存。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use super::bitset::PieceSet;
use super::guard::BtGuard;
use super::ip::IpGroup;
use super::model::{Entry, InfoHash, Layer};
use super::state::{
    LEDGER_RETENTION_SECS, Ledger, OBSERVE_GATE_SECS, STRIKE_DECAY_SECS, StrikeRec, TakenKeys,
    ledger_row,
};
use crate::db::{BtGuardBatch, BtGuardPrune, BtGuardStrikeRow, Db, DbError};

/// 云名单文档元数据（`bt_guard_cloud_meta` 单行 JSON）。
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudMeta {
    sequence: i64,
    updated_at: i64,
    expires_at: i64,
    attribution: Vec<String>,
    client_rules: Vec<super::cloud::ClientRule>,
}

pub(super) fn cloud_meta_json(cloud: &super::state::CloudState) -> Option<String> {
    let meta = CloudMeta {
        sequence: cloud.sequence,
        updated_at: cloud.updated_at,
        expires_at: cloud.valid_until,
        attribution: cloud.attribution.clone(),
        client_rules: cloud.client_rules.clone(),
    };
    match serde_json::to_string(&meta) {
        Ok(json) => Some(json),
        Err(error) => {
            crate::log_warn!("[BtGuard] cloud meta not persisted: {error}");
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadSummary {
    pub entries: usize,
    pub skipped_entries: usize,
    pub ledgers: usize,
    pub strikes: usize,
}

fn parse_cloud_source(source: &str) -> Option<(i64, i64)> {
    let rest = source.strip_prefix("cloud#")?;
    let (seq, exp) = rest.split_once('#')?;
    Some((seq.parse().ok()?, exp.parse().ok()?))
}

impl BtGuard {
    /// 取走待落库内容生成批次（含内存侧过期 / 保留期清理）。
    fn prepare_batch(&self, now: i64, all: bool) -> (BtGuardBatch, TakenKeys) {
        let mut st = self.lock();

        // 到期条目：从内存索引剔除（DB 侧由 prune 删除）。
        for layer in Layer::ALL {
            let entries = self.layer_entries(layer);
            if entries.iter().any(|e| !e.is_active(now)) {
                let live = entries.into_iter().filter(|e| e.is_active(now)).collect();
                self.set_layer(layer, live);
            }
        }
        // 账本保留期（有存活连接的组保留）。
        let cutoff = now - LEDGER_RETENTION_SECS;
        let live: HashSet<(InfoHash, IpGroup)> =
            st.conns.iter().map(|(k, c)| (k.0, c.group)).collect();
        let mut expired: Vec<(InfoHash, IpGroup)> = Vec::new();
        let mut touched: Vec<(InfoHash, IpGroup)> = Vec::new();
        for (hash, map) in &mut st.ledgers {
            map.retain(|group, ledger| {
                let fresh = ledger.last_seen >= cutoff;
                let is_live = live.contains(&(*hash, *group));
                if !fresh && is_live {
                    // 存活连接就是「刚见过」：顺带刷新，避免 DB 侧按 last_seen 清理时丢掉其行。
                    ledger.last_seen = now;
                    touched.push((*hash, *group));
                }
                if !fresh && !is_live {
                    expired.push((*hash, *group));
                }
                fresh || is_live
            });
        }
        st.ledgers.retain(|_, m| !m.is_empty());
        for key in &expired {
            st.pending.dirty_ledgers.remove(key);
        }
        // 刷新过 last_seen 的存活账本立即写回（否则 DB 侧按 last_seen 的清理会删掉其行）。
        st.pending.disconnected.extend(touched);
        // strike 衰减期。
        let strike_cutoff = now - STRIKE_DECAY_SECS;
        st.strikes.retain(|_, s| s.decay_from() >= strike_cutoff);
        let strikes_snapshot = std::mem::take(&mut st.pending.dirty_strikes);
        st.observed_gate.retain(|_, t| now - *t < OBSERVE_GATE_SECS);

        let mut keys = TakenKeys::default();
        // 条目 id 无法与库内最大 id 对齐时（启动加载失败且无法确认）：条目写入会覆盖库内行，丢弃。
        let entry_ops = std::mem::take(&mut st.pending.entry_ops);
        let entry_ops = if self.check_writable().is_err() {
            if !entry_ops.is_empty() {
                crate::log_warn!(
                    "[BtGuard] dropped {} entry write(s): stored entry ids are unknown",
                    entry_ops.len()
                );
            }
            Vec::new()
        } else {
            entry_ops
        };
        let mut batch = BtGuardBatch {
            entry_ops,
            events: std::mem::take(&mut st.pending.events).into(),
            cloud_meta: st.pending.cloud_meta.take(),
            ..BtGuardBatch::default()
        };
        // 启动加载尚未成功：内存里的账本 / strike 不是库内状态的延续，写出去会覆盖库内记录。
        // 与条目写入同一门控——取走并丢弃待写标记，成功重新加载后状态以库为准。事件照常落库。
        let gated = self.load_failed.load(Ordering::Acquire);
        for group in strikes_snapshot {
            if gated {
                continue;
            }
            keys.dirty_strikes.push(group);
            if let Some(rec) = st.strikes.get(&group) {
                batch.strikes.push(BtGuardStrikeRow {
                    ip_group: group.to_string(),
                    strikes: i64::from(rec.strikes),
                    last_strike_at: rec.last_strike_at,
                    last_ban_until: rec.ban_until,
                });
            }
        }
        // 被 LRU 淘汰时仍脏的账本先落库（同键的存活账本在其后写入，后者覆盖）。
        let evicted = std::mem::take(&mut st.pending.evicted);
        if !gated {
            keys.evicted = evicted;
            batch.ledgers.extend(keys.evicted.iter().cloned());
        }
        // 账本落库点：Disconnected 只写该组；判定 / 处置 / 解封（urgent）、会话关闭与引擎
        // 关闭（all）写全部脏账本；Uploaded 只留脏标记。
        let disconnected = std::mem::take(&mut st.pending.disconnected);
        keys.urgent = std::mem::take(&mut st.pending.urgent);
        let write_all = all || keys.urgent;
        let dirty: Vec<(InfoHash, IpGroup)> = if write_all {
            std::mem::take(&mut st.pending.dirty_ledgers)
                .into_iter()
                .chain(disconnected.iter().copied())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            for key in &disconnected {
                st.pending.dirty_ledgers.remove(key);
            }
            disconnected.iter().copied().collect()
        };
        if !gated {
            keys.disconnected.extend(disconnected);
            for (hash, group) in dirty {
                keys.dirty_ledgers.push((hash, group));
                if let Some(ledger) = st.ledger(&hash, &group) {
                    batch.ledgers.push(ledger_row(&hash, &group, ledger));
                }
            }
        }
        let removed = std::mem::take(&mut st.pending.removed_ledgers);
        if !gated {
            for key in removed {
                batch
                    .ledger_deletes
                    .push((hex::encode(key.0), key.1.to_string()));
                keys.removed_ledgers.push(key);
            }
            for (hash, group) in expired {
                batch
                    .ledger_deletes
                    .push((hex::encode(hash), group.to_string()));
            }
        }
        batch.prune = Some(BtGuardPrune {
            now,
            ledger_cutoff: cutoff,
            strike_cutoff,
        });
        (batch, keys)
    }

    /// 事件驱动 flush：条目 / 事件 / strike / 云元数据，加上 `Disconnected` 变脏的账本；
    /// 若期间发生了判定 / 处置 / 解封（urgent）则连带写全部脏账本。仅因上传累加而变脏的账本
    /// 留到 [`Self::flush_all`]。失败则原样放回待办队列并上抛，宿主下次事件点重试。
    pub async fn flush(&self, db: &Db) -> Result<(), DbError> {
        self.flush_impl(db, false).await
    }

    /// 会话关闭 / 引擎关闭：连上传累加留下的脏账本也一并写入。
    pub async fn flush_all(&self, db: &Db) -> Result<(), DbError> {
        self.flush_impl(db, true).await
    }

    /// `flush_all` 是否有事可做。
    pub fn needs_flush_all(&self) -> bool {
        !self.lock().pending.is_clean()
    }

    async fn flush_impl(&self, db: &Db, all: bool) -> Result<(), DbError> {
        let now = self.now();
        let (batch, keys) = self.prepare_batch(now, all);
        match db.bt_guard_apply_batch(&batch).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.lock().restore(batch, keys);
                Err(error)
            }
        }
    }

    /// 启动加载：未过期条目、账本（14 天内）、strike；恢复 cloud 层 sequence / 有效期。
    /// 会清空既有内存状态（连接除外）。
    pub async fn load(&self, db: &Db) -> Result<LoadSummary, DbError> {
        match self.load_inner(db).await {
            Ok(summary) => {
                self.load_failed.store(false, Ordering::Release);
                *self
                    .write_blocked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                Ok(summary)
            }
            Err(error) => {
                self.load_failed.store(true, Ordering::Release);
                // 加载失败时内存条目为空、id 计数从 1 起：必须单独确认库内最大条目 id，
                // 否则新条目会覆盖库内同 id 的行。仍确认不了就禁止产生需落库的新条目。
                match db.bt_guard_max_entry_id().await {
                    Ok(max_id) => {
                        self.next_id.fetch_max(max_id + 1, Ordering::AcqRel);
                    }
                    Err(id_error) => {
                        let reason = format!("load failed: {error:#}; max entry id: {id_error:#}");
                        crate::log_warn!("[BtGuard] writes blocked: {reason}");
                        *self
                            .write_blocked
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason);
                    }
                }
                Err(error)
            }
        }
    }

    async fn load_inner(&self, db: &Db) -> Result<LoadSummary, DbError> {
        let now = self.now();
        let entry_rows = db.bt_guard_load_entries(now).await?;
        let max_id = db.bt_guard_max_entry_id().await?;
        let strike_rows = db.bt_guard_load_strikes().await?;
        let ledger_rows = db.bt_guard_load_ledger(now - LEDGER_RETENTION_SECS).await?;
        let stored_meta = db.bt_guard_load_cloud_meta().await?;

        let mut summary = LoadSummary::default();
        let mut by_layer: HashMap<Layer, Vec<Entry>> = HashMap::new();
        let mut cloud_meta: Option<(i64, i64)> = None;
        for row in &entry_rows {
            match Entry::from_row(row) {
                Ok(entry) => {
                    if entry.layer == Layer::Cloud
                        && let Some((seq, exp)) = parse_cloud_source(&entry.source)
                    {
                        cloud_meta = Some(match cloud_meta {
                            Some((s, e)) => (s.max(seq), e.max(exp)),
                            None => (seq, exp),
                        });
                    }
                    by_layer.entry(entry.layer).or_default().push(entry);
                    summary.entries += 1;
                }
                Err(error) => {
                    summary.skipped_entries += 1;
                    crate::log_warn!("[BtGuard] skipped stored entry {}: {error}", row.id);
                }
            }
        }

        let mut st = self.lock();
        for layer in Layer::ALL {
            let entries = by_layer.remove(&layer).unwrap_or_default();
            if layer == Layer::Cloud {
                st.cloud.entries = entries.len();
            }
            self.set_layer(layer, entries);
        }
        if let Some((seq, exp)) = cloud_meta {
            st.cloud.sequence = seq;
            st.cloud.valid_until = exp;
            self.cloud_valid_until.store(exp, Ordering::Release);
        }
        // 文档元数据（sequence / 时间 / 署名 / clientRules）以独立存储为准；
        // 缺失（旧库）时沿用上面从条目来源恢复的 sequence / 有效期。
        if let Some(json) = stored_meta {
            match serde_json::from_str::<CloudMeta>(&json) {
                Ok(meta) => {
                    st.cloud.sequence = meta.sequence;
                    st.cloud.updated_at = meta.updated_at;
                    st.cloud.valid_until = meta.expires_at;
                    st.cloud.attribution = meta.attribution;
                    st.cloud.client_rules = meta.client_rules;
                    self.cloud_valid_until
                        .store(meta.expires_at, Ordering::Release);
                }
                Err(error) => crate::log_warn!("[BtGuard] ignored malformed cloud meta: {error}"),
            }
        }
        self.next_id.fetch_max(max_id + 1, Ordering::AcqRel);

        st.strikes.clear();
        for row in strike_rows {
            match row.ip_group.parse::<IpGroup>() {
                Ok(group) => {
                    st.strikes.insert(
                        group,
                        StrikeRec {
                            strikes: u32::try_from(row.strikes).unwrap_or(u32::MAX),
                            last_strike_at: row.last_strike_at,
                            ban_until: row.last_ban_until,
                        },
                    );
                    summary.strikes += 1;
                }
                Err(error) => crate::log_warn!("[BtGuard] skipped stored strike: {error}"),
            }
        }
        st.ledgers.clear();
        for row in ledger_rows {
            let (Some(hash), Ok(group)) =
                (parse_hash(&row.info_hash), row.ip_group.parse::<IpGroup>())
            else {
                crate::log_warn!("[BtGuard] skipped malformed stored ledger row");
                continue;
            };
            let delivered =
                PieceSet::from_msb_bytes(&row.delivered_bits, row.delivered_bits.len() * 8);
            let claimed = PieceSet::from_msb_bytes(&row.claimed_bits, row.claimed_bits.len() * 8);
            st.ledgers.entry(hash).or_default().insert(
                group,
                Ledger {
                    delivered_bytes: u64::try_from(row.delivered_bytes).unwrap_or(0),
                    dup_bytes: u64::try_from(row.dup_bytes).unwrap_or(0),
                    delivered,
                    last_claimed: claimed,
                    rerequests: u32::try_from(row.rerequests).unwrap_or(u32::MAX),
                    probed_at: row.probed_at,
                    first_seen: row.first_seen,
                    last_seen: row.last_seen,
                    probe_pending: None,
                    torrent_size: 0,
                    lag_cleared_at: row.lag_cleared_at,
                    excess_baseline: u64::try_from(row.excess_baseline).unwrap_or(0),
                    last_peer_prefix: None,
                    rewind_skip_since: 0,
                    rewind_skips: 0,
                },
            );
            summary.ledgers += 1;
        }
        st.pending.entry_ops.clear();
        st.pending.cloud_meta = None;
        st.pending.dirty_ledgers.clear();
        st.pending.evicted.clear();
        st.pending.dirty_strikes.clear();
        st.pending.removed_ledgers.clear();
        st.pending.disconnected.clear();
        st.pending.urgent = false;
        drop(st);
        self.bump_epoch();
        Ok(summary)
    }
}

fn parse_hash(text: &str) -> Option<InfoHash> {
    hex::decode(text).ok()?.try_into().ok()
}
