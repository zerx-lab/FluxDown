//! 任务详情「Tracker」页的筛选 / 排序纯模型（对齐 qBittorrent：表头排序 + 状态筛选），以及表首
//! DHT 伪行的展示意图。
//!
//! 与 Web `web/src/pages/downloads/detail/btStatus.ts::{trackerView, dhtRow}` 逐条对齐；筛选 /
//! 排序两端共用 `crates/downloads/tests/fixtures/bt_tracker_view_cases.json`。

use std::cmp::Ordering;

use fluxdown_protocol::{BtDhtState, BtStatusDto, BtTrackerDto, BtTrackerStatus};

/// DHT 伪行的展示意图：文案键 + 沿用 tracker 状态配色的语气。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DhtRow {
    /// 状态文案键。
    pub(crate) label: &'static str,
    /// 状态点 / 文字配色，与同语义的 tracker 状态一致。
    pub(crate) tone: BtTrackerStatus,
    /// 悬停说明的文案键。
    pub(crate) hint: Option<&'static str>,
    /// IPv4 / IPv6 路由表节点数；只在 DHT 运行且已有节点时给出。
    pub(crate) nodes: Option<(u32, u32)>,
}

impl DhtRow {
    const fn plain(label: &'static str, tone: BtTrackerStatus) -> Self {
        Self {
            label,
            tone,
            hint: None,
            nodes: None,
        }
    }

    const fn hinted(label: &'static str, tone: BtTrackerStatus, hint: &'static str) -> Self {
        Self {
            label,
            tone,
            hint: Some(hint),
            nodes: None,
        }
    }
}

/// DHT 伪行：旧 daemon（无 `dht`）或未知状态退回只看设置开关。
pub(crate) fn dht_row(status: &BtStatusDto) -> DhtRow {
    let by_setting = || {
        if status.dht_enabled {
            DhtRow::plain("btTrackerDhtOn", BtTrackerStatus::Working)
        } else {
            DhtRow::plain("btTrackerDhtOff", BtTrackerStatus::NotContacted)
        }
    };
    let Some(dht) = status.dht else {
        return by_setting();
    };
    match dht.state {
        BtDhtState::Disabled => DhtRow::plain("btTrackerDhtOff", BtTrackerStatus::NotContacted),
        BtDhtState::Private => DhtRow::hinted(
            "btTrackerDhtPrivate",
            BtTrackerStatus::NotContacted,
            "btTrackerDhtPrivateHint",
        ),
        BtDhtState::Idle => DhtRow::plain("btTrackerDhtOn", BtTrackerStatus::Working),
        BtDhtState::Unavailable => DhtRow::hinted(
            "btTrackerDhtUnavailable",
            BtTrackerStatus::NotWorking,
            "btTrackerDhtUnavailableHint",
        ),
        // 路由表空：刚启动正在引导，长时间如此通常是 UDP 被拦截。
        BtDhtState::Running if dht.nodes_v4 == 0 && dht.nodes_v6 == 0 => DhtRow::hinted(
            "btTrackerDhtConnecting",
            BtTrackerStatus::Updating,
            "btTrackerDhtConnectingHint",
        ),
        BtDhtState::Running => DhtRow {
            nodes: Some((dht.nodes_v4, dht.nodes_v6)),
            ..DhtRow::hinted(
                "btTrackerDhtOn",
                BtTrackerStatus::Working,
                "btTrackerDhtNodesHint",
            )
        },
        BtDhtState::Unknown => by_setting(),
    }
}

/// 节点数文案键与参数（`{n}` 总数、`{v4}` / `{v6}` 分项）：没有 IPv6 节点时只报总数。
pub(crate) fn dht_nodes_text((v4, v6): (u32, u32)) -> (&'static str, [String; 3]) {
    let key = if v6 == 0 {
        "btTrackerDhtNodes"
    } else {
        "btTrackerDhtNodesSplit"
    };
    (
        key,
        [
            v4.saturating_add(v6).to_string(),
            v4.to_string(),
            v6.to_string(),
        ],
    )
}

/// 状态筛选；`All` 含宿主不采集时的 `Unknown`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TrackerFilter {
    #[default]
    All,
    Working,
    Updating,
    NotWorking,
    NotContacted,
}

impl TrackerFilter {
    /// 标签行顺序（「全部」在首）。
    pub(crate) const ALL: [Self; 5] = [
        Self::All,
        Self::Working,
        Self::Updating,
        Self::NotWorking,
        Self::NotContacted,
    ];

    fn admits(self, status: BtTrackerStatus) -> bool {
        match self {
            Self::All => true,
            Self::Working => status == BtTrackerStatus::Working,
            Self::Updating => status == BtTrackerStatus::Updating,
            Self::NotWorking => status == BtTrackerStatus::NotWorking,
            Self::NotContacted => status == BtTrackerStatus::NotContacted,
        }
    }
}

