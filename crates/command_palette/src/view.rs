//! 面板视图：gpui-component 可搜索 `List` 放进无标题对话框；↑↓ 选择、Enter / 点击执行、
//! Esc / 点遮罩关闭。

use std::{collections::HashSet, rc::Rc};

use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Global, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, SharedString, Styled as _, Task, Window, WindowId, div, px,
};
use gpui_base::actions::{SelectDown, SelectUp};
use gpui_component::{
    Icon, IndexPath, Sizable as _, WindowExt as _, h_flex,
    list::{List, ListDelegate, ListItem, ListState},
};

/// 面板根元素的键位上下文。
const KEY_CONTEXT: &str = "CommandPalette";

/// 注册面板内键位：Ctrl+N / Ctrl+P（Vim / Emacs 习惯）上下移动选中项，与 ↑↓ 等价。
/// 只在面板上下文生效，不影响其他列表与输入框。
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-n", SelectDown, context),
        KeyBinding::new("ctrl-p", SelectUp, context),
    ]);
}

use crate::{
    matcher::{Query, Scorer, SearchText, Searchable},
    strings::PaletteStrings,
    usage::UsageStats,
};

const PALETTE_WIDTH: f32 = 640.;
const PALETTE_LIST_HEIGHT: f32 = 420.;
/// 距窗口顶部的偏移：落在标题栏下方、视线上方。
const PALETTE_MARGIN_TOP: f32 = 72.;
/// 有查询时最多展示的结果数。
const MAX_RESULTS: usize = 60;

/// 条目的执行体。
type RunItem = Rc<dyn Fn(&mut Window, &mut App)>;
/// 使用记录回传：宿主据此持久化。
pub type UsageSink = Rc<dyn Fn(&UsageStats, &mut App)>;

/// 条目类别：命令在空查询时直接列出；设置项只在被搜索或曾经使用过时出现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteItemKind {
    Command,
    Setting,
}

/// 面板中的一个可执行条目。
pub struct PaletteItem {
    id: SharedString,
    kind: PaletteItemKind,
    title: SharedString,
    detail: Option<SharedString>,
    icon: Option<Icon>,
    shortcut: Option<SharedString>,
    aliases: Vec<SharedString>,
    keywords: Vec<SharedString>,
    run: RunItem,
}

impl PaletteItem {
    /// `id` 是跨会话稳定的使用记录键；`run` 在面板关闭后于面板所在窗口执行。
    pub fn new(
        kind: PaletteItemKind,
        id: impl Into<SharedString>,
        title: impl Into<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            detail: None,
            icon: None,
            shortcut: None,
            aliases: Vec::new(),
            keywords: Vec::new(),
            run: Rc::new(run),
        }
    }

    /// 右侧的位置说明（如设置项的「设置 › 下载 › 连接」）。
    #[must_use]
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// 右侧的快捷键提示。
    #[must_use]
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// 与标题同权的别名（其他语言的标题、同义词），允许模糊命中。
    #[must_use]
    pub fn aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        self.aliases.extend(aliases.into_iter().map(Into::into));
        self
    }

    /// 补充文本（说明、分组名），只接受连续命中且排序靠后。
    #[must_use]
    pub fn keywords<I, S>(mut self, keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        self.keywords.extend(keywords.into_iter().map(Into::into));
        self
    }
}

/// 兜底条目的执行回调：参数为去掉首尾空白的查询词。
type QueryRunner = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// 以查询词本身为参数的兜底条目（如「问 AI 助手：…」）：查询非空时排在全部结果之后，
/// 没有任何命中时它就是唯一一行、默认选中，回车即执行。不计入使用记录。
pub struct PaletteQueryAction {
    icon: Icon,
    title: Rc<dyn Fn(&str) -> SharedString>,
    run: QueryRunner,
}

impl PaletteQueryAction {
    /// `title` 由去掉首尾空白的查询词生成行标题；`run` 在面板关闭后于面板所在窗口执行。
    pub fn new(
        icon: impl Into<Icon>,
        title: impl Fn(&str) -> SharedString + 'static,
        run: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            icon: icon.into(),
            title: Rc::new(title),
            run: Rc::new(run),
        }
    }
}

/// 打开面板所需的全部输入。
pub struct PaletteConfig {
    pub items: Vec<PaletteItem>,
    pub usage: UsageStats,
    /// 执行条目后回传最新使用记录，由宿主持久化。
    pub on_usage: UsageSink,
    /// 查询词兜底条目；`None` = 不提供。
    pub query_action: Option<PaletteQueryAction>,
}

