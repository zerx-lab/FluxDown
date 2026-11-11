//! Pure directory grouping model shared with Web. Paths are normalized lexically only; no filesystem access.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use super::view_prefs::natural_cmp;

#[cfg_attr(test, derive(serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirectoryItem {
    pub path: String,
    pub root: String,
    pub scope: String,
    pub scope_label: String,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryKind {
    Default,
    External,
    Unknown,
    Scope,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryRow {
    Header {
        key: String,
        label: String,
        path: String,
        depth: usize,
        count: usize,
        kind: DirectoryKind,
    },
    Item {
        index: usize,
        depth: usize,
    },
}

#[derive(Default)]
struct Node {
    name: String,
    name_fold: String,
    path: String,
    parent: Option<usize>,
    direct: Vec<(usize, usize)>,
    children: Vec<usize>,
    child_by_name: HashMap<String, usize>,
    count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Default,
    External,
    Unknown,
}

struct Tree {
    scope: String,
    scope_label: String,
    category: Category,
    root_path: String,
    root_fold: String,
    nodes: Vec<Node>,
}

fn is_windows(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        || path.starts_with("\\\\")
        || path.starts_with("//")
}

fn normalize(path: &str, windows: bool) -> String {
    if path.is_empty() {
        return String::new();
    }
    let normalized_slashes = if windows {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let unc = windows && normalized_slashes.starts_with("//");
    let drive = windows && normalized_slashes.as_bytes().get(1) == Some(&b':');
    let absolute = normalized_slashes.starts_with('/') || drive;
    let prefix = if unc {
        "//".to_owned()
    } else if drive {
        normalized_slashes.get(..2).unwrap_or_default().to_owned()
    } else if absolute {
        "/".to_owned()
    } else {
        String::new()
    };
    let rest = if unc {
        normalized_slashes.trim_start_matches('/')
    } else if drive {
        normalized_slashes
            .get(2..)
            .unwrap_or_default()
            .trim_start_matches('/')
    } else {
        normalized_slashes.trim_start_matches('/')
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if prefix == "//" {
        format!("//{joined}")
    } else if drive {
        if joined.is_empty() {
            format!("{prefix}/")
        } else {
            format!("{prefix}/{joined}")
        }
    } else if absolute {
        if joined.is_empty() {
            "/".to_owned()
        } else {
            format!("/{joined}")
        }
    } else {
        joined
    }
}

fn components(path: &str, windows: bool) -> Vec<String> {
    if path.is_empty() {
        return Vec::new();
    }
    if windows && path.starts_with("//") {
        let mut parts = path.trim_start_matches('/').split('/');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        let mut result = vec![format!("//{server}/{share}")];
        result.extend(parts.filter(|part| !part.is_empty()).map(str::to_owned));
        return result;
    }
    let rest = if windows && path.as_bytes().get(1) == Some(&b':') {
        path.get(3..).unwrap_or_default()
    } else {
        path.trim_start_matches('/')
    };
    let mut result = Vec::new();
    if windows && path.as_bytes().get(1) == Some(&b':') {
        result.push(path.get(..2).unwrap_or_default().to_owned());
    }
    result.extend(
        rest.split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_owned),
    );
    result
}

fn is_descendant(path: &str, root: &str, windows: bool) -> bool {
    if path == root {
        return true;
    }
    let path_is_absolute =
        path.starts_with('/') || (windows && path.as_bytes().get(1) == Some(&b':'));
    if !path_is_absolute {
        return false;
    }
    let path_components = components(path, windows);
    let root_components = components(root, windows);
    path_components.len() > root_components.len()
        && path_components.get(..root_components.len()) == Some(root_components.as_slice())
}

fn compare_names(left_fold: &str, left: &str, right_fold: &str, right: &str) -> Ordering {
    natural_cmp(left_fold, right_fold).then_with(|| left.cmp(right))
}

fn tree_for(
    trees: &mut Vec<Tree>,
    scope: &str,
    scope_label: &str,
    category: Category,
    root: &str,
) -> usize {
    if let Some(index) = trees
        .iter()
        .position(|tree| tree.scope == scope && tree.category == category && tree.root_path == root)
    {
        return index;
    }
    let root_node = Node {
        path: root.to_owned(),
        ..Node::default()
    };
    trees.push(Tree {
        scope: scope.to_owned(),
        scope_label: scope_label.to_owned(),
        category,
        root_path: root.to_owned(),
        root_fold: root.to_lowercase(),
        nodes: vec![root_node],
    });
    trees.len() - 1
}

fn add_path(tree: &mut Tree, path: &str, root: &str, index: usize, count: usize, windows: bool) {
    let path_parts = components(path, windows);
    let root_parts = components(root, windows);
    let relative = match tree.category {
        Category::External => path_parts.as_slice(),
        Category::Default => path_parts.get(root_parts.len()..).unwrap_or_default(),
        Category::Unknown => &[],
    };
    let mut node_index = 0;
    for name in relative {
        let parent = node_index;
        node_index = if let Some(child) = tree.nodes[parent].child_by_name.get(name).copied() {
            child
        } else {
            let parent_path = tree.nodes[parent].path.clone();
            let child_path = if parent_path.is_empty() {
                if name.starts_with("//") || (windows && name.as_bytes().get(1) == Some(&b':')) {
                    name.clone()
                } else if path.starts_with('/') {
                    format!("/{name}")
                } else {
                    name.clone()
                }
            } else if parent_path.ends_with('/') {
                format!("{parent_path}{name}")
            } else {
                format!("{parent_path}/{name}")
            };
            let child = tree.nodes.len();
            tree.nodes.push(Node {
                name: name.clone(),
                name_fold: name.to_lowercase(),
                path: child_path,
                parent: Some(parent),
                ..Node::default()
            });
            tree.nodes[parent].children.push(child);
            tree.nodes[parent].child_by_name.insert(name.clone(), child);
            child
        };
    }
    tree.nodes[node_index].direct.push((index, count));
    let mut current = Some(node_index);
    while let Some(node) = current {
        tree.nodes[node].count += count;
        current = tree.nodes[node].parent;
    }
}

fn emit_tree(
    tree: &Tree,
    collapsed: &impl Fn(&str) -> bool,
    depth_offset: usize,
    rows: &mut Vec<DirectoryRow>,
) {
    let (kind, key, label) = match tree.category {
        Category::Default => (
            DirectoryKind::Default,
            format!("directory:{}:{}", tree.scope, tree.root_path),
            String::new(),
        ),
        Category::External => (
            DirectoryKind::External,
            format!("category:{}:external", tree.scope),
            String::new(),
        ),
        Category::Unknown => (
            DirectoryKind::Unknown,
            format!("category:{}:unknown", tree.scope),
            String::new(),
        ),
    };
    let root = &tree.nodes[0];
    rows.push(DirectoryRow::Header {
        key: key.clone(),
        label,
        path: tree.root_path.clone(),
        depth: depth_offset,
        count: root.count,
        kind,
    });
    if collapsed(&key) {
        return;
    }
    enum Event {
        Visit {
            node: usize,
            depth: usize,
        },
        Folder {
            child: usize,
            end: usize,
            label: String,
            depth: usize,
        },
    }
    let mut stack = vec![Event::Visit {
        node: 0,
        depth: depth_offset + 1,
    }];
    while let Some(event) = stack.pop() {
        match event {
            Event::Folder {
                child,
                end,
                label,
                depth,
            } => {
                let path = tree.nodes[end].path.clone();
                let child_key = format!("directory:{}:{}", tree.scope, path);
                rows.push(DirectoryRow::Header {
                    key: child_key.clone(),
                    label,
                    path,
                    depth,
                    count: tree.nodes[child].count,
                    kind: DirectoryKind::Folder,
                });
                if !collapsed(&child_key) {
                    stack.push(Event::Visit {
                        node: end,
                        depth: depth + 1,
                    });
                }
            }
            Event::Visit {
                node: node_index,
                depth,
            } => {
                let node = &tree.nodes[node_index];
                for &(index, _) in &node.direct {
                    rows.push(DirectoryRow::Item { index, depth });
                }
                let mut children = node.children.clone();
                children.sort_by(|left, right| {
                    compare_names(
                        &tree.nodes[*left].name_fold,
                        &tree.nodes[*left].name,
                        &tree.nodes[*right].name_fold,
                        &tree.nodes[*right].name,
                    )
                });
                let mut folders = Vec::with_capacity(children.len());
                for child in children {
                    let mut end = child;
                    let mut parts = vec![tree.nodes[end].name.clone()];
                    loop {
                        let current_key =
                            format!("directory:{}:{}", tree.scope, tree.nodes[end].path);
                        if collapsed(&current_key)
                            || !tree.nodes[end].direct.is_empty()
                            || tree.nodes[end].children.len() != 1
                        {
                            break;
                        }
                        let next = tree.nodes[end].children[0];
                        let next_key =
                            format!("directory:{}:{}", tree.scope, tree.nodes[next].path);
                        if collapsed(&next_key) {
                            break;
                        }
                        end = next;
                        parts.push(tree.nodes[end].name.clone());
                    }
                    folders.push((child, end, parts.join(" / ")));
                }
                for (child, end, label) in folders.into_iter().rev() {
                    stack.push(Event::Folder {
                        child,
                        end,
                        label,
                        depth,
                    });
                }
            }
        }
    }
}

/// Flatten filtered task-directory items into stable, collapsible rows.
pub(crate) fn flatten(
    items: &[DirectoryItem],
    collapsed: impl Fn(&str) -> bool,
) -> Vec<DirectoryRow> {
    let mut trees = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let windows = is_windows(&item.path) || is_windows(&item.root);
        let path = normalize(&item.path, windows);
        let root = normalize(&item.root, windows);
        let category = if path.is_empty() {
            Category::Unknown
        } else if root.is_empty() || !is_descendant(&path, &root, windows) {
            Category::External
        } else {
            Category::Default
        };
        let root_path = if category == Category::Default {
            root.clone()
        } else {
            String::new()
        };
        let tree_index = tree_for(
            &mut trees,
            &item.scope,
            &item.scope_label,
            category,
            &root_path,
        );
        add_path(
            &mut trees[tree_index],
            &path,
            &root_path,
            index,
            item.count,
            windows,
        );
    }
    trees.sort_by(|left, right| {
        let category_order = |category| match category {
            Category::Default => 0,
            Category::External => 1,
            Category::Unknown => 2,
        };
        left.scope
            .cmp(&right.scope)
            .then_with(|| category_order(left.category).cmp(&category_order(right.category)))
            .then_with(|| {
                compare_names(
                    &left.root_fold,
                    &left.root_path,
                    &right.root_fold,
                    &right.root_path,
                )
            })
    });
    let mut scope_counts: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
    for tree in &trees {
        if !tree.scope.is_empty() {
            let entry = scope_counts
                .entry(&tree.scope)
                .or_insert((&tree.scope_label, 0));
            entry.1 += tree.nodes[0].count;
        }
    }
    let mut rows = Vec::new();
    let mut last_scope = String::new();
    for tree in &trees {
        let depth_offset = if tree.scope.is_empty() {
            0
        } else {
            if last_scope != tree.scope {
                if let Some((label, count)) = scope_counts.get(tree.scope.as_str()) {
                    let key = format!("scope:{}", tree.scope);
                    rows.push(DirectoryRow::Header {
                        key,
                        label: (*label).to_owned(),
                        path: String::new(),
                        depth: 0,
                        count: *count,
                        kind: DirectoryKind::Scope,
                    });
                }
                last_scope.clone_from(&tree.scope);
            }
            if collapsed(&format!("scope:{}", tree.scope)) {
                continue;
            }
            1
        };
        emit_tree(tree, &collapsed, depth_offset, &mut rows);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{DirectoryItem, DirectoryKind, DirectoryRow, flatten};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Case {
        name: String,
        items: Vec<DirectoryItem>,
        collapsed: Vec<String>,
        expect: Vec<ExpectedRow>,
    }
    #[derive(Deserialize)]
    struct ExpectedRow {
        kind: String,
        #[serde(rename = "directoryKind")]
        directory_kind: Option<String>,
        key: Option<String>,
        label: Option<String>,
        path: Option<String>,
        depth: usize,
        count: Option<usize>,
        index: Option<usize>,
    }

    fn kind_name(kind: DirectoryKind) -> &'static str {
        match kind {
            DirectoryKind::Default => "default",
            DirectoryKind::External => "external",
            DirectoryKind::Unknown => "unknown",
            DirectoryKind::Scope => "scope",
            DirectoryKind::Folder => "folder",
        }
    }

    #[test]
    fn shared_directory_fixtures() {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../tests/fixtures/directory_tree_cases.json"
        ))
        .expect("valid shared fixture");
        for case in fixture.cases {
            let rows = flatten(&case.items, |key| {
                case.collapsed.iter().any(|candidate| candidate == key)
            });
            assert_eq!(rows.len(), case.expect.len(), "{}", case.name);
            for (actual, expected) in rows.iter().zip(case.expect) {
                match actual {
                    DirectoryRow::Header {
                        key,
                        label,
                        path,
                        depth,
                        count,
                        kind,
                    } => {
                        assert_eq!(expected.kind, "header", "{}", case.name);
                        assert_eq!(
                            expected.directory_kind.as_deref(),
                            Some(kind_name(*kind)),
                            "{}",
                            case.name
                        );
                        assert_eq!(expected.key.as_deref(), Some(key.as_str()), "{}", case.name);
                        assert_eq!(
                            expected.label.as_deref(),
                            Some(label.as_str()),
                            "{}",
                            case.name
                        );
                        assert_eq!(
                            expected.path.as_deref(),
                            Some(path.as_str()),
                            "{}",
                            case.name
                        );
                        assert_eq!(expected.depth, *depth, "{}", case.name);
                        assert_eq!(expected.count, Some(*count), "{}", case.name);
                    }
                    DirectoryRow::Item { index, depth } => {
                        assert_eq!(expected.kind, "item", "{}", case.name);
                        assert_eq!(expected.index, Some(*index), "{}", case.name);
                        assert_eq!(expected.depth, *depth, "{}", case.name);
                    }
                }
            }
        }
    }
}
