use std::collections::HashSet;

use fluxdown_protocol::{CreateTaskRequest, PreviewItemDto, PreviewVariantDto};

use super::{ManifestRow, ManifestSelection, ManifestSort};

fn file(id: &str, name: &str, path: &str, size: i64) -> PreviewItemDto {
    PreviewItemDto {
        id: id.to_owned(),
        name: name.to_owned(),
        path: path.to_owned(),
        size,
        variants: Vec::new(),
    }
}
fn variant(id: &str, size: i64) -> PreviewVariantDto {
    PreviewVariantDto {
        id: id.to_owned(),
        label: id.to_owned(),
        size,
    }
}
fn ids(model: &ManifestSelection) -> HashSet<String> {
    model
        .group_items()
        .into_iter()
        .map(|item| item.resolver_item)
        .collect()
}

#[test]
fn default_group_name_percent_decodes_url_segment() {
    use super::default_group_name;
    assert_eq!(default_group_name("  Pack ", "https://a.example/x"), "Pack");
    assert_eq!(
        default_group_name("", "https://a.example/d/%E4%B8%AD%E6%96%87.m3u8?t=1"),
        "中文.m3u8"
    );
    assert_eq!(
        default_group_name("", "https://a.example/d/plain/"),
        "plain"
    );
    assert_eq!(default_group_name("", "https://a.example/%B6%D4"), "");
    assert_eq!(default_group_name("", "https://a.example"), "");
}

fn dir(model: &ManifestSelection, label: &str) -> usize {
    model
        .rows()
        .iter()
        .find_map(|row| match row {
            ManifestRow::Directory {
                node,
                label: actual,
                ..
            } if actual == label => Some(*node),
            _ => None,
        })
        .expect("the directory must be visible")
}

#[test]
fn all_files_default_selected_without_conflating_equal_names_or_variants() {
    let mut first = file("opaque-first", "same.mp4", "first", 120);
    first.variants = vec![variant("default-looking-label", 300)];
    let second = file("opaque-second", "same.mp4", "second", 240);
    let model = ManifestSelection::new(vec![first, second]);
    assert_eq!(
        ids(&model),
        HashSet::from(["opaque-first".to_owned(), "opaque-second".to_owned()])
    );
    let stat = model.selection_stat();
    assert_eq!((stat.count, stat.size, stat.unknown), (2, 360, 0));
    let items = model.group_items();
    assert_eq!(
        (
            items[0].file_name.as_str(),
            items[0].rel_path.as_str(),
            items[0].size
        ),
        ("same.mp4", "first", 120)
    );
    assert_eq!(
        (items[1].file_name.as_str(), items[1].rel_path.as_str()),
        ("same.mp4", "second")
    );
}

#[test]
fn global_filter_selection_replaces_scope_instead_of_retaining_hidden_files() {
    let mut model = ManifestSelection::new(vec![
        file("root-video", "root.MP4", "", 10),
        file("deep-video", "nested.mp4", "one/two", 20),
        file("document", "readme.txt", "one", 30),
    ]);
    let nested = dir(&model, "one");
    model.navigate(nested);
    model.toggle_extension("MP4");
    model.select_visible(false);
    assert_eq!(
        ids(&model),
        HashSet::from(["root-video".to_owned(), "deep-video".to_owned()])
    );
    // Current directory does not narrow the global toolbar's scope.
    assert_eq!(model.selection_stat().count, 2);
    model.toggle_file(0);
    model.select_visible(true);
    assert_eq!(ids(&model), HashSet::from(["root-video".to_owned()]));
    model.set_search("nested");
    model.select_visible(false);
    assert_eq!(ids(&model), HashSet::from(["deep-video".to_owned()]));
    model.clear();
    assert!(model.group_items().is_empty());
}

#[test]
fn search_crosses_levels_and_intersects_extension_filter() {
    let mut model = ManifestSelection::new(vec![
        file("root", "root.mp4", "", 2),
        file("deep", "movie.mp4", "Season/Finale", 3),
        file("notes", "movie.txt", "Season/Finale", 4),
    ]);
    model.set_search("  FINALE  ");
    assert_eq!(model.visible_count(), 2);
    assert!(model.rows().iter().all(|row| matches!(
        row,
        ManifestRow::File {
            show_path: true,
            ..
        }
    )));
    model.toggle_extension("MP4");
    assert_eq!(
        model.rows(),
        &[ManifestRow::File {
            index: 1,
            show_path: true
        }]
    );
    model.set_search("root");
    assert_eq!(
        model.rows(),
        &[ManifestRow::File {
            index: 0,
            show_path: true
        }]
    );
    // Merely filtering never changes the user's global selection.
    assert_eq!(model.selection_stat().count, 3);
}

