//! 窗口 chrome 布局模型：顶栏 / 活动栏 / 状态栏六个区域各自摆放哪些条目、以什么顺序。
//!
//! 纯数据，不依赖渲染；Web `web/src/shell/chromeLayout.ts` 逐条镜像，两端共用
//! `crates/shell/tests/fixtures/chrome_layout_cases.json` 的用例。持久化值是
//! `{ 区域键: [条目 id…], "hidden"?: [条目 id…] }` 对象；读取时按目录归一化，未知 / 重复 / 放错区域的条目被丢弃，
//! 未摆放的条目按目录顺序插回默认区域，因此新增条目与旧偏好天然兼容。隐藏是摆放之外的独立标记：
//! 被隐藏的条目保留所在区域与位置（取消隐藏即回原位），固定条目（[`ChromeItemSpec::fixed`]）不可隐藏。

use gpui::SharedString;
use serde_json::{Map, Value};

/// 持久化值里隐藏条目列表的键。
const HIDDEN_KEY: &str = "hidden";

/// 布局区域，按规范顺序：顶栏左 / 右、活动栏上 / 下、状态栏左 / 右。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromeZone {
    TitleLeading,
    TitleTrailing,
    RailTop,
    RailBottom,
    StatusLeading,
    StatusTrailing,
}

impl ChromeZone {
    /// 规范顺序：归一化时同一 id 出现在多个区域，保留顺序在前的那个。
    pub const ALL: [Self; 6] = [
        Self::TitleLeading,
        Self::TitleTrailing,
        Self::RailTop,
        Self::RailBottom,
        Self::StatusLeading,
        Self::StatusTrailing,
    ];

    /// 持久化 / wire 键。
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::TitleLeading => "titleLeading",
            Self::TitleTrailing => "titleTrailing",
            Self::RailTop => "railTop",
            Self::RailBottom => "railBottom",
            Self::StatusLeading => "statusLeading",
            Self::StatusTrailing => "statusTrailing",
        }
    }

    /// 由持久化键解析区域；未知键为 `None`。
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|zone| zone.key() == key)
    }

    /// 区域所属的承载条（决定条目能否放入）。
    #[must_use]
    pub const fn family(self) -> ChromeFamily {
        match self {
            Self::TitleLeading | Self::TitleTrailing => ChromeFamily::Title,
            Self::RailTop | Self::RailBottom => ChromeFamily::Rail,
            Self::StatusLeading | Self::StatusTrailing => ChromeFamily::Status,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::TitleLeading => 0,
            Self::TitleTrailing => 1,
            Self::RailTop => 2,
            Self::RailBottom => 3,
            Self::StatusLeading => 4,
            Self::StatusTrailing => 5,
        }
    }
}

/// 承载条：顶栏、活动栏（48px 宽竖条）、状态栏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromeFamily {
    Title,
    Rail,
    Status,
}

impl ChromeFamily {
    /// 用例 / wire 键。
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Rail => "rail",
            Self::Status => "status",
        }
    }

    /// 由键解析承载条；未知键为 `None`。
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Title, Self::Rail, Self::Status]
            .into_iter()
            .find(|family| family.key() == key)
    }
}

/// 条目可进入的承载条集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChromeFamilies {
    pub title: bool,
    pub rail: bool,
    pub status: bool,
}

impl ChromeFamilies {
    /// 任意承载条。
    pub const ALL: Self = Self {
        title: true,
        rail: true,
        status: true,
    };
    /// 带文字的条目：放不进 48px 宽的活动栏。
    pub const TITLE_STATUS: Self = Self {
        title: true,
        rail: false,
        status: true,
    };
    /// 固定尺寸的自绘按钮（账户头像）：高于状态栏，不放进状态栏。
    pub const RAIL_TITLE: Self = Self {
        title: true,
        rail: true,
        status: false,
    };
    /// 页面专属的顶栏控件（搜索框、视图菜单）：只放在顶栏。
    pub const TITLE: Self = Self {
        title: true,
        rail: false,
        status: false,
    };