/// 可排序的列（消息列不排序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackerSortKey {
    Url,
    Status,
    Peers,
    NextAnnounce,
    LastOk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackerSort {
    pub(crate) key: TrackerSortKey,
    pub(crate) desc: bool,
}

/// 表头点击三态：未排序 → 升序 → 降序 → 未排序；换列从升序开始。
pub(crate) fn next_sort(current: Option<TrackerSort>, key: TrackerSortKey) -> Option<TrackerSort> {
    match current {
        Some(sort) if sort.key == key && !sort.desc => Some(TrackerSort { key, desc: true }),
        Some(sort) if sort.key == key => None,
        _ => Some(TrackerSort { key, desc: false }),
    }
}

/// 各筛选项的计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrackerCounts {
    pub(crate) all: usize,
    pub(crate) working: usize,
    pub(crate) updating: usize,
    pub(crate) not_working: usize,
    pub(crate) not_contacted: usize,
}

impl TrackerCounts {
    pub(crate) fn of(trackers: &[BtTrackerDto]) -> Self {
        let mut counts = Self {
            all: trackers.len(),
            ..Self::default()
        };
        for tracker in trackers {
            match tracker.status {
                BtTrackerStatus::Working => counts.working += 1,
                BtTrackerStatus::Updating => counts.updating += 1,
                BtTrackerStatus::NotWorking => counts.not_working += 1,
                BtTrackerStatus::NotContacted => counts.not_contacted += 1,
                BtTrackerStatus::Unknown => {}
            }
        }
        counts
    }

    pub(crate) fn get(&self, filter: TrackerFilter) -> usize {
        match filter {
            TrackerFilter::All => self.all,
            TrackerFilter::Working => self.working,
            TrackerFilter::Updating => self.updating,
            TrackerFilter::NotWorking => self.not_working,
            TrackerFilter::NotContacted => self.not_contacted,
        }
    }
}

/// 升序名次：先看失败（首次点击状态列就把问题顶上来）。
fn status_rank(status: BtTrackerStatus) -> u8 {
    match status {
        BtTrackerStatus::NotWorking => 0,
        BtTrackerStatus::Updating => 1,
        BtTrackerStatus::Working => 2,
        BtTrackerStatus::NotContacted => 3,
        BtTrackerStatus::Unknown => 4,
    }
}