#[test]
fn directory_toggle_uses_filtered_subtree_and_preserves_unrelated_selection() {
    let mut model = ManifestSelection::new(vec![
        file("a", "first.mp4", "set", 10),
        file("b", "second.mp4", "set/sub", 0),
        file("text", "notes.txt", "set", 5),
        file("outside", "other.mp4", "set-other", 7),
    ]);
    let set = dir(&model, "set");
    model.toggle_file(0);
    let stat = model
        .rows()
        .iter()
        .find_map(|row| match row {
            ManifestRow::Directory { node, stat, .. } if *node == set => Some(*stat),
            _ => None,
        })
        .expect("set row");
    assert_eq!((stat.count, stat.selected, stat.unknown), (3, 2, 1));
    // Partial -> checked, checked -> unchecked.
    model.toggle_directory(set);
    assert_eq!(model.selection_stat().count, 4);
    model.toggle_directory(set);
    assert_eq!(ids(&model), HashSet::from(["outside".to_owned()]));
    model.toggle_extension("MP4");
    model.toggle_directory(set);
    assert_eq!(
        ids(&model),
        HashSet::from(["a".to_owned(), "b".to_owned(), "outside".to_owned()])
    );
    assert!(!model.is_selected(2));
}

#[test]
fn drilldown_collapses_transition_directories_and_up_skips_them() {
    let mut model = ManifestSelection::new(vec![
        file("root", "root.txt", "", 1),
        file("deep", "deep.txt", "one/two/three", 2),
    ]);
    let deep = dir(&model, "one / two / three");
    model.navigate(deep);
    assert_eq!(
        model.rows(),
        &[ManifestRow::File {
            index: 1,
            show_path: false
        }]
    );
    let crumbs: Vec<_> = model
        .breadcrumbs()
        .iter()
        .map(|(_, label)| *label)
        .collect();
    assert_eq!(crumbs, ["", "one", "two", "three"]);
    model.up();
    assert_eq!(model.cwd(), 0);
    model.navigate(deep);
    model.toggle_extension("MP4");
    assert_eq!(model.cwd(), 0);
    assert!(model.rows().is_empty());
}

#[test]
fn explicit_variant_controls_token_size_and_unknown_statistics_only_for_its_item() {
    let mut item = file("opaque-episode", "original.bin", "folder", 50);
    item.variants = vec![variant("large", 150), variant("unknown", 0)];
    let mut model = ManifestSelection::new(vec![item, file("other", "other.bin", "", 20)]);
    assert!(model.set_variant(0, Some("large")));
    let items = model.group_items();
    assert_eq!(
        (items[0].resolver_item.as_str(), items[0].size),
        ("opaque-episode@large", 150)
    );
    assert_eq!(
        (items[0].file_name.as_str(), items[0].rel_path.as_str()),
        ("original.bin", "folder")
    );
    assert_eq!(
        (items[1].resolver_item.as_str(), items[1].size),
        ("other", 20)
    );
    assert_eq!(model.selection_stat().size, 170);
    assert!(!model.set_variant(0, Some("foreign-variant")));
    assert_eq!(model.variant_id(0), Some("large"));
    assert!(model.set_variant(0, Some("unknown")));
    let stat = model.selection_stat();
    assert_eq!((stat.size, stat.unknown), (20, 1));
    assert_eq!(model.group_items()[0].size, 0);
    model.toggle_file(0);
    assert_eq!(
        (model.selection_stat().count, model.selection_stat().unknown),
        (1, 0)
    );
    model.toggle_file(0);
    assert!(model.set_variant(0, None));
    assert_eq!(
        (
            model.group_items()[0].resolver_item.as_str(),
            model.group_items()[0].size
        ),
        ("opaque-episode", 50)
    );
}

#[test]
fn size_sort_uses_effective_variant_size_with_unknown_last_and_name_ties() {
    let mut item = file("variant", "b.bin", "", 3);
    item.variants.push(variant("large", 200));
    let mut model = ManifestSelection::new(vec![
        file("unknown", "a.bin", "", 0),
        item,
        file("known", "c.bin", "", 50),
    ]);
    model.toggle_sort();
    assert_eq!(model.sort(), ManifestSort::Size);
    assert_eq!(
        model.rows(),
        &[
            ManifestRow::File {
                index: 2,
                show_path: false
            },
            ManifestRow::File {
                index: 1,
                show_path: false
            },
            ManifestRow::File {
                index: 0,
                show_path: false
            },
        ]
    );
    assert!(model.set_variant(1, Some("large")));
    assert_eq!(
        model.rows()[0],
        ManifestRow::File {
            index: 1,
            show_path: false
        }
    );
}

#[test]
fn empty_selection_cannot_build_a_group_even_when_visible_files_exist() {
    let mut model = ManifestSelection::new(vec![file("file", "file.bin", "", 1)]);
    let base: CreateTaskRequest =
        serde_json::from_value(serde_json::json!({"url": "https://example.test/list"}))
            .expect("request fixture");
    model.clear();
    assert!(
        model
            .build_request(&base, "name", "", "", "", false)
            .is_none()
    );
    model.select_visible(false);
    assert!(
        model
            .build_request(&base, "name", "", "", "", false)
            .is_some()
    );
}
