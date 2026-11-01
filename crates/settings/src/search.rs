//! 设置项搜索索引：把设置窗口的分类 / 子 Tab / 行拍平成可定位条目，供全局命令面板检索。
//!
//! 索引与设置窗口共用同一份页面构建，新增设置行自动可搜；非英文界面额外按英文再构建一遍，
//! 给每个条目附上英文标题作为别名，并用英文标题生成跨语言稳定的 id（使用记录不随语言切换丢失）。

use fluxdown_ui_i18n::Translator;
use gpui::{App, Entity, SharedString};

use crate::sections::SectionContext;
use crate::store::SettingsStore;
use crate::ui::SettingsPage;
use crate::view::{ActivityBarToggle, ReferralPage, build_pages};

/// 面包屑分隔符。
const CRUMB_SEPARATOR: &str = " › ";

/// 设置窗口内的定位目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsTarget {
    /// 分类 key。
    pub page: &'static str,
    /// 子 Tab id；空串表示分类默认 Tab。
    pub tab: &'static str,
    /// 行标题：定位时填入设置窗口搜索框筛出该行；`None` 只切分类 / Tab。
    pub row: Option<SharedString>,
}

/// 一个可搜索的设置条目。
#[derive(Clone, Debug)]
pub struct SettingsSearchEntry {
    /// 跨语言稳定的使用记录键。
    pub id: SharedString,
    pub title: SharedString,
    /// 所在位置（分类 › Tab › 分组），不含「设置」前缀。
    pub breadcrumb: SharedString,
    /// 另一语言（英文）的标题；当前即英文界面时为空。
    pub aliases: Vec<SharedString>,
    /// 描述、帮助、关键词与分组名。
    pub keywords: Vec<SharedString>,
    pub target: SettingsTarget,
}

/// 构建全部设置条目：分类、多 Tab 分类的子 Tab、带标题的设置行。
pub fn search_index(
    translator: &Entity<Translator>,
    store: &Entity<SettingsStore>,
    activity_bar: &[ActivityBarToggle],
    referral_visible: bool,
    cx: &mut App,
) -> Vec<SettingsSearchEntry> {
    let current = translator.read(cx).clone();
    let mut entries = flatten(&pages_for(
        &current,
        store,
        activity_bar,
        referral_visible,
        cx,
    ));
    if current.locale() == "en" {
        return entries;
    }
    let mut english = current;
    english.set_locale("en");
    let english = flatten(&pages_for(
        &english,
        store,
        activity_bar,
        referral_visible,
        cx,
    ));
    // 两次构建读同一份设置状态，结构应逐项一致；不一致（构建期间状态变化）时放弃别名。
    let aligned = english.len() == entries.len()
        && english.iter().zip(&entries).all(|(en, local)| {
            en.target.page == local.target.page && en.target.tab == local.target.tab
        });
    if aligned {
        for (entry, en) in entries.iter_mut().zip(english) {
            entry.id = en.id;
            if en.title != entry.title {
                entry.aliases.push(en.title);
            }
        }
    }
    entries
}

fn pages_for(
    translator: &Translator,
    store: &Entity<SettingsStore>,
    activity_bar: &[ActivityBarToggle],
    referral_visible: bool,
    cx: &mut App,
) -> Vec<SettingsPage> {
    let ctx = SectionContext { store, translator };
    let referral = if referral_visible {
        ReferralPage::Shown(None)
    } else {
        ReferralPage::Hidden
    };
    build_pages(&ctx, activity_bar, None, None, referral, cx)
}

fn flatten(pages: &[SettingsPage]) -> Vec<SettingsSearchEntry> {
    let mut entries = Vec::new();
    for page in pages {
        entries.push(SettingsSearchEntry {
            id: SharedString::from(format!("setting:{}", page.key)),
            title: page.title.clone(),
            breadcrumb: SharedString::default(),
            aliases: Vec::new(),
            keywords: vec![page.description.clone()],
            target: SettingsTarget {
                page: page.key,
                tab: "",
                row: None,
            },
        });
        let tabbed = page.visible_tabs().len() > 1;
        for tab in page.tabs() {
            if tabbed {
                entries.push(SettingsSearchEntry {
                    id: SharedString::from(format!("setting:{}/{}", page.key, tab.id)),
                    title: tab.label.clone(),
                    breadcrumb: page.title.clone(),
                    aliases: Vec::new(),
                    keywords: Vec::new(),
                    target: SettingsTarget {
                        page: page.key,
                        tab: tab.id,
                        row: None,
                    },
                });
            }
            for section in tab.section_list() {
                let mut crumbs = vec![page.title.as_ref()];
                if tabbed {
                    crumbs.push(tab.label.as_ref());
                }
                if let Some(heading) = section.heading()
                    && (!tabbed || heading != &tab.label)
                {
                    crumbs.push(heading.as_ref());
                }
                let breadcrumb = SharedString::from(crumbs.join(CRUMB_SEPARATOR));
                for row in section.rows() {
                    if row.title.is_empty() {
                        continue;
                    }
                    let mut keywords: Vec<SharedString> = row.search_terms().cloned().collect();
                    keywords.extend(section.heading().cloned());
                    entries.push(SettingsSearchEntry {
                        id: SharedString::from(format!(
                            "setting:{}/{}/{}",
                            page.key, tab.id, row.title
                        )),
                        title: row.title.clone(),
                        breadcrumb: breadcrumb.clone(),
                        aliases: Vec::new(),
                        keywords,
                        target: SettingsTarget {
                            page: page.key,
                            tab: tab.id,
                            row: Some(row.title.clone()),
                        },
                    });
                }
            }
        }
    }
    entries
}
