//! 热路径：`on_event` 的规则评估与处置阶梯。只改内存、入队待落库 / 待处置动作。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::bitset::PieceSet;
use super::config::{BtGuardConfig, DownloadingMaxAction, RuleMode};
use super::guard::{BtGuard, ban_duration_secs, mode_of};
use super::ip::{IpGroup, is_lan};
use super::layers::Decision;
use super::model::{
    Entry, EvidenceItem, FilterAction, GuardAction, GuardCtx, GuardEvent, Layer, Rule, Verdict,
    ip_of,
};
use super::state::{
    CONN_DELIVERED_CAP, Conn, EVIDENCE_CAP, LAG_CHECK_INTERVAL_SECS, LAG_GRACE_SECS,
    OBSERVE_GATE_SECS, PROBE_INTERVAL_SECS, REWIND_MIN_LOST_PIECES, STRIKE_DECAY_SECS, State,
    StrikeRec,
};
use crate::db::BtGuardEntryOp;

/// 一次规则命中（尚未按 mode 处置）。
struct Hit {
    rule: Rule,
    ratio_milli: u64,
    rewind_permille: u32,
    detail: String,
    /// 云 clientRule 指定的动作（仅 fingerprint）。
    soft_action: Option<FilterAction>,
    /// lag：对端缺失的分片，供探测往返比对。
    probe_pieces: Option<Vec<u32>>,
}

impl Hit {
    fn new(rule: Rule, detail: String) -> Self {
        Self {
            rule,
            ratio_milli: 0,
            rewind_permille: 0,
            detail,
            soft_action: None,
            probe_pieces: None,
        }
    }
}

/// 下载中任务的动作封顶：返回封顶后的动作；`None` = 仅观察。
fn cap_for_downloading(action: FilterAction, cap: DownloadingMaxAction) -> Option<FilterAction> {
    let limit = match cap {
        DownloadingMaxAction::Observe => return None,
        DownloadingMaxAction::Throttle => FilterAction::Throttle,
        DownloadingMaxAction::Choke => FilterAction::Choke,
        DownloadingMaxAction::Ban => FilterAction::Ban,
    };
    Some(action.min(limit))
}

fn expected_piece_len(ctx: &GuardCtx, piece: u32) -> u64 {
    if ctx.num_pieces == 0 || piece >= ctx.num_pieces {
        return 0;
    }
    if piece + 1 == ctx.num_pieces {
        let before = u64::from(piece) * u64::from(ctx.piece_length);
        ctx.total_length.saturating_sub(before).max(1)
    } else {
        u64::from(ctx.piece_length)
    }
}

fn peer_id_prefix(peer_id: Option<&[u8; 20]>) -> String {
    peer_id
        .and_then(|id| id.first_chunk::<8>())
        .filter(|prefix| prefix.iter().all(u8::is_ascii_graphic))
        .map(|prefix| String::from_utf8_lossy(prefix).into_owned())
        .unwrap_or_default()
}

impl BtGuard {
    pub fn on_event(&self, ctx: &GuardCtx, event: GuardEvent<'_>) -> Verdict {
        self.on_event_at(ctx, event, self.now())
    }

    /// 与 [`Self::on_event`] 相同，时间由调用方给定（unix 秒）。
    pub fn on_event_at(&self, ctx: &GuardCtx, event: GuardEvent<'_>, now: i64) -> Verdict {
        let ip = ip_of(ctx.addr);
        let key = (ctx.info_hash, SocketAddr::new(ip, ctx.addr.port()));
        if let GuardEvent::Disconnected { bits } = event {
            let mut st = self.lock();
            Self::on_disconnect(&mut st, ctx, key, bits, now);
            return Verdict::Allow;
        }
        // 热路径：先过无锁的 enabled 镜像与 LAN 豁免，再取全局锁。
        if !self.enabled.load(Ordering::Acquire) {
            return Verdict::Allow;
        }
        let cfg = self.config();
        if !cfg.enabled || (cfg.skip_lan && is_lan(ip)) {
            return Verdict::Allow;
        }
        let mut st = self.lock();
        let mut conn = st
            .conns
            .remove(&key)
            .unwrap_or_else(|| Conn::new(IpGroup::of(ip, cfg.ipv6_prefix), ip));
        let verdict = self.process(&mut st, &cfg, ctx, &mut conn, event, now);
        st.conns.insert(key, conn);
        verdict
    }