/// 已打开面板的窗口（快捷键再按一次即关闭；同一窗口不叠开）。
#[derive(Default)]
struct OpenPalettes(HashSet<WindowId>);

impl Global for OpenPalettes {}

fn window_id(window: &Window) -> WindowId {
    window.window_handle().window_id()
}

fn mark_closed(id: WindowId, cx: &mut App) {
    if let Some(open) = cx.try_global::<OpenPalettes>()
        && open.0.contains(&id)
    {
        cx.global_mut::<OpenPalettes>().0.remove(&id);
    }
}

/// 该窗口当前是否显示着面板。对话框被外部关掉（没走面板自己的关闭回调）时顺带清掉残留标记。
#[must_use]
pub fn is_open(window: &mut Window, cx: &mut App) -> bool {
    let id = window_id(window);
    let tracked = cx
        .try_global::<OpenPalettes>()
        .is_some_and(|open| open.0.contains(&id));
    if tracked && !window.has_active_dialog(cx) {
        mark_closed(id, cx);
        return false;
    }
    tracked
}

/// 关闭该窗口的面板；未打开时不动其他对话框。
pub fn close(window: &mut Window, cx: &mut App) {
    if is_open(window, cx) {
        mark_closed(window_id(window), cx);
        window.close_dialog(cx);
    }
}

/// 在窗口内打开面板并聚焦搜索框。
pub fn open(window: &mut Window, cx: &mut App, translator: &Translator, config: PaletteConfig) {
    let id = window_id(window);
    cx.default_global::<OpenPalettes>().0.insert(id);
    let strings = PaletteStrings::new(translator);
    let placeholder = strings.placeholder.clone();
    let delegate = PaletteDelegate::new(config, strings);
    let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
    list.update(cx, |list, cx| {
        let first = (list.delegate().row_count() > 0).then(IndexPath::default);
        list.set_selected_index(first, window, cx);
    });

    let dialog_list = list.clone();
    let width = active_theme(cx).text_extent(PALETTE_WIDTH);
    window.open_dialog(cx, move |dialog, _, _| {
        let list = dialog_list.clone();
        let placeholder = placeholder.clone();
        dialog
            .w(width)
            .margin_top(px(PALETTE_MARGIN_TOP))
            .p_0()
            .close_button(false)
            .on_close(move |_, _, cx| mark_closed(id, cx))
            .content(move |content, _, _| {
                // 设计高度固定；窗口矮于面板时随对话框收缩，列表自带滚动。
                content.min_h_0().child(
                    div()
                        .key_context(KEY_CONTEXT)
                        .w_full()
                        .h(px(PALETTE_LIST_HEIGHT))
                        .min_h_0()
                        .child(
                            List::new(&list)
                                .search_placeholder(placeholder.clone())
                                // 搜索框用大尺寸：默认中号在面板里过矮，与命令面板的主输入地位不符。
                                .large()
                                .size_full(),
                        ),
                )
            })
    });
    list.update(cx, |list, cx| list.focus(window, cx));
}

struct Entry {
    item: PaletteItem,
    search: Searchable,
    title_len: usize,
}

struct PaletteDelegate {
    entries: Vec<Entry>,
    /// 当前展示顺序（`entries` 下标）。
    matches: Vec<usize>,
    selected: Option<usize>,
    usage: UsageStats,
    on_usage: UsageSink,
    query_action: Option<PaletteQueryAction>,
    /// 当前查询词（已去首尾空白），供查询词兜底条目使用。
    query: String,
    scorer: Scorer,
    strings: PaletteStrings,
}

impl PaletteDelegate {
    fn new(config: PaletteConfig, strings: PaletteStrings) -> Self {
        let entries = config
            .items
            .into_iter()
            .map(|item| Entry {
                search: Searchable {
                    title: SearchText::new(&item.title),
                    aliases: item
                        .aliases
                        .iter()
                        .map(|text| SearchText::new(text))
                        .collect(),
                    keywords: item
                        .keywords
                        .iter()
                        .map(|text| SearchText::new(text))
                        .collect(),
                },
                title_len: item.title.chars().count(),
                item,
            })
            .collect();
        let mut delegate = Self {
            entries,
            matches: Vec::new(),
            selected: None,
            usage: config.usage,
            on_usage: config.on_usage,
            query_action: config.query_action,
            query: String::new(),
            scorer: Scorer::default(),
            strings,
        };
        delegate.update_matches("");
        delegate
    }

