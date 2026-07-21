//! 已配对设备名册持久化 —— `link_devices` 表的强类型门面。

use crate::db::{Db, LinkDeviceRow};

use super::error::LinkResult;
use super::types::{PeerCandidate, PeerRecord};

/// 本地设备名册（LinkStore）：把 [`PeerRecord`] 读写到引擎 `link_devices` 表，
/// 处理 `candidates` 的 JSON 序列化与空 platform 的 `Option` 归一。
#[derive(Clone)]
pub struct LinkStore {
    db: Db,
}

impl LinkStore {
    /// 绑定到引擎数据库。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 落库一条已配对设备（配对成功后调用）。
    pub async fn upsert(&self, record: &PeerRecord) -> LinkResult<()> {
        let candidates_json =
            serde_json::to_string(&record.candidates).unwrap_or_else(|_| "[]".to_string());
        self.db
            .link_upsert_device(
                &record.fingerprint,
                &record.identity_pub,
                &record.name,
                record.platform.as_deref().unwrap_or(""),
                &record.link_secret,
                &candidates_json,
                record.paired_at,
                record.last_seen_at,
            )
            .await?;
        Ok(())
    }

    /// 读取全部已配对设备（最近活跃降序）。
    pub async fn list(&self) -> LinkResult<Vec<PeerRecord>> {
        let rows = self.db.link_load_devices().await?;
        Ok(rows.into_iter().map(row_to_record).collect())
    }

    /// 按指纹读取单台设备（数据面链路鉴权查密钥用）。
    pub async fn get(&self, fingerprint: &str) -> LinkResult<Option<PeerRecord>> {
        Ok(self
            .db
            .link_load_device(fingerprint)
            .await?
            .map(row_to_record))
    }

    /// 解除配对（删除设备）。返回是否删到行。
    pub async fn remove(&self, fingerprint: &str) -> LinkResult<bool> {
        Ok(self.db.link_delete_device(fingerprint).await?)
    }

    /// 刷新最近活跃时间。
    pub async fn touch(&self, fingerprint: &str, at: i64) -> LinkResult<()> {
        self.db.link_touch_device(fingerprint, at).await?;
        Ok(())
    }
}

/// 把持久化行映射为强类型 [`PeerRecord`]。`candidates` JSON 解析失败退化为空列表
/// （不让单条坏数据毒死整份名册加载，与引擎其他「坏行降级」纪律一致）。
fn row_to_record(row: LinkDeviceRow) -> PeerRecord {
    let candidates: Vec<PeerCandidate> =
        serde_json::from_str(&row.candidates_json).unwrap_or_default();
    PeerRecord {
        fingerprint: row.fingerprint,
        identity_pub: row.identity_pub,
        name: row.name,
        platform: if row.platform.is_empty() {
            None
        } else {
            Some(row.platform)
        },
        link_secret: row.link_secret,
        candidates,
        paired_at: row.paired_at,
        last_seen_at: row.last_seen_at,
    }
}
