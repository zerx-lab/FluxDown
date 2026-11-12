use std::collections::HashMap;

use gpui::{Pixels, SharedString, px};

use super::{DownloadTableDelegate, VisibleRow, groups::Entity, row_at};
use crate::model::{
    DownloadTaskView, TaskSource,
    directory_tree::{DirectoryItem, DirectoryKind, DirectoryRow, flatten},
};

impl DownloadTableDelegate {
    pub(crate) fn directory_root(&self) -> &str {
        &self.default_save_dir
    }

    pub(crate) fn set_directory_context(
        &mut self,
        root: String,
        device_roots: HashMap<String, String>,
    ) {
        if self.default_save_dir != root || self.device_roots != device_roots {
            self.default_save_dir = root;
            self.device_roots = device_roots;
            self.view_dirty = true;
        }
    }

    fn directory_item(&self, task: &DownloadTaskView) -> DirectoryItem {
        if task.source == TaskSource::Local {
            DirectoryItem {
                path: task.save_dir.clone(),
                root: self.default_save_dir.clone(),
                scope: String::new(),
                scope_label: String::new(),
                count: 1,
            }
        } else {
            let scope = if task.to_device.is_empty() {
                "remote".to_owned()
            } else {
                task.to_device.clone()
            };
            DirectoryItem {
                path: task.save_dir.clone(),
                root: self.device_roots.get(&scope).cloned().unwrap_or_default(),
                scope_label: self.device_names.get(&scope).cloned().unwrap_or_else(|| {
                    if task.to_device.is_empty() {
                        self.strings.remote_tasks.to_string()
                    } else {
                        scope.clone()
                    }
                }),
                scope,
                count: 1,
            }
        }
    }

    pub(super) fn push_directories(
        &mut self,
        visible: &mut Vec<VisibleRow>,
        entities: &[Entity],
        local: &[DownloadTaskView],
        remote: &[DownloadTaskView],
    ) {
        let items: Vec<DirectoryItem> = entities
            .iter()
            .map(|entity| match *entity {
                Entity::Task(id) => row_at(local, remote, id)
                    .map(|task| self.directory_item(task))
                    .unwrap_or_default(),
                Entity::Group(ix) => {
                    self.group_rows
                        .get(ix)
                        .map_or_else(DirectoryItem::default, |group| DirectoryItem {
                            path: self
                                .groups
                                .get(&group.id)
                                .map(|meta| meta.save_dir.clone())
                                .unwrap_or_default(),
                            root: self.default_save_dir.clone(),
                            scope: String::new(),
                            scope_label: String::new(),
                            count: group.members.len(),
                        })
                }
            })
            .collect();
        for row in flatten(&items, |key| self.prefs.is_group_collapsed(key)) {
            match row {
                DirectoryRow::Header {
                    key,
                    label,
                    path,
                    depth,
                    count,
                    kind,
                } => {
                    let label = match kind {
                        DirectoryKind::Default => self.strings.directory_default.clone(),
                        DirectoryKind::External => self.strings.directory_external.clone(),
                        DirectoryKind::Unknown => self.strings.directory_unknown.clone(),
                        DirectoryKind::Scope | DirectoryKind::Folder => SharedString::from(label),
                    };
                    let collapsed = self.prefs.is_group_collapsed(&key);
                    if !path.is_empty() {
                        self.directory_paths.insert(visible.len(), path);
                    }
                    visible.push(VisibleRow::GroupHeader {
                        key,
                        label,
                        count,
                        collapsed,
                    });
                    self.directory_depths.push(depth);
                }
                DirectoryRow::Item { index, .. } => {
                    if let Some(&entity) = entities.get(index) {
                        self.push_entity(visible, entity);
                        self.directory_depths.resize(visible.len(), 0);
                    }
                }
            }
        }
    }

    pub(super) fn directory_indent(&self, row_ix: usize) -> Pixels {
        px(
            self.directory_depths.get(row_ix).copied().unwrap_or(0) as f32
                * super::groups::TREE_INDENT
                * self.text_scale.get(),
        )
    }
}