    fn update_matches(&mut self, raw: &str) {
        let now = UsageStats::now();
        raw.trim().clone_into(&mut self.query);
        let query = Query::new(raw);
        let mut ranked: Vec<(usize, i32)> = if query.is_empty() {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    let boost = self.usage.boost(&entry.item.id, now);
                    (boost > 0 || entry.item.kind == PaletteItemKind::Command)
                        .then_some((index, boost))
                })
                .collect()
        } else {
            let scorer = &mut self.scorer;
            let usage = &self.usage;
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    scorer
                        .score(&query, &entry.search)
                        .map(|score| (index, score + usage.boost(&entry.item.id, now)))
                })
                .collect()
        };
        let entries = &self.entries;
        ranked.sort_by(|(a, a_score), (b, b_score)| {
            b_score
                .cmp(a_score)
                .then_with(|| entries[*a].title_len.cmp(&entries[*b].title_len))
                .then_with(|| a.cmp(b))
        });
        if !query.is_empty() {
            ranked.truncate(MAX_RESULTS);
        }
        self.matches = ranked.into_iter().map(|(index, _)| index).collect();
        self.selected = (self.row_count() > 0).then_some(0);
    }

    fn entry_at(&self, row: usize) -> Option<&Entry> {
        self.matches
            .get(row)
            .and_then(|index| self.entries.get(*index))
    }

    /// 查询词兜底条目所在行（排在全部命中之后）；查询为空或未提供时没有这一行。
    fn query_row(&self) -> Option<usize> {
        (self.query_action.is_some() && !self.query.is_empty()).then_some(self.matches.len())
    }

    fn row_count(&self) -> usize {
        self.matches.len() + usize::from(self.query_row().is_some())
    }
}

impl ListDelegate for PaletteDelegate {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.update_matches(query);
        cx.notify();
        // List 只在上一帧已有行时才选中首行；结果从空变为非空时这里补选，保证 Enter 可用。
        cx.spawn_in(window, async move |list, cx| {
            let Ok(()) = list.update_in(cx, |list, window, cx| {
                let first = (list.delegate().row_count() > 0).then(IndexPath::default);
                list.set_selected_index(first, window, cx);
            }) else {
                // 调色板或窗口已释放，结束这次异步选择更新。
                return;
            };
        })
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.row_count()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let (icon, title, detail, shortcut) = if self.query_row() == Some(ix.row) {
            let action = self.query_action.as_ref()?;
            (
                Some(action.icon.clone()),
                (action.title)(&self.query),
                None,
                None,
            )
        } else {
            let item = &self.entry_at(ix.row)?.item;
            (
                item.icon.clone(),
                item.title.clone(),
                item.detail.clone(),
                item.shortcut.clone(),
            )
        };
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let icon_size = extended.icon.md;