    /// 是否包含该承载条。
    #[must_use]
    pub const fn contains(self, family: ChromeFamily) -> bool {
        match family {
            ChromeFamily::Title => self.title,
            ChromeFamily::Rail => self.rail,
            ChromeFamily::Status => self.status,
        }
    }

    /// 加入一个承载条。
    #[must_use]
    pub const fn with(mut self, family: ChromeFamily) -> Self {
        match family {
            ChromeFamily::Title => self.title = true,
            ChromeFamily::Rail => self.rail = true,
            ChromeFamily::Status => self.status = true,
        }
        self
    }
}

/// 布局目录中的一个条目：稳定 id、默认区域、可进入的承载条、能否隐藏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeItemSpec {
    pub id: SharedString,
    pub default_zone: ChromeZone,
    pub families: ChromeFamilies,
    /// 能否被用户隐藏；固定入口（下载页、设置）为 `false`，保证始终有路回到它们。
    pub hideable: bool,
    /// 默认隐藏：存储布局里还没有摆放它（新用户、新增条目）时以隐藏状态加入，可在自定义布局里显示。
    pub default_hidden: bool,
}

impl ChromeItemSpec {
    /// 创建目录条目。
    pub fn new(
        id: impl Into<SharedString>,
        default_zone: ChromeZone,
        families: ChromeFamilies,
    ) -> Self {
        Self {
            id: id.into(),
            default_zone,
            families,
            hideable: true,
            default_hidden: false,
        }
    }

    /// 标记为固定条目：可移动，不可隐藏。
    #[must_use]
    pub fn fixed(mut self) -> Self {
        self.hideable = false;
        self
    }

    /// 标记为默认隐藏（仍可隐藏 / 显示）。
    #[must_use]
    pub fn hidden_by_default(mut self) -> Self {
        self.default_hidden = true;
        self
    }

    /// 能否放入该区域。
    #[must_use]
    pub const fn fits(&self, zone: ChromeZone) -> bool {
        self.families.contains(zone.family())
    }
}

/// 六个区域各自的条目序列（每个目录条目恰好出现一次）与被隐藏的条目（按目录顺序）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChromeLayout {
    zones: [Vec<SharedString>; 6],
    hidden: Vec<SharedString>,
}

impl ChromeLayout {
    /// 默认布局：各条目按目录顺序落在默认区域。
    #[must_use]
    pub fn default_for(catalog: &[ChromeItemSpec]) -> Self {
        Self::from_value(None, catalog)
    }

    /// 由持久化值归一化：非对象视为未设置；按规范区域顺序保留「字符串、在目录内、未摆放、
    /// 放得下」的条目，其余跳过；最后把未摆放的条目按目录顺序插回默认区域——排在该区域第一个
    /// 目录位置靠后的条目之前（没有则追加末尾），因此新增的内置入口不会落到插件入口之后。
    /// `hidden` 只保留目录内可隐藏的条目，按目录顺序去重；存储值里没摆放的默认隐藏条目也加入。
    #[must_use]
    pub fn from_value(stored: Option<&Value>, catalog: &[ChromeItemSpec]) -> Self {
        let mut layout = Self::default();
        if let Some(object) = stored.and_then(Value::as_object) {
            for zone in ChromeZone::ALL {
                let Some(entries) = object.get(zone.key()).and_then(Value::as_array) else {
                    continue;
                };
                for id in entries.iter().filter_map(Value::as_str) {
                    let Some(spec) = catalog.iter().find(|spec| spec.id.as_ref() == id) else {
                        continue;
                    };
                    if spec.fits(zone) && layout.zone_of(id).is_none() {
                        layout.zones[zone.index()].push(spec.id.clone());
                    }
                }
            }
            if let Some(hidden) = object.get(HIDDEN_KEY).and_then(Value::as_array) {
                layout.hidden = catalog
                    .iter()
                    .filter(|spec| {
                        spec.hideable
                            && hidden
                                .iter()
                                .any(|entry| entry.as_str() == Some(spec.id.as_ref()))
                    })
                    .map(|spec| spec.id.clone())
                    .collect();
            }
        }
        let stored_placed: Vec<SharedString> = layout.zones.iter().flatten().cloned().collect();
        for (index, spec) in catalog.iter().enumerate() {
            if layout.zone_of(&spec.id).is_some() {
                continue;
            }
            let zone = &mut layout.zones[spec.default_zone.index()];
            let at = zone
                .iter()
                .position(|id| catalog.iter().position(|s| s.id == *id) > Some(index))
                .unwrap_or(zone.len());
            zone.insert(at, spec.id.clone());
        }
        layout.hidden = catalog
            .iter()
            .filter(|spec| {
                spec.hideable
                    && (layout.hidden.contains(&spec.id)
                        || (spec.default_hidden && !stored_placed.contains(&spec.id)))
            })
            .map(|spec| spec.id.clone())
            .collect();
        layout
    }

