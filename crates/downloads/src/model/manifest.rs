//! Plugin-neutral manifest navigation, selection and group-request projection.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fluxdown_protocol::{CreateGroupRequest, CreateTaskRequest, GroupItemRequest, PreviewItemDto};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ManifestStat {
    pub count: usize,
    pub selected: usize,
    pub size: u64,
    pub unknown: usize,
}

impl ManifestStat {
    fn add(&mut self, other: Self) {
        self.count += other.count;
        self.selected += other.selected;
        self.size = self.size.saturating_add(other.size);
        self.unknown += other.unknown;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ManifestSort {
    #[default]
    Name,
    Size,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManifestRow {
    Directory {
        node: usize,
        label: String,
        stat: ManifestStat,
    },
    File {
        index: usize,
        show_path: bool,
    },
}

#[derive(Debug, Default)]
struct Directory {
    parent: usize,
    name: String,
    path: String,
    children: BTreeMap<String, usize>,
    files: Vec<usize>,
}

struct FileSearch {
    extension: String,
    text: String,
}

/// The tree is built once; rows and aggregates are refreshed only on user changes.
/// Selection keys and variant tokens always come from plugin IDs, never names.
pub(crate) struct ManifestSelection {
    items: Vec<PreviewItemDto>,
    sizes: Vec<i64>,
    directories: Vec<Directory>,
    file_search: Vec<FileSearch>,
    visible: Vec<bool>,
    stats: Vec<ManifestStat>,
    total: ManifestStat,
    selection: ManifestStat,
    rows: Vec<ManifestRow>,
    row_files: Vec<usize>,
    selected: HashSet<String>,
    variants: HashMap<String, String>,
    extensions: Vec<(String, usize)>,
    extension_filter: BTreeSet<String>,
    search: String,
    cwd: usize,
    sort: ManifestSort,
}

impl ManifestSelection {
    pub(crate) fn new(items: Vec<PreviewItemDto>) -> Self {
        let mut directories = vec![Directory::default()];
        let mut file_search = Vec::with_capacity(items.len());
        let mut counts = BTreeMap::<String, usize>::new();
        for (index, item) in items.iter().enumerate() {
            let mut node = 0;
            for segment in item.path.split('/').filter(|segment| !segment.is_empty()) {
                node = if let Some(child) = directories[node].children.get(segment) {
                    *child
                } else {
                    let child = directories.len();
                    let path = if node == 0 {
                        segment.to_owned()
                    } else {
                        format!("{}/{segment}", directories[node].path)
                    };
                    directories.push(Directory {
                        parent: node,
                        name: segment.to_owned(),
                        path,
                        ..Directory::default()
                    });
                    directories[node].children.insert(segment.to_owned(), child);
                    child
                };
            }
            directories[node].files.push(index);
            let extension = extension_label(&item.name);
            *counts.entry(extension.clone()).or_default() += 1;
            file_search.push(FileSearch {
                extension,
                text: format!("{}/{}", item.path, item.name).to_lowercase(),
            });
        }
        let mut extensions: Vec<_> = counts.into_iter().collect();
        extensions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        extensions.truncate(7);
        let selected = items.iter().map(|item| item.id.clone()).collect();
        let mut this = Self {
            visible: vec![true; items.len()],
            sizes: items.iter().map(|item| item.size).collect(),
            stats: vec![ManifestStat::default(); directories.len()],
            total: ManifestStat::default(),
            selection: ManifestStat::default(),
            items,
            directories,
            file_search,
            rows: Vec::new(),
            row_files: Vec::new(),
            selected,
            variants: HashMap::new(),
            extensions,
            extension_filter: BTreeSet::new(),
            search: String::new(),
            cwd: 0,
            sort: ManifestSort::Name,
        };
        this.refresh();
        this
    }

    pub(crate) fn items(&self) -> &[PreviewItemDto] {
        &self.items
    }
    pub(crate) fn rows(&self) -> &[ManifestRow] {
        &self.rows
    }
    pub(crate) fn extensions(&self) -> &[(String, usize)] {
        &self.extensions
    }
    pub(crate) fn extension_active(&self, extension: &str) -> bool {
        self.extension_filter.contains(extension)
    }
    pub(crate) fn sort(&self) -> ManifestSort {
        self.sort
    }
    pub(crate) fn searching(&self) -> bool {
        !self.search.is_empty()
    }
    pub(crate) fn cwd(&self) -> usize {
        self.cwd
    }
    pub(crate) fn is_selected(&self, index: usize) -> bool {
        self.selected.contains(&self.items[index].id)
    }
    pub(crate) fn variant_id(&self, index: usize) -> Option<&str> {
        self.variants.get(&self.items[index].id).map(String::as_str)
    }
    pub(crate) fn size(&self, index: usize) -> i64 {
        self.sizes[index]
    }

    pub(crate) fn set_search(&mut self, search: &str) {
        self.search = search.trim().to_lowercase();
        self.refilter();
    }
    pub(crate) fn toggle_extension(&mut self, extension: &str) {
        if !self.extension_filter.remove(extension) {
            self.extension_filter.insert(extension.to_owned());
        }
        self.refilter();
    }
    pub(crate) fn toggle_sort(&mut self) {
        self.sort = match self.sort {
            ManifestSort::Name => ManifestSort::Size,
            ManifestSort::Size => ManifestSort::Name,
        };
        self.refresh_rows();
    }
    pub(crate) fn toggle_file(&mut self, index: usize) {
        let id = &self.items[index].id;
        if !self.selected.remove(id) {
            self.selected.insert(id.clone());
        }
        self.refresh();
    }
    pub(crate) fn set_variant(&mut self, index: usize, variant: Option<&str>) -> bool {
        let Some(item) = self.items.get(index) else {
            return false;
        };
        if let Some(variant) = variant {
            let Some(choice) = item
                .variants
                .iter()
                .find(|candidate| candidate.id == variant)
            else {
                return false;
            };
            self.sizes[index] = choice.size;
            self.variants.insert(item.id.clone(), variant.to_owned());
        } else {
            self.variants.remove(&item.id);
            self.sizes[index] = item.size;
        }
        self.refresh();
        true
    }

    /// Global toolbar operations replace selection with their filtered scope,
    /// including files below the current directory, matching the Web picker.
    pub(crate) fn select_visible(&mut self, invert: bool) {
        self.selected = self
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| {
                self.visible[*index] && (!invert || !self.selected.contains(&item.id))
            })
            .map(|(_, item)| item.id.clone())
            .collect();
        self.refresh();
    }
    pub(crate) fn clear(&mut self) {
        self.selected.clear();
        self.refresh();
    }
    pub(crate) fn toggle_directory(&mut self, node: usize) {
        let Some(stat) = self.stats.get(node) else {
            return;
        };
        let select = stat.selected != stat.count;
        select_subtree(
            &self.directories,
            &self.items,
            &self.visible,
            &mut self.selected,
            node,
            select,
        );
        self.refresh();
    }
    pub(crate) fn navigate(&mut self, node: usize) {
        self.cwd = if self.stats.get(node).is_some_and(|stat| stat.count > 0) {
            node
        } else {
            0
        };
        self.refresh_rows();
    }
    pub(crate) fn up(&mut self) {
        let mut node = self.directories[self.cwd].parent;
        while node != 0 && self.transition(node) {
            node = self.directories[node].parent;
        }
        self.navigate(node);
    }
    pub(crate) fn breadcrumbs(&self) -> Vec<(usize, &str)> {
        let mut result = Vec::new();
        let mut node = self.cwd;
        while node != 0 {
            result.push((node, self.directories[node].name.as_str()));
            node = self.directories[node].parent;
        }
        result.push((0, ""));
        result.reverse();
        result
    }
    pub(crate) fn visible_count(&self) -> usize {
        self.stats[0].count
    }
    pub(crate) fn total_stat(&self) -> ManifestStat {
        self.total
    }
    pub(crate) fn selection_stat(&self) -> ManifestStat {
        self.selection
    }
    fn file_stat(&self, index: usize) -> ManifestStat {
        let size = self.size(index);
        ManifestStat {
            count: 1,
            selected: usize::from(self.is_selected(index)),
            size: size.max(0) as u64,
            unknown: usize::from(size <= 0),
        }
    }
    fn refilter(&mut self) {
        for (visible, file) in self.visible.iter_mut().zip(&self.file_search) {
            *visible = (self.extension_filter.is_empty()
                || self.extension_filter.contains(&file.extension))
                && (self.search.is_empty() || file.text.contains(&self.search));
        }
        self.refresh();
    }
    fn refresh(&mut self) {
        // Parents are inserted before children; reverse order aggregates bottom-up.
        self.stats.fill(ManifestStat::default());
        self.total = ManifestStat::default();
        self.selection = ManifestStat::default();
        for node in (0..self.directories.len()).rev() {
            for &index in &self.directories[node].files {
                let file = self.file_stat(index);
                self.total.add(file);
                if file.selected > 0 {
                    self.selection.add(file);
                }
                if self.visible[index] {
                    self.stats[node].add(file);
                }
            }
            if node != 0 {
                let child = self.stats[node];
                self.stats[self.directories[node].parent].add(child);
            }
        }
        if !self.searching() && self.stats[self.cwd].count == 0 {
            self.cwd = 0;
        }
        self.refresh_rows();
    }
    fn transition(&self, node: usize) -> bool {
        !self.directories[node]
            .files
            .iter()
            .any(|index| self.visible[*index])
            && self.directories[node]
                .children
                .values()
                .filter(|child| self.stats[**child].count > 0)
                .count()
                == 1
    }
    fn refresh_rows(&mut self) {
        self.rows.clear();
        self.row_files.clear();
        if self.searching() {
            self.row_files
                .extend((0..self.items.len()).filter(|index| self.visible[*index]));
        } else {
            for &child in self.directories[self.cwd].children.values() {
                if self.stats[child].count == 0 {
                    continue;
                }
                let mut node = child;
                let mut label = self.directories[node].name.clone();
                while self.transition(node) {
                    let Some(next) = self.directories[node]
                        .children
                        .values()
                        .find(|child| self.stats[**child].count > 0)
                        .copied()
                    else {
                        break;
                    };
                    node = next;
                    label.push_str(" / ");
                    label.push_str(&self.directories[node].name);
                }
                self.rows.push(ManifestRow::Directory {
                    node,
                    label,
                    stat: self.stats[node],
                });
            }
            self.row_files.extend(
                self.directories[self.cwd]
                    .files
                    .iter()
                    .copied()
                    .filter(|index| self.visible[*index]),
            );
        }
        let items = &self.items;
        let sizes = &self.sizes;
        self.row_files.sort_by(|a, b| {
            let name = || {
                items[*a]
                    .name
                    .cmp(&items[*b].name)
                    .then_with(|| items[*a].id.cmp(&items[*b].id))
            };
            match self.sort {
                ManifestSort::Name => name(),
                ManifestSort::Size => sizes[*b].max(0).cmp(&sizes[*a].max(0)).then_with(name),
            }
        });
        let show_path = self.searching();
        self.rows
            .extend(self.row_files.iter().map(|index| ManifestRow::File {
                index: *index,
                show_path,
            }));
    }

    pub(crate) fn group_items(&self) -> Vec<GroupItemRequest> {
        self.items
            .iter()
            .enumerate()
            .filter(|(index, _)| self.is_selected(*index))
            .map(|(index, item)| GroupItemRequest {
                resolver_item: self.variant_id(index).map_or_else(
                    || item.id.clone(),
                    |variant| format!("{}@{variant}", item.id),
                ),
                file_name: item.name.clone(),
                rel_path: item.path.clone(),
                size: self.size(index),
            })
            .collect()
    }
    /// `resolver_plugin_id` is the preview's `pluginId`: the engine binds every member to
    /// exactly that plugin instead of re-routing, so members cannot drift to another one.
    pub(crate) fn build_request(
        &self,
        base: &CreateTaskRequest,
        group_name: &str,
        save_dir: &str,
        queue_id: &str,
        resolver_plugin_id: &str,
        start_paused: bool,
    ) -> Option<CreateGroupRequest> {
        let items = self.group_items();
        if items.is_empty() {
            return None;
        }
        Some(CreateGroupRequest {
            source_url: base.url.clone(),
            group_name: group_name.trim().to_owned(),
            save_dir: save_dir.trim().to_owned(),
            queue_id: queue_id.to_owned(),
            segments: base.segments,
            cookies: base.cookies.clone(),
            referrer: base.referrer.clone(),
            user_agent: base.user_agent.clone(),
            proxy_url: base.proxy_url.clone(),
            extra_headers: base.headers.clone().unwrap_or_default(),
            ignore_tls_errors: base.ignore_tls_errors,
            start_paused,
            items,
            resolver_plugin_id: resolver_plugin_id.to_owned(),
            archive_metadata: None,
            category_route: None,
        })
    }
}

fn select_subtree(
    directories: &[Directory],
    items: &[PreviewItemDto],
    visible: &[bool],
    selected: &mut HashSet<String>,
    node: usize,
    select: bool,
) {
    for &index in &directories[node].files {
        if !visible[index] {
            continue;
        }
        let id = &items[index].id;
        if select {
            selected.insert(id.clone());
        } else {
            selected.remove(id);
        }
    }
    for &child in directories[node].children.values() {
        select_subtree(directories, items, visible, selected, child, select);
    }
}

/// 清单分组默认名：清单自带名；为空时取来源 URL 路径末段并百分号解码（与 Web
/// `manifestDefaultGroupName` 一致；解码失败 → 空串，交调用方套用占位符）。
pub(crate) fn default_group_name(manifest_name: &str, source_url: &str) -> String {
    if !manifest_name.trim().is_empty() {
        return manifest_name.trim().to_owned();
    }
    source_url
        .split(['?', '#'])
        .next()
        .and_then(|url| url.split_once("://"))
        .and_then(|(_, rest)| rest.split_once('/'))
        .and_then(|(_, path)| path.split('/').rfind(|part| !part.is_empty()))
        .and_then(super::new_download::percent_decode_utf8)
        .unwrap_or_default()
}

fn extension_label(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            extension.to_uppercase()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