        let icon = match icon {
            Some(icon) => icon
                .size(icon_size)
                .text_color(tokens.colors.muted_foreground)
                .into_any_element(),
            None => div().size(icon_size).into_any_element(),
        };
        let row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(div().flex_none().child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .text_color(tokens.colors.foreground)
                    .child(title),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_none()
                    .max_w(theme.text_extent(PALETTE_WIDTH * 0.45))
                    .truncate()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(extended.colors.text_tertiary)
                    .child(detail)
            }))
            .children(shortcut.map(|shortcut| {
                div()
                    .flex_none()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(shortcut)
            }));

        Some(
            ListItem::new(("command-palette-item", ix.row))
                .h(theme.density().nav_row)
                .px(tokens.spacing.md)
                .child(row),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(tokens.typography.sm.size)
            .text_color(tokens.colors.muted_foreground)
            .child(self.strings.no_results.clone())
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.map(|ix| ix.row);
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if let Some(row) = self.selected
            && self.query_row() == Some(row)
            && let Some(action) = &self.query_action
        {
            let run = Rc::clone(&action.run);
            let query = self.query.clone();
            close(window, cx);
            window.defer(cx, move |window, cx| run(query, window, cx));
            return;
        }
        let Some((id, run)) = self
            .selected
            .and_then(|row| self.entry_at(row))
            .map(|entry| (entry.item.id.clone(), entry.item.run.clone()))
        else {
            return;
        };
        self.usage.record(&id, UsageStats::now());
        (self.on_usage)(&self.usage, cx);
        close(window, cx);
        window.defer(cx, move |window, cx| run(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::SharedString;

    use super::{PaletteConfig, PaletteDelegate, PaletteItem, PaletteItemKind, PaletteQueryAction};
    use crate::{strings::PaletteStrings, usage::UsageStats};

    fn delegate(items: Vec<PaletteItem>, usage: UsageStats) -> PaletteDelegate {
        PaletteDelegate::new(
            PaletteConfig {
                items,
                usage,
                on_usage: Rc::new(|_, _| {}),
                query_action: None,
            },
            PaletteStrings {
                placeholder: SharedString::default(),
                no_results: SharedString::default(),
            },
        )
    }

    fn item(kind: PaletteItemKind, id: &'static str, title: &'static str) -> PaletteItem {
        PaletteItem::new(kind, id, title, |_, _| {})
    }

    fn shown(delegate: &PaletteDelegate) -> Vec<&str> {
        delegate
            .matches
            .iter()
            .map(|index| delegate.entries[*index].item.id.as_ref())
            .collect()
    }

    #[test]
    fn query_action_follows_matches_and_is_selected_when_nothing_matches() {
        let mut delegate = PaletteDelegate::new(
            PaletteConfig {
                items: vec![item(PaletteItemKind::Command, "cmd.pause_all", "全部暂停")],
                usage: UsageStats::default(),
                on_usage: Rc::new(|_, _| {}),
                query_action: Some(PaletteQueryAction::new(
                    gpui_component::IconName::Search,
                    |query| SharedString::from(format!("ask {query}")),
                    |_, _, _| {},
                )),
            },
            PaletteStrings {
                placeholder: SharedString::default(),
                no_results: SharedString::default(),
            },
        );
        assert_eq!(delegate.query_row(), None);

        delegate.update_matches("qb");
        assert_eq!(shown(&delegate), ["cmd.pause_all"]);
        assert_eq!(delegate.query_row(), Some(1));
        assert_eq!(delegate.selected, Some(0));

        delegate.update_matches("  why is BT slow  ");
        assert!(shown(&delegate).is_empty());
        assert_eq!(delegate.query, "why is BT slow");
        assert_eq!(delegate.query_row(), Some(0));
        assert_eq!(delegate.selected, Some(0));

        delegate.update_matches("   ");
        assert_eq!(delegate.row_count(), 1);
        assert_eq!(delegate.query_row(), None);
    }

    #[test]
    fn empty_query_lists_commands_with_frequent_items_first() {
        let mut usage = UsageStats::default();
        let now = UsageStats::now();
        usage.record("setting.proxy", now);
        for _ in 0..3 {
            usage.record("cmd.resume_all", now);
        }
        let delegate = delegate(
            vec![
                item(PaletteItemKind::Command, "cmd.pause_all", "全部暂停"),
                item(PaletteItemKind::Command, "cmd.resume_all", "全部恢复"),
                item(PaletteItemKind::Setting, "setting.proxy", "代理模式"),
                item(PaletteItemKind::Setting, "setting.ua", "User-Agent"),
            ],
            usage,
        );
        assert_eq!(
            shown(&delegate),
            ["cmd.resume_all", "setting.proxy", "cmd.pause_all"]
        );
        assert_eq!(delegate.selected, Some(0));
    }

    #[test]
    fn usage_breaks_ties_between_equally_matching_items() {
        let items = || {
            vec![
                item(PaletteItemKind::Command, "cmd.pause_all", "全部暂停"),
                item(PaletteItemKind::Command, "cmd.resume_all", "全部恢复"),
            ]
        };
        let mut fresh = delegate(items(), UsageStats::default());
        fresh.update_matches("qb");
        assert_eq!(shown(&fresh), ["cmd.pause_all", "cmd.resume_all"]);

        let mut usage = UsageStats::default();
        usage.record("cmd.resume_all", UsageStats::now());
        let mut used = delegate(items(), usage);
        used.update_matches("qb");
        assert_eq!(shown(&used), ["cmd.resume_all", "cmd.pause_all"]);

        used.update_matches("zzzz");
        assert!(shown(&used).is_empty());
        assert_eq!(used.selected, None);
    }
}