    fn on_disconnect(
        st: &mut State,
        ctx: &GuardCtx,
        key: super::state::ConnKey,
        bits: Option<&PieceSet>,
        now: i64,
    ) {
        let Some(conn) = st.conns.remove(&key) else {
            return;
        };
        let exists = st.ledger(&ctx.info_hash, &conn.group).is_some();
        if !exists {
            return;
        }
        let claimed = match bits {
            Some(b) => Some(b.clone()),
            None if conn.claimed.count_ones() > 0 => Some(conn.claimed.clone()),
            None => None,
        };
        let prefix = conn
            .peer_id
            .as_ref()
            .and_then(|id| id.first_chunk::<8>().copied());
        let ledger = st.ledger_mut(ctx.info_hash, conn.group, now, 0, ctx.num_pieces);
        // 「上次连接结束时对端拥有的分片」= 对端声明 ∪ 已送达满宽限期的分片。刚写进 socket
        // 的分片对端可能还没收到，不能拿来当 rewind 的比较基准。
        let mut merged = claimed.unwrap_or_else(|| ledger.last_claimed.clone());
        for (piece, at) in &conn.delivered_at {
            if now - *at >= LAG_GRACE_SECS {
                merged.set(*piece as usize);
            }
        }
        ledger.last_claimed = merged;
        if prefix.is_some() {
            ledger.last_peer_prefix = prefix;
        }
        // Disconnected 是落库驱动点之一：只把该组账本标记为「断开写入」。
        st.pending.disconnected.insert((ctx.info_hash, conn.group));
        st.wake = true;
    }

    fn process(
        &self,
        st: &mut State,
        cfg: &Arc<BtGuardConfig>,
        ctx: &GuardCtx,
        conn: &mut Conn,
        event: GuardEvent<'_>,
        now: i64,
    ) -> Verdict {
        conn.seeding = ctx.we_are_seeding;
        self.refresh_decision(cfg, conn, now);
        match conn.decision.decided.decision {
            Decision::Exempt => return Verdict::Allow,
            Decision::Act {
                action,
                layer,
                entry_id,
            } => self.apply_layer(cfg, conn, action, layer, entry_id, now),
            Decision::ObserveCloud { action, entry_id } => {
                self.observe_cloud(st, ctx, conn, action, entry_id, now);
            }
            Decision::None => {}
        }

        let mut hits: Vec<Hit> = Vec::new();
        let detect = !(ctx.private && cfg.skip_private) && conn.verdict != Verdict::Disconnect;
        if detect {
            self.detect(st, cfg, ctx, conn, event, now, &mut hits);
            for hit in hits {
                self.process_hit(st, cfg, ctx, conn, hit, now);
            }
        }
        if let GuardEvent::Request { length, .. } = event
            && conn.verdict >= Verdict::Throttle
        {
            self.stats_bump(now, |s| s.refused_bytes += u64::from(length));
        }
        conn.verdict
    }

    /// 应用过滤层判决（每个 (条目, 动作) 对每连接只计一次）。
    fn apply_layer(
        &self,
        cfg: &BtGuardConfig,
        conn: &mut Conn,
        action: FilterAction,
        layer: Layer,
        entry_id: i64,
        now: i64,
    ) {
        if conn.layer_applied == Some((entry_id, action)) {
            return;
        }
        let effective = if layer == Layer::Cloud && !conn.seeding && action != FilterAction::Ban {
            cap_for_downloading(action, cfg.downloading_max_action)
        } else {
            Some(action)
        };
        conn.layer_applied = Some((entry_id, action));
        let Some(effective) = effective else {
            return;
        };
        conn.raise(effective.verdict());
        if layer == Layer::Cloud {
            conn.hits.push((Rule::Cloud, true));
        }
        self.stats_bump(now, |s| s.enforced += 1);
    }

