//! BT 防吸血（bt_guard）的持久化：过滤条目 / strike / 账本 / 事件环。
//!
//! 行类型是纯数据（不依赖 `bt` feature），由 `crate::bt_guard` 在内存模型与行之间转换。
//! 所有写入都经 [`BtGuardBatch`] 单事务提交——宿主在事件驱动点显式 flush，无周期定时器。

use sqlx::{AssertSqlSafe, Row};

use super::{Db, DbError};

/// 事件环保留条数。
pub const BT_GUARD_EVENT_RING: i64 = 5000;

/// `bt_guard_entries` 行。`cidr` 为规范文本（`1.2.3.4/32` / `2001:db8::/64`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtGuardEntryRow {
    pub id: i64,
    /// allow | manual | auto | cloud | file
    pub layer: String,
    pub cidr: String,
    /// allow | ban | choke | throttle
    pub action: String,
    pub rule: String,
    pub reason: String,
    /// 证据 JSON 摘要；无证据为空串。
    pub evidence: String,
    pub source: String,
    pub created_at: i64,
    /// `None` = 永久。
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtGuardStrikeRow {
    pub ip_group: String,
    pub strikes: i64,
    pub last_strike_at: i64,
    /// 最近一次封禁的到期时刻（0 = 无封禁）。衰减期从 `max(last_strike_at, last_ban_until)` 起算。
    pub last_ban_until: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtGuardLedgerRow {
    /// 40 位小写 hex。
    pub info_hash: String,
    pub ip_group: String,
    pub delivered_bytes: i64,
    pub dup_bytes: i64,
    /// 按分片 MSB-first 打包的位集。
    pub delivered_bits: Vec<u8>,
    pub claimed_bits: Vec<u8>,
    pub rerequests: i64,
    /// 0 = 从未探测。
    pub probed_at: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    /// 最近一次 lag 探测被补齐清除的时刻（0 = 无）。
    pub lag_cleared_at: i64,
    /// 上次 excessive 封禁时的 delivered_bytes 基线。
    pub excess_baseline: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtGuardEventRow {
    /// 写入时由数据库分配，入参忽略。
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub ip_group: String,
    pub info_hash: String,
    pub rule: String,
    pub mode: String,
    pub action: String,
    pub detail: String,
}

/// 条目层面的有序写操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtGuardEntryOp {
    Upsert(BtGuardEntryRow),
    Delete(i64),
    /// 整体替换某层（`source` 为 `Some` 时只替换该来源的行，用于 file 层按 URL 订阅）。
    ReplaceLayer {
        layer: String,
        source: Option<String>,
        rows: Vec<BtGuardEntryRow>,
    },
}

/// 一次 flush 的全部写入（单事务）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtGuardBatch {
    pub entry_ops: Vec<BtGuardEntryOp>,
    pub strikes: Vec<BtGuardStrikeRow>,
    pub strike_deletes: Vec<String>,
    pub ledgers: Vec<BtGuardLedgerRow>,
    /// `(info_hash, ip_group)`。
    pub ledger_deletes: Vec<(String, String)>,
    pub events: Vec<BtGuardEventRow>,
    /// 清理：到期条目、过旧账本 / strike（`now`、各截止时间均为 unix 秒）。
    pub prune: Option<BtGuardPrune>,
    /// 云名单文档元数据（JSON，单行 upsert）。
    pub cloud_meta: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtGuardPrune {
    pub now: i64,
    pub ledger_cutoff: i64,
    pub strike_cutoff: i64,
}

impl BtGuardBatch {
    pub fn is_empty(&self) -> bool {
        self.entry_ops.is_empty()
            && self.strikes.is_empty()
            && self.strike_deletes.is_empty()
            && self.ledgers.is_empty()
            && self.ledger_deletes.is_empty()
            && self.events.is_empty()
            && self.prune.is_none()
            && self.cloud_meta.is_none()
    }
}

/// 条目分页 / 筛选。
#[derive(Debug, Clone, Default)]
pub struct BtGuardEntryQuery {
    pub layer: Option<String>,
    pub rule: Option<String>,
    /// 对 cidr / reason / source 的不区分大小写子串匹配。
    pub query: Option<String>,
    /// `Some(now)`：只返回在 `now` 时刻未过期的条目。
    pub active_at: Option<i64>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Default)]
