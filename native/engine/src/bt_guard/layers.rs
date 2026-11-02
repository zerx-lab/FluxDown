//! 过滤层：每层一个不可变 `LayerIndex`（条目 + 区间索引），整层重建后原子替换；
//! 到期条目在查询时惰性剔除，无定时器。

use std::net::IpAddr;
use std::sync::Arc;

use super::config::RuleMode;
use super::model::{Entry, FilterAction, Layer};
use super::ranges::RangeIndex;

#[derive(Debug, Default)]
pub struct LayerIndex {
    entries: Vec<Entry>,
    index: RangeIndex,
}

impl LayerIndex {
    pub fn new(entries: Vec<Entry>) -> Self {
        let index = RangeIndex::build(entries.iter().enumerate().map(|(i, e)| (e.range, i as u32)));
        Self { entries, index }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 对每个覆盖 `ip` 且在 `now` 未过期的条目调用 `f`。
    pub fn for_each_active_match(&self, ip: IpAddr, now: i64, mut f: impl FnMut(&Entry)) {
        if self.index.is_empty() {
            return;
        }
        self.index.for_each_match(ip, |i| {
            if let Some(entry) = self.entries.get(i as usize)
                && entry.is_active(now)
            {
                f(entry);
            }
        });
    }
}

/// 五层快照。每层 `Arc`，只重建变动的层。
#[derive(Debug, Clone)]
pub struct Tables {
    layers: [Arc<LayerIndex>; 5],
}

impl Default for Tables {
    fn default() -> Self {
        let empty = Arc::new(LayerIndex::default());
        Self {
            layers: std::array::from_fn(|_| Arc::clone(&empty)),
        }
    }
}

impl Tables {
    pub fn layer(&self, layer: Layer) -> &Arc<LayerIndex> {
        &self.layers[layer.slot()]
    }

    pub fn with_layer(&self, layer: Layer, entries: Vec<Entry>) -> Self {
        let mut next = self.clone();
        next.layers[layer.slot()] = Arc::new(LayerIndex::new(entries));
        next
    }
}

/// 过滤层判定输入（来自配置与云名单状态）。
#[derive(Clone, Copy, Debug)]
pub struct DecideInput {
    pub now: i64,
    pub cloud_mode: RuleMode,
    /// 云名单启用且文档未过期。
    pub cloud_active: bool,
    /// 云文档过期时刻（`cloud_active` 为真时参与缓存失效计算）。
    pub cloud_valid_until: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    None,
    /// 命中白名单：豁免一切检测与封禁。
    Exempt,
    Act {
        action: FilterAction,
        layer: Layer,
        entry_id: i64,
    },
    /// 命中云名单但 `cloud_mode = observe`：只记录。
    ObserveCloud {
        action: FilterAction,
        entry_id: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decided {
    pub decision: Decision,
    /// 该判定有效至（含）此时刻之前；到点须重算（`i64::MAX` = 不过期）。
    pub valid_until: i64,
}

fn exp(entry: &Entry) -> i64 {
    entry.expires_at.unwrap_or(i64::MAX)
}

/// 优先级：allow > manual > {auto, cloud, file}（后者取最严厉动作）。
pub fn decide(tables: &Tables, ip: IpAddr, input: DecideInput) -> Decided {
    let now = input.now;

    // allow：存在任一未过期命中即豁免；有效期取其中最晚到期者。
    let mut allow_until: Option<i64> = None;
    tables
        .layer(Layer::Allow)
        .for_each_active_match(ip, now, |e| {
            let t = exp(e);
            allow_until = Some(allow_until.map_or(t, |cur| cur.max(t)));
        });
    if let Some(until) = allow_until {
        return Decided {
            decision: Decision::Exempt,
            valid_until: until,
        };
    }

    // manual
    let mut best: Option<(FilterAction, i64)> = None;
    let mut until = i64::MAX;
    tables
        .layer(Layer::Manual)
        .for_each_active_match(ip, now, |e| {
            until = until.min(exp(e));
            if best.is_none_or(|(a, _)| e.action > a) {
                best = Some((e.action, e.id));
            }
        });
    if let Some((action, entry_id)) = best {
        return Decided {
            decision: Decision::Act {
                action,
                layer: Layer::Manual,
                entry_id,
            },
            valid_until: until,
        };
    }

    // auto / file 恒执行；cloud 受 mode 控制。
    let mut enforce_best: Option<(FilterAction, Layer, i64)> = None;
    let mut cloud_observed: Option<(FilterAction, i64)> = None;
    for layer in [Layer::Auto, Layer::File] {
        tables.layer(layer).for_each_active_match(ip, now, |e| {
            until = until.min(exp(e));
            if enforce_best.is_none_or(|(a, _, _)| e.action > a) {
                enforce_best = Some((e.action, layer, e.id));
            }
        });
    }
    let cloud_consulted = input.cloud_active && input.cloud_mode != RuleMode::Off;
    if cloud_consulted {
        until = until.min(input.cloud_valid_until);
        tables
            .layer(Layer::Cloud)
            .for_each_active_match(ip, now, |e| {
                until = until.min(exp(e));
                if input.cloud_mode == RuleMode::Enforce {
                    if enforce_best.is_none_or(|(a, _, _)| e.action > a) {
                        enforce_best = Some((e.action, Layer::Cloud, e.id));
                    }
                } else if cloud_observed.is_none_or(|(a, _)| e.action > a) {
                    cloud_observed = Some((e.action, e.id));
                }
            });
    }
    let decision = match (enforce_best, cloud_observed) {
        (Some((action, layer, entry_id)), _) => Decision::Act {
            action,
            layer,
            entry_id,
        },
        (None, Some((action, entry_id))) => Decision::ObserveCloud { action, entry_id },
        (None, None) => Decision::None,
    };
    Decided {
        decision,
        valid_until: until,
    }
}