    /// 持久化值：始终写出全部六个区域；有隐藏条目时再写 `hidden`。
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        for zone in ChromeZone::ALL {
            let items = self.zones[zone.index()]
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect();
            object.insert(zone.key().to_owned(), Value::Array(items));
        }
        if !self.hidden.is_empty() {
            let hidden = self
                .hidden
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect();
            object.insert(HIDDEN_KEY.to_owned(), Value::Array(hidden));
        }
        Value::Object(object)
    }

    /// 条目是否被隐藏。
    #[must_use]
    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|item| item.as_ref() == id)
    }

    /// 隐藏 / 取消隐藏 `id`；未知条目、固定条目或状态未变时不变。返回是否变化。
    pub fn set_hidden(&mut self, catalog: &[ChromeItemSpec], id: &str, hidden: bool) -> bool {
        let Some(spec) = catalog.iter().find(|spec| spec.id.as_ref() == id) else {
            return false;
        };
        if !spec.hideable || self.is_hidden(id) == hidden {
            return false;
        }
        if hidden {
            self.hidden.push(spec.id.clone());
            self.hidden.sort_by_key(|item| {
                catalog
                    .iter()
                    .position(|spec| spec.id == *item)
                    .unwrap_or(usize::MAX)
            });
        } else {
            self.hidden.retain(|item| item.as_ref() != id);
        }
        true
    }

    /// 区域内的条目，按显示顺序。
    #[must_use]
    pub fn items(&self, zone: ChromeZone) -> &[SharedString] {
        &self.zones[zone.index()]
    }

    /// 条目当前所在区域。
    #[must_use]
    pub fn zone_of(&self, id: &str) -> Option<ChromeZone> {
        ChromeZone::ALL.into_iter().find(|zone| {
            self.zones[zone.index()]
                .iter()
                .any(|item| item.as_ref() == id)
        })
    }

    /// 拖放：把 `id` 放到 `zone`。`target` 在该区域内时占据它的原位置（同区向后拖落在其后、
    /// 向前拖或跨区落在其前），否则追加到末尾。未知条目、放不下或拖到自身时不变；返回是否变化。
    pub fn move_item(
        &mut self,
        catalog: &[ChromeItemSpec],
        id: &str,
        zone: ChromeZone,
        target: Option<&str>,
    ) -> bool {
        let Some(spec) = catalog.iter().find(|spec| spec.id.as_ref() == id) else {
            return false;
        };
        if !spec.fits(zone) || target == Some(id) {
            return false;
        }
        let target_index = target.and_then(|target| {
            self.zones[zone.index()]
                .iter()
                .position(|item| item.as_ref() == target)
        });
        let before = self.clone();
        for items in &mut self.zones {
            items.retain(|item| item.as_ref() != id);
        }
        let items = &mut self.zones[zone.index()];
        match target_index {
            Some(index) => items.insert(index.min(items.len()), spec.id.clone()),
            None => items.push(spec.id.clone()),
        }
        *self != before
    }
}
