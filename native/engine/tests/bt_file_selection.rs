//! Regression test for zerx-lab/FluxDown#90: a known BT file selection must be
//! baked into `AddTorrentOptions.only_files` at add time. librqbit rejects
//! `update_only_files` while `Initializing`, so a post-add update silently
//! dropped the subset and downloaded every file.

use fluxdown_engine::bt_downloader::{
    BtSelectionStrategy, build_add_torrent_options, decide_bt_selection_strategy,
};

// Path A: partial subset known before add → must be applied at add time.
#[test]
fn partial_pre_selection_is_applied_at_add_time() {
    let selected: Vec<i32> = vec![0, 1, 2, 3, 4, 5, 6, 7, 8];

    let strategy = decide_bt_selection_strategy(false, &selected);

    assert_eq!(
        strategy,
        BtSelectionStrategy::AtAdd(vec![0, 1, 2, 3, 4, 5, 6, 7, 8])
    );
    assert_eq!(
        strategy.only_files_for_add(),
        Some(vec![0, 1, 2, 3, 4, 5, 6, 7, 8])
    );
}

// Path S: user confirmed "all files" → no restriction.
#[test]
fn all_files_confirmed_needs_no_restriction() {
    let strategy = decide_bt_selection_strategy(true, &[]);

    assert_eq!(strategy, BtSelectionStrategy::All);
    assert_eq!(strategy.only_files_for_add(), None);
}

// Path B: first-time magnet, dialog pending → selection applied post-add.
#[test]
fn unknown_selection_is_applied_post_add() {
    let strategy = decide_bt_selection_strategy(false, &[]);

    assert_eq!(strategy, BtSelectionStrategy::PostAdd);
    assert_eq!(strategy.only_files_for_add(), None);
}

// The -1 cancel sentinel and corrupt negatives are never valid file ids.
#[test]
fn cancel_sentinel_and_negatives_are_never_baked_as_files() {
    let strategy = decide_bt_selection_strategy(false, &[-1]);

    assert_eq!(strategy, BtSelectionStrategy::PostAdd);
    assert_eq!(strategy.only_files_for_add(), None);
}

// Corrupt persisted selection like [-2] (non-empty, all-negative, NOT the -1
// cancel sentinel): the negative filter empties it, so it must fall back to a
// safe strategy — never AtAdd(empty), which would silently download nothing (#90).
#[test]
fn corrupt_negative_preselection_never_downloads_none() {
    let strategy = decide_bt_selection_strategy(false, &[-2]);

    assert_ne!(strategy, BtSelectionStrategy::AtAdd(vec![]));
    assert_eq!(strategy, BtSelectionStrategy::PostAdd);
    assert_eq!(strategy.only_files_for_add(), None);
}

// Guards the real add-site wiring: production builds its add options via
// build_add_torrent_options, so reverting only_files there turns this red.
#[test]
fn build_add_options_bakes_known_subset_into_only_files() {
    let strategy = decide_bt_selection_strategy(false, &[0, 1, 2, 3, 4, 5, 6, 7, 8]);

    let opts = build_add_torrent_options(&strategy, "stage".to_string());

    assert_eq!(opts.only_files, Some(vec![0, 1, 2, 3, 4, 5, 6, 7, 8]));
    assert_eq!(opts.output_folder.as_deref(), Some("stage"));
}

// All / PostAdd must leave only_files unset so librqbit fetches every file.
#[test]
fn build_add_options_leaves_only_files_unset_when_not_preselected() {
    let all = build_add_torrent_options(&BtSelectionStrategy::All, "stage".to_string());
    assert_eq!(all.only_files, None);

    let post = build_add_torrent_options(&BtSelectionStrategy::PostAdd, "stage".to_string());
    assert_eq!(post.only_files, None);
}