pub struct BtGuardEventQuery {
    pub rule: Option<String>,
    pub kind: Option<String>,
    /// 规范化后的 IP 组文本精确匹配（`1.2.3.4` / `2001:db8::/64`）。
    pub ip_group: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

enum Bind {
    Text(String),
    Int(i64),
}

fn like_pattern(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('%');
    for ch in raw.to_lowercase().chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

fn entry_row(row: &sqlx::any::AnyRow) -> Result<BtGuardEntryRow, DbError> {
    Ok(BtGuardEntryRow {
        id: row.try_get("id")?,
        layer: row.try_get("layer")?,
        cidr: row.try_get("cidr")?,
        action: row.try_get("action")?,
        rule: row.try_get("rule")?,
        reason: row.try_get("reason")?,
        evidence: row.try_get("evidence")?,
        source: row.try_get("source")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

const ENTRY_COLUMNS: &str =
    "id, layer, cidr, action, rule, reason, evidence, source, created_at, expires_at";

async fn upsert_entry(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    row: &BtGuardEntryRow,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO bt_guard_entries \
         (id, layer, cidr, action, rule, reason, evidence, source, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (id) DO UPDATE SET layer = excluded.layer, cidr = excluded.cidr, \
         action = excluded.action, rule = excluded.rule, reason = excluded.reason, \
         evidence = excluded.evidence, source = excluded.source, \
         created_at = excluded.created_at, expires_at = excluded.expires_at",
    )
    .bind(row.id)
    .bind(&row.layer)
    .bind(&row.cidr)
    .bind(&row.action)
    .bind(&row.rule)
    .bind(&row.reason)
    .bind(&row.evidence)
    .bind(&row.source)
    .bind(row.created_at)
    .bind(row.expires_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_ledger(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    row: &BtGuardLedgerRow,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO bt_guard_ledger \
         (info_hash, ip_group, delivered_bytes, dup_bytes, delivered_bits, claimed_bits, \
          rerequests, probed_at, first_seen, last_seen, lag_cleared_at, excess_baseline) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (info_hash, ip_group) DO UPDATE SET \
         delivered_bytes = excluded.delivered_bytes, dup_bytes = excluded.dup_bytes, \
         delivered_bits = excluded.delivered_bits, claimed_bits = excluded.claimed_bits, \
         rerequests = excluded.rerequests, probed_at = excluded.probed_at, \
         first_seen = excluded.first_seen, last_seen = excluded.last_seen, \
         lag_cleared_at = excluded.lag_cleared_at, excess_baseline = excluded.excess_baseline",
    )
    .bind(&row.info_hash)
    .bind(&row.ip_group)
    .bind(row.delivered_bytes)
    .bind(row.dup_bytes)
    .bind(row.delivered_bits.as_slice())
    .bind(row.claimed_bits.as_slice())
    .bind(row.rerequests)
    .bind(row.probed_at)
    .bind(row.first_seen)
    .bind(row.last_seen)
    .bind(row.lag_cleared_at)
    .bind(row.excess_baseline)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Db {
    /// 单事务提交一批 bt_guard 写入。失败整体回滚，调用方可原样重试（全部为幂等 upsert / 删除）。
    pub async fn bt_guard_apply_batch(&self, batch: &BtGuardBatch) -> Result<(), DbError> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for op in &batch.entry_ops {
            match op {
                BtGuardEntryOp::Upsert(row) => upsert_entry(&mut tx, row).await?,
                BtGuardEntryOp::Delete(id) => {
                    sqlx::query("DELETE FROM bt_guard_entries WHERE id = $1")
                        .bind(*id)
                        .execute(&mut *tx)
                        .await?;
                }
                BtGuardEntryOp::ReplaceLayer {
                    layer,
                    source,
                    rows,
                } => {
                    match source {
                        Some(source) => {
                            sqlx::query(
                                "DELETE FROM bt_guard_entries WHERE layer = $1 AND source = $2",
                            )
                            .bind(layer)
                            .bind(source)
                            .execute(&mut *tx)
                            .await?;
                        }
                        None => {
                            sqlx::query("DELETE FROM bt_guard_entries WHERE layer = $1")
                                .bind(layer)
                                .execute(&mut *tx)
                                .await?;
                        }
                    }
                    for row in rows {
                        upsert_entry(&mut tx, row).await?;
                    }
                }
            }
        }
        for row in &batch.strikes {
            sqlx::query(
                "INSERT INTO bt_guard_strikes (ip_group, strikes, last_strike_at, last_ban_until) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT (ip_group) DO UPDATE SET \
                 strikes = excluded.strikes, last_strike_at = excluded.last_strike_at, \
                 last_ban_until = excluded.last_ban_until",
            )
            .bind(&row.ip_group)
            .bind(row.strikes)
            .bind(row.last_strike_at)
            .bind(row.last_ban_until)
            .execute(&mut *tx)
            .await?;
        }
        for group in &batch.strike_deletes {
            sqlx::query("DELETE FROM bt_guard_strikes WHERE ip_group = $1")
                .bind(group)
                .execute(&mut *tx)
                .await?;
        }
        // 先删后写：同一批里既有「被淘汰账本的删除」又有「同键重建的新账本」时，新账本必须留下。
        for (info_hash, ip_group) in &batch.ledger_deletes {
            sqlx::query("DELETE FROM bt_guard_ledger WHERE info_hash = $1 AND ip_group = $2")
                .bind(info_hash)
                .bind(ip_group)
                .execute(&mut *tx)
                .await?;
        }
        for row in &batch.ledgers {
            upsert_ledger(&mut tx, row).await?;
        }
        if !batch.events.is_empty() {
            for row in &batch.events {
                sqlx::query(
                    "INSERT INTO bt_guard_events \
                     (at, kind, ip_group, info_hash, rule, mode, action, detail) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(row.at)
                .bind(&row.kind)
                .bind(&row.ip_group)
                .bind(&row.info_hash)
                .bind(&row.rule)
                .bind(&row.mode)
                .bind(&row.action)
                .bind(&row.detail)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "DELETE FROM bt_guard_events WHERE id <= COALESCE(\
                 (SELECT id FROM bt_guard_events ORDER BY id DESC LIMIT 1 OFFSET $1), 0)",
            )
            .bind(BT_GUARD_EVENT_RING)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(meta) = &batch.cloud_meta {
            sqlx::query(
                "INSERT INTO bt_guard_cloud_meta (id, data) VALUES (1, $1) \
                 ON CONFLICT (id) DO UPDATE SET data = excluded.data",
            )
            .bind(meta)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(prune) = batch.prune {
            sqlx::query(
                "DELETE FROM bt_guard_entries WHERE expires_at IS NOT NULL AND expires_at <= $1",
            )
            .bind(prune.now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM bt_guard_strikes WHERE last_strike_at < $1 AND last_ban_until < $1",
            )
            .bind(prune.strike_cutoff)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM bt_guard_ledger WHERE last_seen < $1")
                .bind(prune.ledger_cutoff)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 启动加载：`now` 时刻未过期的全部条目（按 id 升序）。
    pub async fn bt_guard_load_entries(&self, now: i64) -> Result<Vec<BtGuardEntryRow>, DbError> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM bt_guard_entries \
             WHERE expires_at IS NULL OR expires_at > $1 ORDER BY id ASC"
        );
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(now)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(entry_row).collect()
    }

    /// 当前最大条目 id（无条目为 0），供内存侧继续分配。
    pub async fn bt_guard_max_entry_id(&self) -> Result<i64, DbError> {
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM bt_guard_entries")
            .fetch_one(&self.pool)
            .await?;
        Ok(max.unwrap_or(0))
    }

    /// 条目分页 / 筛选（按 id 降序）。返回 `(total, items)`。
    pub async fn bt_guard_list_entries(
        &self,
        query: &BtGuardEntryQuery,
    ) -> Result<(i64, Vec<BtGuardEntryRow>), DbError> {
        let mut conds: Vec<String> = Vec::new();
        let mut binds: Vec<Bind> = Vec::new();
        if let Some(layer) = &query.layer {
            binds.push(Bind::Text(layer.clone()));
            conds.push(format!("layer = ${}", binds.len()));
        }
        if let Some(rule) = &query.rule {
            binds.push(Bind::Text(rule.clone()));
            conds.push(format!("rule = ${}", binds.len()));
        }
        if let Some(now) = query.active_at {
            binds.push(Bind::Int(now));
            conds.push(format!(
                "(expires_at IS NULL OR expires_at > ${})",
                binds.len()
            ));
        }
        if let Some(text) = query.query.as_deref().filter(|t| !t.is_empty()) {
            binds.push(Bind::Text(like_pattern(text)));
            let n = binds.len();
            conds.push(format!(
                "(lower(cidr) LIKE ${n} ESCAPE '\\' OR lower(reason) LIKE ${n} ESCAPE '\\' \
                 OR lower(source) LIKE ${n} ESCAPE '\\')"
            ));
        }
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };

        let count_sql = format!("SELECT COUNT(*) FROM bt_guard_entries{where_sql}");
        let mut count_q = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
        for bind in &binds {
            count_q = match bind {
                Bind::Text(text) => count_q.bind(text.clone()),
                Bind::Int(int) => count_q.bind(*int),
            };
        }
        let total = count_q.fetch_one(&self.pool).await?;

        let list_sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM bt_guard_entries{where_sql} \
             ORDER BY id DESC LIMIT ${} OFFSET ${}",
            binds.len() + 1,
            binds.len() + 2
        );
        let mut list_q = sqlx::query(AssertSqlSafe(list_sql));
        for bind in &binds {
            list_q = match bind {
                Bind::Text(text) => list_q.bind(text.clone()),
                Bind::Int(int) => list_q.bind(*int),
            };
        }
        let rows = list_q
            .bind(i64::from(query.limit))
            .bind(i64::from(query.offset))
            .fetch_all(&self.pool)
            .await?;
        Ok((total, rows.iter().map(entry_row).collect::<Result<_, _>>()?))
    }

    /// 读取云名单文档元数据 JSON（从未应用过为 `None`）。
    pub async fn bt_guard_load_cloud_meta(&self) -> Result<Option<String>, DbError> {
        let data: Option<String> =
            sqlx::query_scalar("SELECT data FROM bt_guard_cloud_meta WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        Ok(data)
    }

    pub async fn bt_guard_load_strikes(&self) -> Result<Vec<BtGuardStrikeRow>, DbError> {
        let rows = sqlx::query(
            "SELECT ip_group, strikes, last_strike_at, last_ban_until FROM bt_guard_strikes",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(BtGuardStrikeRow {
                    ip_group: row.try_get("ip_group")?,
                    strikes: row.try_get("strikes")?,
                    last_strike_at: row.try_get("last_strike_at")?,
                    last_ban_until: row.try_get("last_ban_until")?,
                })
            })
            .collect()
    }

    /// 加载 `last_seen >= since` 的账本行。
    pub async fn bt_guard_load_ledger(&self, since: i64) -> Result<Vec<BtGuardLedgerRow>, DbError> {
        let rows = sqlx::query(
            "SELECT info_hash, ip_group, delivered_bytes, dup_bytes, delivered_bits, claimed_bits, \
             rerequests, probed_at, first_seen, last_seen, lag_cleared_at, excess_baseline \
             FROM bt_guard_ledger WHERE last_seen >= $1",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(BtGuardLedgerRow {
                    info_hash: row.try_get("info_hash")?,
                    ip_group: row.try_get("ip_group")?,
                    delivered_bytes: row.try_get("delivered_bytes")?,
                    dup_bytes: row.try_get("dup_bytes")?,
                    delivered_bits: row.try_get("delivered_bits")?,
                    claimed_bits: row.try_get("claimed_bits")?,
                    rerequests: row.try_get("rerequests")?,
                    probed_at: row.try_get("probed_at")?,
                    first_seen: row.try_get("first_seen")?,
                    last_seen: row.try_get("last_seen")?,
                    lag_cleared_at: row.try_get("lag_cleared_at")?,
                    excess_baseline: row.try_get("excess_baseline")?,
                })
            })
            .collect()
    }

    /// 事件分页（按 id 降序）。返回 `(total, items)`。
    pub async fn bt_guard_list_events(
        &self,
        query: &BtGuardEventQuery,
    ) -> Result<(i64, Vec<BtGuardEventRow>), DbError> {
        let mut conds: Vec<String> = Vec::new();
        let mut binds: Vec<String> = Vec::new();
        if let Some(rule) = &query.rule {
            binds.push(rule.clone());
            conds.push(format!("rule = ${}", binds.len()));
        }
        if let Some(kind) = &query.kind {
            binds.push(kind.clone());
            conds.push(format!("kind = ${}", binds.len()));
        }
        if let Some(group) = &query.ip_group {
            binds.push(group.clone());
            conds.push(format!("ip_group = ${}", binds.len()));
        }
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        let count_sql = format!("SELECT COUNT(*) FROM bt_guard_events{where_sql}");
        let mut count_q = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
        for bind in &binds {
            count_q = count_q.bind(bind.clone());
        }
        let total = count_q.fetch_one(&self.pool).await?;
        let list_sql = format!(
            "SELECT id, at, kind, ip_group, info_hash, rule, mode, action, detail \
             FROM bt_guard_events{where_sql} ORDER BY id DESC LIMIT ${} OFFSET ${}",
            binds.len() + 1,
            binds.len() + 2
        );
        let mut list_q = sqlx::query(AssertSqlSafe(list_sql));
        for bind in &binds {
            list_q = list_q.bind(bind.clone());
        }
        let rows = list_q
            .bind(i64::from(query.limit))
            .bind(i64::from(query.offset))
            .fetch_all(&self.pool)
            .await?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(BtGuardEventRow {
                    id: row.try_get("id")?,
                    at: row.try_get("at")?,
                    kind: row.try_get("kind")?,
                    ip_group: row.try_get("ip_group")?,
                    info_hash: row.try_get("info_hash")?,
                    rule: row.try_get("rule")?,
                    mode: row.try_get("mode")?,
                    action: row.try_get("action")?,
                    detail: row.try_get("detail")?,
                })
            })
            .collect::<Result<Vec<_>, DbError>>()?;
        Ok((total, items))
    }
}