/// 缺省值不论升降序都垫底；有值时按 `desc` 决定方向。
fn present_first<T: Ord>(a: Option<T>, b: Option<T>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) if desc => b.cmp(&a),
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn positive(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

fn compare(a: &BtTrackerDto, b: &BtTrackerDto, sort: TrackerSort) -> Ordering {
    let desc = sort.desc;
    match sort.key {
        TrackerSortKey::Url => {
            let ordering = a
                .url
                .chars()
                .flat_map(char::to_lowercase)
                .cmp(b.url.chars().flat_map(char::to_lowercase));
            if desc { ordering.reverse() } else { ordering }
        }
        TrackerSortKey::Status => {
            let ordering = status_rank(a.status).cmp(&status_rank(b.status));
            if desc { ordering.reverse() } else { ordering }
        }
        TrackerSortKey::Peers => present_first(a.peers, b.peers, desc),
        TrackerSortKey::NextAnnounce => present_first(
            positive(a.next_announce_at),
            positive(b.next_announce_at),
            desc,
        ),
        // 升序 = 最近一次成功离现在最近（时间戳大）在前。
        TrackerSortKey::LastOk => {
            present_first(positive(a.last_ok_at), positive(b.last_ok_at), !desc)
        }
    }
}

/// 按筛选与排序得出要显示的 tracker 下标（稳定排序：平局保持服务端顺序）。
pub(crate) fn tracker_view(
    trackers: &[BtTrackerDto],
    filter: TrackerFilter,
    sort: Option<TrackerSort>,
) -> Vec<usize> {
    let mut view: Vec<(usize, &BtTrackerDto)> = trackers
        .iter()
        .enumerate()
        .filter(|(_, tracker)| filter.admits(tracker.status))
        .collect();
    if let Some(sort) = sort {
        view.sort_by(|(_, a), (_, b)| compare(a, b, sort));
    }
    view.into_iter().map(|(index, _)| index).collect()
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{BtStatusDto, BtTrackerDto, BtTrackerStatus};
    use serde::Deserialize;

    use super::{
        DhtRow, TrackerCounts, TrackerFilter, TrackerSort, TrackerSortKey, dht_nodes_text, dht_row,
        next_sort, tracker_view,
    };

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Counts {
        all: usize,
        working: usize,
        updating: usize,
        not_working: usize,
        not_contacted: usize,
    }

    #[derive(Deserialize)]
    struct Sort {
        key: String,
        desc: bool,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        filter: String,
        sort: Option<Sort>,
        expect: Vec<usize>,
    }

    #[derive(Deserialize)]
    struct Fixture {
        trackers: Vec<BtTrackerDto>,
        counts: Counts,
        cases: Vec<Case>,
    }

    fn filter_of(name: &str) -> TrackerFilter {
        match name {
            "all" => TrackerFilter::All,
            "working" => TrackerFilter::Working,
            "updating" => TrackerFilter::Updating,
            "notWorking" => TrackerFilter::NotWorking,
            "notContacted" => TrackerFilter::NotContacted,
            other => panic!("unknown filter {other}"),
        }
    }

    fn key_of(name: &str) -> TrackerSortKey {
        match name {
            "url" => TrackerSortKey::Url,
            "status" => TrackerSortKey::Status,
            "peers" => TrackerSortKey::Peers,
            "nextAnnounce" => TrackerSortKey::NextAnnounce,
            "lastOk" => TrackerSortKey::LastOk,
            other => panic!("unknown sort key {other}"),
        }
    }

    #[test]
    fn shared_fixture_counts_and_views() {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../tests/fixtures/bt_tracker_view_cases.json"
        ))
        .expect("tracker view fixture");
        let counts = TrackerCounts::of(&fixture.trackers);
        assert_eq!(
            (
                counts.all,
                counts.working,
                counts.updating,
                counts.not_working,
                counts.not_contacted
            ),
            (
                fixture.counts.all,
                fixture.counts.working,
                fixture.counts.updating,
                fixture.counts.not_working,
                fixture.counts.not_contacted
            )
        );
        for case in fixture.cases {
            let sort = case.sort.map(|sort| TrackerSort {
                key: key_of(&sort.key),
                desc: sort.desc,
            });
            assert_eq!(
                tracker_view(&fixture.trackers, filter_of(&case.filter), sort),
                case.expect,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn header_click_cycles_asc_desc_off_and_restarts_on_new_column() {
        let asc = next_sort(None, TrackerSortKey::Peers);
        assert_eq!(
            asc,
            Some(TrackerSort {
                key: TrackerSortKey::Peers,
                desc: false
            })
        );
        let desc = next_sort(asc, TrackerSortKey::Peers);
        assert_eq!(desc.map(|sort| sort.desc), Some(true));
        assert_eq!(next_sort(desc, TrackerSortKey::Peers), None);
        assert_eq!(
            next_sort(desc, TrackerSortKey::Url),
            Some(TrackerSort {
                key: TrackerSortKey::Url,
                desc: false
            })
        );
    }

    #[derive(Deserialize)]
    struct DhtExpect {
        label: String,
        tone: BtTrackerStatus,
        hint: Option<String>,
        nodes: Option<(u32, u32)>,
    }

    #[derive(Deserialize)]
    struct DhtCase {
        name: String,
        status: serde_json::Value,
        expect: DhtExpect,
    }

    #[derive(Deserialize)]
    struct NodesCase {
        nodes: (u32, u32),
        key: String,
        args: [String; 3],
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DhtFixture {
        rows: Vec<DhtCase>,
        nodes_text: Vec<NodesCase>,
    }

    #[test]
    fn shared_fixture_dht_row() {
        let fixture: DhtFixture =
            serde_json::from_str(include_str!("../../tests/fixtures/bt_dht_row_cases.json"))
                .expect("dht row fixture");
        for case in fixture.rows {
            // 夹具只写 DHT 相关字段，其余按旧 daemon 缺省补齐。
            let mut status = serde_json::json!({
                "peerCounts": {"live": 0, "connecting": 0, "queued": 0, "seen": 0, "dead": 0},
                "trackers": [],
                "peers": [],
            });
            if let (Some(base), Some(extra)) = (status.as_object_mut(), case.status.as_object()) {
                base.extend(extra.clone());
            }
            let status: BtStatusDto = serde_json::from_value(status).expect(&case.name);
            let DhtRow {
                label,
                tone,
                hint,
                nodes,
            } = dht_row(&status);
            assert_eq!(label, case.expect.label, "{}", case.name);
            assert_eq!(tone, case.expect.tone, "{}", case.name);
            assert_eq!(hint, case.expect.hint.as_deref(), "{}", case.name);
            assert_eq!(nodes, case.expect.nodes, "{}", case.name);
        }
        for case in fixture.nodes_text {
            assert_eq!(
                dht_nodes_text(case.nodes),
                (case.key.as_str(), case.args),
                "{:?}",
                case.nodes
            );
        }
    }
}