    fn observe_cloud(
        &self,
        st: &mut State,
        ctx: &GuardCtx,
        conn: &mut Conn,
        action: FilterAction,
        entry_id: i64,
        now: i64,
    ) {
        if conn.fired & Rule::Cloud.bit() != 0 {
            return;
        }
        conn.fired |= Rule::Cloud.bit();
        conn.hits.push((Rule::Cloud, false));
        self.observe_record(
            st,
            ctx,
            conn,
            Rule::Cloud,
            action.as_str(),
            json!({"entryId": entry_id}).to_string(),
            now,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_record(
        &self,
        st: &mut State,
        ctx: &GuardCtx,
        conn: &Conn,
        rule: Rule,
        action: &str,
        detail: String,
        now: i64,
    ) {
        let gate_key = (conn.group, rule);
        if let Some(last) = st.observed_gate.get(&gate_key)
            && now - *last < OBSERVE_GATE_SECS
        {
            return;
        }
        st.observed_gate.insert(gate_key, now);
        st.pending.urgent = true;
        self.stats_bump(now, |s| s.observed += 1);
        st.record_event(
            now,
            "observed",
            Some(conn.group),
            Some(&ctx.info_hash),
            rule.as_str(),
            RuleMode::Observe.as_str(),
            action,
            detail,
        );
        st.push_action(GuardAction::Changed);
    }

    // ------------------------------------------------------------ 规则检测

    #[allow(clippy::too_many_arguments)]
    fn detect(
        &self,
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &mut Conn,
        event: GuardEvent<'_>,
        now: i64,
        hits: &mut Vec<Hit>,
    ) {
        match event {
            GuardEvent::Handshake { peer_id, .. } => {
                conn.peer_id = Some(peer_id);
                hits.extend(self.fingerprint_hit(st, cfg, conn, now));
            }
            GuardEvent::ExtendedHandshake { client } => {
                conn.client = client.map(|c| c.chars().take(64).collect());
                hits.extend(self.fingerprint_hit(st, cfg, conn, now));
            }
            GuardEvent::Bitfield { bits } => {
                conn.claimed = bits.clone();
                self.bitfield_hits(st, cfg, ctx, conn, bits, now, hits);
            }
            GuardEvent::Have { piece } => {
                if piece < ctx.num_pieces {
                    conn.claimed.set(piece as usize);
                }
                hits.extend(Self::lag_hit(cfg, conn, now));
            }
            GuardEvent::Request {
                piece,
                peer_claims_piece,
                ..
            } => {
                if peer_claims_piece
                    && piece < ctx.num_pieces
                    && conn.claimed_requests.insert(piece)
                {
                    let ledger = st.ledger_mut(
                        ctx.info_hash,
                        conn.group,
                        now,
                        ctx.total_length,
                        ctx.num_pieces,
                    );
                    ledger.rerequests = ledger.rerequests.saturating_add(1);
                    if conn.claimed_requests.len() >= 2 && cfg.rule_rerequest != RuleMode::Off {
                        hits.push(Hit::new(
                            Rule::Rerequest,
                            format!(
                                "{} claimed pieces re-requested",
                                conn.claimed_requests.len()
                            ),
                        ));
                    }
                }
                hits.extend(Self::lag_hit(cfg, conn, now));
            }
            GuardEvent::Cancel { .. } => {}
            GuardEvent::Uploaded {
                piece,
                begin,
                length,
            } => {
                let expected = expected_piece_len(ctx, piece);
                let ledger = st.ledger_mut(
                    ctx.info_hash,
                    conn.group,
                    now,
                    ctx.total_length,
                    ctx.num_pieces,
                );
                ledger.delivered_bytes = ledger.delivered_bytes.saturating_add(u64::from(length));
                if expected > 0 {
                    if ledger.delivered.get(piece as usize) {
                        ledger.dup_bytes = ledger.dup_bytes.saturating_add(u64::from(length));
                    } else {
                        let progress = conn.progress.entry(piece).or_default();
                        // 按 (piece, begin) 去重：同一块重发不能提前凑满一片（也不算 dup，
                        // dup 只统计已完整送达分片的重复字节）。
                        let completed = if progress.begins.insert(begin) {
                            progress.bytes = progress.bytes.saturating_add(length);
                            u64::from(progress.bytes) >= expected
                        } else {
                            false
                        };
                        if completed {
                            conn.progress.remove(&piece);
                            ledger.delivered.set(piece as usize);
                            conn.delivered_at.push((piece, now));
                            if conn.delivered_at.len() > CONN_DELIVERED_CAP {
                                let drop = conn.delivered_at.len() / 2;
                                conn.delivered_at.drain(..drop);
                            }
                        }
                    }
                }
                if cfg.rule_excessive != RuleMode::Off
                    && ctx.total_length > 0
                    && ctx.total_length >= cfg.min_bytes()
                {
                    // 只看超出上次封禁基线的增量：解封后不会因为历史累计一个块就再封。
                    let increment = ledger
                        .delivered_bytes
                        .saturating_sub(ledger.excess_baseline);
                    let basis = u128::from(ctx.total_length.max(cfg.min_bytes()));
                    if u128::from(increment) * 100 > basis * u128::from(cfg.excess_ratio_pct) {
                        let ratio = u128::from(increment) * 1000 / u128::from(ctx.total_length);
                        let mut hit = Hit::new(
                            Rule::Excessive,
                            format!("delivered {}‰ of torrent size", ratio),
                        );
                        hit.ratio_milli = u64::try_from(ratio).unwrap_or(u64::MAX);
                        hits.push(hit);
                    }
                }
                hits.extend(Self::lag_hit(cfg, conn, now));
            }
            GuardEvent::Disconnected { .. } => {}
        }
    }

    fn fingerprint_hit(
        &self,
        st: &State,
        cfg: &BtGuardConfig,
        conn: &Conn,
        now: i64,
    ) -> Option<Hit> {
        if cfg.rule_fingerprint == RuleMode::Off || !self.decide_input(cfg, now).cloud_active {
            return None;
        }
        let rule = st.cloud.client_rules.iter().find(|r| {
            (!r.seeding_only || conn.seeding)
                && r.matches(conn.peer_id.as_ref(), conn.client.as_deref())
        })?;
        let mut hit = Hit::new(Rule::Fingerprint, format!("client rule {}", rule.id));
        hit.soft_action = Some(rule.action);
        Some(hit)
    }

    #[allow(clippy::too_many_arguments)]
    fn bitfield_hits(
        &self,
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &Conn,
        bits: &PieceSet,
        now: i64,
        hits: &mut Vec<Hit>,
    ) {
        // 该组在同一种子上还有其它存活连接（NAT / 共享 /64 下的第二个客户端）：
        // 新连接的位图不能代表「刚才那个客户端回退了」。
        let others_alive = st
            .conns
            .iter()
            .any(|(k, c)| k.0 == ctx.info_hash && c.group == conn.group);
        let prefix = conn
            .peer_id
            .as_ref()
            .and_then(|id| id.first_chunk::<8>().copied());
        let Some(ledger) = st
            .ledgers
            .get_mut(&ctx.info_hash)
            .and_then(|m| m.get_mut(&conn.group))
        else {
            return;
        };
        ledger.last_seen = now;
        // 空位图且客户端指纹与上次不同：多半是同 IP 组下换了个客户端，不是回退。
        let other_client_empty = bits.count_ones() == 0 && prefix != ledger.last_peer_prefix;
        let skip_reason = if others_alive {
            Some("another live connection of the group")
        } else if other_client_empty {
            Some("empty bitfield from a different client")
        } else {
            None
        };
        let mut rewind: Option<(u32, String)> = None;
        if cfg.rule_rewind != RuleMode::Off {
            let previous_n = ledger.last_claimed.count_ones();
            if ledger.delivered.count_ones() >= 1 && previous_n > 0 {
                let lost = ledger.last_claimed.count_missing_from(bits);
                if lost >= REWIND_MIN_LOST_PIECES
                    && (lost as u64) * 100 > u64::from(cfg.rewind_tolerance_pct) * previous_n as u64
                {
                    let permille = u32::try_from(lost * 1000 / previous_n).unwrap_or(u32::MAX);
                    rewind = Some((
                        permille,
                        format!("bitfield lost {lost}/{previous_n} previously held pieces"),
                    ));
                }
            }
        }
        if let Some(reason) = skip_reason {
            // NAT 保护：这次位图看起来像回退，但可能是另一个客户端。每个账本 24h 内最多跳过
            // REWIND_SKIP_QUOTA 次（两种跳过条件共用），否则对端可用一条锚定连接 / 换指纹稳定绕过。
            let Some((permille, _)) = &rewind else {
                return;
            };
            let permille = *permille;
            if let Some(left) = ledger.try_skip_rewind(now) {
                st.record_event(
                    now,
                    "observed",
                    Some(conn.group),
                    Some(&ctx.info_hash),
                    Rule::Rewind.as_str(),
                    RuleMode::Observe.as_str(),
                    "skip",
                    json!({"skipped": reason, "rewindPermille": permille, "skipsLeft": left})
                        .to_string(),
                );
                return;
            }
        }
        let probe = ledger.probe_pending.take();
        if cfg.rule_rewind != RuleMode::Off
            && rewind.is_none()
            && let Some(pending) = &probe
            && !pending.is_empty()
        {
            let still_missing = pending.iter().filter(|p| !bits.get(**p as usize)).count();
            if still_missing * 2 > pending.len() {
                rewind = Some((
                    u32::try_from(still_missing * 1000 / pending.len()).unwrap_or(u32::MAX),
                    format!(
                        "probe failed: {still_missing}/{} pieces still missing after reconnect",
                        pending.len()
                    ),
                ));
            }
        }
        // 未评估探测结果（rewind 规则关闭）时不能当作「补齐」授予冷却。
        let cleared = cfg.rule_rewind != RuleMode::Off && probe.is_some() && rewind.is_none();
        if cleared {
            // 探测补齐：写入清除时刻，冷却期内同组的 lag 只观察不处置。
            ledger.lag_cleared_at = now;
            st.pending.dirty_ledgers.insert((ctx.info_hash, conn.group));
            st.pending.urgent = true;
        }
        if let Some((permille, detail)) = rewind {
            let mut hit = Hit::new(Rule::Rewind, detail);
            hit.rewind_permille = permille;
            hits.push(hit);
        }
        if cleared {
            st.record_event(
                now,
                "probe_cleared",
                Some(conn.group),
                Some(&ctx.info_hash),
                Rule::Lag.as_str(),
                mode_of(cfg, Rule::Lag).as_str(),
                "",
                String::new(),
            );
        }
    }

    /// lag：送达且已过宽限的分片中超过一半未出现在对端位图。节流评估。
    fn lag_hit(cfg: &BtGuardConfig, conn: &mut Conn, now: i64) -> Option<Hit> {
        if cfg.rule_lag == RuleMode::Off
            || conn.fired & Rule::Lag.bit() != 0
            || now - conn.last_lag_check < LAG_CHECK_INTERVAL_SECS
        {
            return None;
        }
        conn.last_lag_check = now;
        let cutoff = now - LAG_GRACE_SECS;
        let aged = conn.delivered_at.partition_point(|(_, t)| *t <= cutoff);
        if aged == 0 {
            return None;
        }
        let aged_slice = conn.delivered_at.get(..aged).unwrap_or_default();
        let missing: Vec<u32> = aged_slice
            .iter()
            .filter(|(p, _)| !conn.claimed.get(*p as usize))
            .map(|(p, _)| *p)
            .collect();
        if missing.len() * 2 <= aged {
            return None;
        }
        let mut hit = Hit::new(
            Rule::Lag,
            format!(
                "{}/{} aged pieces absent from peer bitfield",
                missing.len(),
                aged
            ),
        );
        hit.probe_pieces = Some(missing);
        Some(hit)
    }

    // ------------------------------------------------------------ 处置

    fn process_hit(
        &self,
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &mut Conn,
        hit: Hit,
        now: i64,
    ) {
        let mode = mode_of(cfg, hit.rule);
        if mode == RuleMode::Off || conn.fired & hit.rule.bit() != 0 {
            return;
        }
        conn.fired |= hit.rule.bit();
        Self::queue_evidence(st, cfg, ctx, conn, &hit, now);
        // lag 探测刚被补齐清除：冷却期内只观察、不处置（否则补齐后的新连接会被直接 choke）。
        let cooling_down = hit.rule == Rule::Lag
            && st.ledger(&ctx.info_hash, &conn.group).is_some_and(|l| {
                l.lag_cleared_at != 0 && now - l.lag_cleared_at < PROBE_INTERVAL_SECS
            });
        match mode {
            RuleMode::Off => {}
            RuleMode::Observe | RuleMode::Enforce if cooling_down => {
                conn.hits.push((hit.rule, false));
                let detail = self.hit_json(st, ctx, conn, &hit, None);
                self.observe_record(st, ctx, conn, hit.rule, "cooldown", detail, now);
            }
            RuleMode::Observe => {
                conn.hits.push((hit.rule, false));
                let label = self.intended_label(st, cfg, ctx, conn, &hit, now);
                let detail = self.hit_json(st, ctx, conn, &hit, None);
                self.observe_record(st, ctx, conn, hit.rule, &label, detail, now);
            }
            RuleMode::Enforce => self.enforce(st, cfg, ctx, conn, hit, now),
        }
    }

    fn can_probe(st: &State, ctx: &GuardCtx, conn: &Conn, now: i64) -> bool {
        st.ledger(&ctx.info_hash, &conn.group)
            .is_some_and(|l| l.probed_at == 0 || now - l.probed_at >= PROBE_INTERVAL_SECS)
    }

    fn soft_action(cfg: &BtGuardConfig, conn: &Conn, hit: &Hit) -> Option<FilterAction> {
        let wanted = if conn.seeding {
            hit.soft_action.unwrap_or(FilterAction::Choke)
        } else {
            hit.soft_action
                .unwrap_or(FilterAction::Throttle)
                .min(FilterAction::Throttle)
        };
        if conn.seeding {
            Some(wanted)
        } else {
            cap_for_downloading(wanted, cfg.downloading_max_action)
        }
    }

    /// observe 模式下「本应执行」的动作标签。
    fn intended_label(
        &self,
        st: &State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &Conn,
        hit: &Hit,
        now: i64,
    ) -> String {
        match hit.rule.strength() {
            super::model::Strength::Strong if conn.seeding => FilterAction::Ban.as_str().to_owned(),
            super::model::Strength::Strong => {
                cap_for_downloading(FilterAction::Ban, cfg.downloading_max_action)
                    .map_or("observe", FilterAction::as_str)
                    .to_owned()
            }
            super::model::Strength::Medium if Self::can_probe(st, ctx, conn, now) => {
                "probe".to_owned()
            }
            _ => Self::soft_action(cfg, conn, hit)
                .map_or("observe", FilterAction::as_str)
                .to_owned(),
        }
    }

    fn hit_json(
        &self,
        st: &State,
        ctx: &GuardCtx,
        conn: &Conn,
        hit: &Hit,
        strikes: Option<u32>,
    ) -> String {
        let ledger = st.ledger(&ctx.info_hash, &conn.group);
        json!({
            "rule": hit.rule.as_str(),
            "detail": hit.detail,
            "torrentSize": ctx.total_length,
            "deliveredBytes": ledger.map_or(0, |l| l.delivered_bytes),
            "dupBytes": ledger.map_or(0, |l| l.dup_bytes),
            "ratioMilli": hit.ratio_milli,
            "rerequests": ledger.map_or(0, |l| l.rerequests),
            "rewindPermille": hit.rewind_permille,
            "strikes": strikes,
        })
        .to_string()
    }

    fn queue_evidence(
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &Conn,
        hit: &Hit,
        now: i64,
    ) {
        if !cfg.report_enabled || ctx.private || hit.rule == Rule::Cloud {
            return;
        }
        if !st.reported.insert((conn.group, hit.rule, ctx.info_hash)) {
            return;
        }
        if st.reported.len() > 100_000 {
            st.reported.clear();
        }
        let ledger = st.ledger(&ctx.info_hash, &conn.group);
        let item = EvidenceItem {
            ip_group: conn.group.to_string(),
            rule: hit.rule.as_str().to_owned(),
            strength: hit.rule.strength().as_str().to_owned(),
            info_hash: hex::encode(ctx.info_hash),
            torrent_size: ctx.total_length,
            delivered_bytes: ledger.map_or(0, |l| l.delivered_bytes),
            dup_bytes: ledger.map_or(0, |l| l.dup_bytes),
            ratio_milli: hit.ratio_milli,
            rerequests: ledger.map_or(0, |l| l.rerequests),
            rewind_permille: hit.rewind_permille,
            peer_id_prefix: peer_id_prefix(conn.peer_id.as_ref()),
            client: conn.client.clone().unwrap_or_default(),
            first_seen: ledger.map_or(now, |l| l.first_seen),
            last_seen: now,
        };
        st.evidence.push_back(item);
        while st.evidence.len() > EVIDENCE_CAP {
            st.evidence.pop_front();
        }
        st.push_action(GuardAction::EvidenceReady);
    }

    fn enforce(
        &self,
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &mut Conn,
        hit: Hit,
        now: i64,
    ) {
        match hit.rule.strength() {
            super::model::Strength::Strong => {
                if conn.seeding || cfg.downloading_max_action == DownloadingMaxAction::Ban {
                    self.ban_group(st, cfg, ctx, conn, &hit, now);
                } else {
                    let capped = cap_for_downloading(FilterAction::Ban, cfg.downloading_max_action);
                    self.apply_soft(st, ctx, conn, &hit, capped, now);
                }
            }
            super::model::Strength::Medium => {
                if Self::can_probe(st, ctx, conn, now) {
                    self.probe(st, ctx, conn, hit, now);
                } else {
                    let action = Self::soft_action(cfg, conn, &hit);
                    self.apply_soft(st, ctx, conn, &hit, action, now);
                }
            }
            super::model::Strength::Weak => {
                let action = Self::soft_action(cfg, conn, &hit);
                self.apply_soft(st, ctx, conn, &hit, action, now);
            }
        }
    }

    /// 非封禁动作；`None` = 被下载上限压成仅观察。
    fn apply_soft(
        &self,
        st: &mut State,
        ctx: &GuardCtx,
        conn: &mut Conn,
        hit: &Hit,
        action: Option<FilterAction>,
        now: i64,
    ) {
        let Some(action) = action else {
            conn.hits.push((hit.rule, false));
            let detail = self.hit_json(st, ctx, conn, hit, None);
            self.observe_record(st, ctx, conn, hit.rule, "capped", detail, now);
            return;
        };
        conn.hits.push((hit.rule, true));
        conn.raise(action.verdict());
        st.pending.urgent = true;
        self.stats_bump(now, |s| s.enforced += 1);
        let detail = self.hit_json(st, ctx, conn, hit, None);
        st.record_event(
            now,
            "enforced",
            Some(conn.group),
            Some(&ctx.info_hash),
            hit.rule.as_str(),
            RuleMode::Enforce.as_str(),
            action.as_str(),
            detail,
        );
        st.push_action(GuardAction::Changed);
    }

    /// lag 探测：断开一次并标记，等待对方重连后的 Bitfield 裁决。
    fn probe(&self, st: &mut State, ctx: &GuardCtx, conn: &mut Conn, hit: Hit, now: i64) {
        let pieces = hit.probe_pieces.clone();
        let ledger = st.ledger_mut(
            ctx.info_hash,
            conn.group,
            now,
            ctx.total_length,
            ctx.num_pieces,
        );
        ledger.probed_at = now;
        ledger.probe_pending = pieces;
        conn.hits.push((hit.rule, true));
        conn.raise(Verdict::Disconnect);
        st.pending.urgent = true;
        self.stats_bump(now, |s| s.enforced += 1);
        let detail = self.hit_json(st, ctx, conn, &hit, None);
        st.record_event(
            now,
            "probe",
            Some(conn.group),
            Some(&ctx.info_hash),
            hit.rule.as_str(),
            RuleMode::Enforce.as_str(),
            "probe",
            detail,
        );
        st.push_action(GuardAction::Changed);
    }

    /// 强信号封禁：strike +1，时长 `base × 4^(n-1)` 封顶，写入 auto 层。
    fn ban_group(
        &self,
        st: &mut State,
        cfg: &BtGuardConfig,
        ctx: &GuardCtx,
        conn: &mut Conn,
        hit: &Hit,
        now: i64,
    ) {
        conn.hits.push((hit.rule, true));
        conn.raise(Verdict::Disconnect);
        // 该 IP 已被某层封禁：只断开，不再追加 strike / 条目。
        let already = matches!(
            self.decide_ip(cfg, conn.ip, now).decision,
            Decision::Act {
                action: FilterAction::Ban,
                ..
            }
        );
        if already {
            return;
        }
        let rec = st.strikes.entry(conn.group).or_insert(StrikeRec {
            strikes: 0,
            last_strike_at: now,
            ban_until: 0,
        });
        // 衰减从「最近一次封禁到期时刻」起算：封顶封禁到期后立即再犯仍然升档。
        if now - rec.decay_from() >= STRIKE_DECAY_SECS {
            rec.strikes = 0;
        }
        rec.strikes = rec.strikes.saturating_add(1);
        rec.last_strike_at = now;
        let strikes = rec.strikes;
        let duration = ban_duration_secs(cfg.ban_base_hours, cfg.ban_max_days, strikes);
        rec.ban_until = now.saturating_add(duration);
        st.pending.dirty_strikes.insert(conn.group);
        st.pending.urgent = true;
        if hit.rule == Rule::Excessive
            && let Some(ledger) = st
                .ledgers
                .get_mut(&ctx.info_hash)
                .and_then(|m| m.get_mut(&conn.group))
        {
            // 封禁时记录基线：解封后再犯只看超出基线的增量。
            ledger.excess_baseline = ledger.delivered_bytes;
            st.pending.dirty_ledgers.insert((ctx.info_hash, conn.group));
        }

        let detail = self.hit_json(st, ctx, conn, hit, Some(strikes));
        let entry = Entry {
            id: self.alloc_id(),
            layer: Layer::Auto,
            range: conn.group.range(),
            action: FilterAction::Ban,
            rule: Arc::from(hit.rule.as_str()),
            reason: Arc::from(hit.detail.as_str()),
            evidence: Some(Arc::from(detail.as_str())),
            source: Arc::from("auto"),
            created_at: now,
            expires_at: Some(now.saturating_add(duration)),
        };
        let mut entries = self.layer_entries(Layer::Auto);
        entries.push(entry.clone());
        self.set_layer(Layer::Auto, entries);
        st.pending
            .entry_ops
            .push(BtGuardEntryOp::Upsert(entry.to_row()));
        self.stats_bump(now, |s| s.enforced += 1);
        st.record_event(
            now,
            "enforced",
            Some(conn.group),
            Some(&ctx.info_hash),
            hit.rule.as_str(),
            RuleMode::Enforce.as_str(),
            FilterAction::Ban.as_str(),
            detail,
        );
        crate::log_info!(
            "[BtGuard] banned {} rule={} strikes={} for {}s",
            conn.group,
            hit.rule.as_str(),
            strikes,
            duration
        );
        st.push_action(GuardAction::DisconnectBanned);
        st.push_action(GuardAction::Changed);
    }
}
