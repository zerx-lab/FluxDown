use super::{
    TorrentDraft, TorrentForm, TorrentInput, cleanup_result, custom_name_for, split_inputs,
};
use crate::controller::{DownloadsCommand, DownloadsResult};
use crate::model::devices::DispatchTarget;
use fluxdown_protocol::{ApplicationErrorCode, RpcErrorData, torrent_preview::TorrentPreviewDto};
use fluxdown_protocol::{BtFileDto, torrent_preview::MagnetPreviewTicket};
use std::collections::{HashSet, VecDeque};

fn form() -> TorrentForm {
    TorrentForm {
        pending: VecDeque::from([TorrentDraft {
            input: TorrentInput::Magnet("magnet:?xt=test".into()),
            options: Default::default(),
            external: false,
        }]),
        preview: Some(Box::new(TorrentPreviewDto {
            torrent_blob_id: "torrent:test".into(),
            name: "资料".into(),
            total_bytes: 8,
            files: vec![
                BtFileDto {
                    index: 0,
                    path: "a.txt".into(),
                    size: 4,
                },
                BtFileDto {
                    index: 2,
                    path: "目录/b.txt".into(),
                    size: 4,
                },
            ],
        })),
        ticket: None,
        selected: HashSet::from([0, 2]),
        filled_name: String::new(),
        restore_target: DispatchTarget::Local,
        restore_category_edited: false,
        close_when_idle: false,
        resume_submit: None,
        submitted_any: false,
        loading: false,
        submitting: false,
        error: None,
        expired: false,
    }
}

#[test]
fn torrent_form_requires_ready_preview_and_explicit_nonempty_selection() {
    let mut form = form();
    form.expired = true;
    assert!(
        form.confirmation("destination", "queue".into(), false, String::new())
            .is_none()
    );
    form.expired = false;
    form.loading = true;
    assert!(
        form.confirmation("destination", "queue".into(), false, String::new())
            .is_none()
    );
    form.loading = false;
    form.submitting = true;
    assert!(
        form.confirmation("destination", "queue".into(), false, String::new())
            .is_none()
    );
    form.submitting = false;
    assert!(
        form.confirmation("  ", "queue".into(), false, String::new())
            .is_none()
    );
    form.selected.clear();
    assert!(
        form.confirmation("destination", "queue".into(), false, String::new())
            .is_none()
    );
    form.selected.insert(2);
    let params = form
        .confirmation(" destination ", "chosen-queue".into(), true, String::new())
        .expect("explicit confirmation");
    assert_eq!(params.selected_file_indices, vec![2]); // Padding indices must not be renumbered.
    form.selected.extend([0, 2]);
    let both = form
        .confirmation("destination", String::new(), false, String::new())
        .expect("both files");
    assert_eq!(
        both.selected_file_indices,
        vec![0, 2],
        "indices are submitted in ascending order regardless of click order"
    );
    assert_eq!(params.save_dir, "destination");
    assert_eq!(params.queue_id, "chosen-queue");
    assert!(params.start_paused);
    form.preview = None;
    assert!(
        form.confirmation("destination", "queue".into(), false, String::new())
            .is_none()
    );
}

#[test]
fn magnets_can_skip_the_preview_with_the_draft_options_and_current_directory() {
    let mut form = form();
    form.preview = None;
    form.loading = true;
    if let Some(draft) = form.pending.front_mut() {
        draft.options.queue_id = "queue".into();
        draft.options.start_paused = true;
        draft.options.file_name = "draft-name".into();
    }
    // 解析中与解析失败后都能直接添加；目录取确认界面当前值。
    let request = form
        .direct_request(" D:\\下载 ", "  ")
        .expect("direct add while resolving");
    assert_eq!(request.url, "magnet:?xt=test");
    assert_eq!(request.save_dir, "D:\\下载");
    assert_eq!(request.queue_id, "queue");
    assert!(request.start_paused);
    assert_eq!(request.file_name, "draft-name");
    form.loading = false;
    let renamed = form
        .direct_request("D:\\下载", " 我的资料 ")
        .expect("direct add after failure");
    assert_eq!(renamed.file_name, "我的资料");
    // 落不了盘的名字不带过去，沿用草稿里的。
    let invalid = form
        .direct_request("D:\\下载", "a/b")
        .expect("invalid name ignored");
    assert_eq!(invalid.file_name, "draft-name");
    assert!(form.direct_request("  ", "").is_none(), "needs a directory");
    form.submitting = true;
    assert!(form.direct_request("D:\\下载", "").is_none());
    form.submitting = false;
    // 预览已经就绪：走确认，不再提供跳过。
    form.preview = self::form().preview;
    assert!(form.direct_request("D:\\下载", "").is_none());
    // 本机种子文件没有「先建任务再等元数据」的路径。
    form.preview = None;
    if let Some(draft) = form.pending.front_mut() {
        draft.input = TorrentInput::File("a.torrent".into());
    }
    assert!(!form.current_is_magnet());
    assert!(form.direct_request("D:\\下载", "").is_none());
}

#[test]
fn torrent_late_results_have_cleanup_for_both_metadata_stages() {
    let preview = form().preview.expect("preview");
    assert!(
        matches!(cleanup_result(&Ok(DownloadsResult::TorrentPreview(preview))),
        Some(DownloadsCommand::DiscardTorrentPreview { torrent_blob_id }) if torrent_blob_id == "torrent:test")
    );
    assert!(
        matches!(cleanup_result(&Ok(DownloadsResult::MagnetPreviewTicket(MagnetPreviewTicket { preview_id: "ticket".into() }))),
        Some(DownloadsCommand::CancelMagnetPreview { preview_id }) if preview_id == "ticket")
    );
    assert!(
        cleanup_result(&Err(RpcErrorData::new(
            ApplicationErrorCode::Timeout,
            false
        )))
        .is_none()
    );
}

#[test]
fn opened_inputs_keep_local_paths_and_separate_http_torrent_downloads() {
    let path = "C:\\下载 2026\\100%20  完整😀.ToRrEnT";
    let (inputs, urls) = split_inputs(
        vec![
            path.into(),
            "MAGNET:?xt=urn:btih:abc".into(),
            "https://example.com/a.torrent".into(),
            "https://example.com/a.zip".into(),
        ],
        true,
    );
    assert_eq!(inputs.len(), 2);
    assert!(matches!(&inputs[0], TorrentInput::File(file) if file == std::path::Path::new(path)));
    assert!(matches!(&inputs[1], TorrentInput::Magnet(url) if url == "MAGNET:?xt=urn:btih:abc"));
    assert_eq!(
        urls,
        ["https://example.com/a.torrent", "https://example.com/a.zip"]
    );
    // 引擎不认的 `magnet:` 写法不进预览，留在普通链接里按原路径处理。
    let (inputs, urls) = split_inputs(vec!["magnet:xt=urn:btih:abc".into()], true);
    assert!(inputs.is_empty());
    assert_eq!(urls, ["magnet:xt=urn:btih:abc"]);
    let (inputs, urls) = split_inputs(
        vec!["file:///tmp/%E8%B5%84%E6%96%99%20test.torrent".into()],
        true,
    );
    assert!(urls.is_empty());
    assert!(
        matches!(&inputs[0], TorrentInput::File(path) if path.to_string_lossy().contains("资料 test.torrent"))
    );
}

#[test]
fn torrent_rename_target_ignores_untouched_and_blank_input() {
    let preview = form().preview.expect("preview");
    // 没动输入框、只删了尾随空格、或整段清空，都按「不改名」提交。
    assert_eq!(custom_name_for(&preview, ""), "");
    assert_eq!(custom_name_for(&preview, "   "), "");
    assert_eq!(custom_name_for(&preview, "资料"), "");
    assert_eq!(custom_name_for(&preview, " 资料 "), "");
    // 真正改过才把去空格后的名字交给 daemon。
    assert_eq!(custom_name_for(&preview, " 新的名字 "), "新的名字");
    assert_eq!(custom_name_for(&preview, "第 01 集.mkv"), "第 01 集.mkv");
}

#[test]
fn torrent_confirmation_refuses_unportable_rename_targets() {
    let form = form();
    let confirm =
        |name: &str| form.confirmation("destination", "queue".into(), false, name.to_owned());
    assert!(confirm("改名后的名字").is_some());
    for invalid in [
        "a/b".to_owned(),
        "a\\b".to_owned(),
        "a:b".to_owned(),
        "名字.".to_owned(),
        "CON".to_owned(),
        "x".repeat(201),
    ] {
        assert!(
            confirm(&invalid).is_none(),
            "unportable name must not reach the daemon: {invalid:?}"
        );
    }
}

#[test]
fn only_real_renames_are_validated_so_unportable_preview_names_still_confirm() {
    // 种子作者起的名字可能带 `:` 或超长：没改动 ≠ 改名，不能因此禁用确认按钮。
    let preview = TorrentPreviewDto {
        torrent_blob_id: "torrent:test".into(),
        name: "Star Wars: Episode IV".into(),
        total_bytes: 8,
        files: Vec::new(),
    };
    assert_eq!(custom_name_for(&preview, "Star Wars: Episode IV"), "");
    assert_eq!(custom_name_for(&preview, "  Star Wars: Episode IV  "), "");
    assert_eq!(super::custom_name_issue_key(""), None);

    // 真的改过名才校验，且错误分类要能对上文案键。
    assert_eq!(super::custom_name_issue_key("改名后的名字"), None);
    assert_eq!(
        super::custom_name_issue_key("Star: Wars"),
        Some("torrentConfirmNameInvalid")
    );
    assert_eq!(
        super::custom_name_issue_key("名字."),
        Some("torrentConfirmNameInvalid")
    );
    assert_eq!(
        super::custom_name_issue_key("CON"),
        Some("torrentConfirmNameReserved")
    );
    assert_eq!(
        super::custom_name_issue_key(&"x".repeat(201)),
        Some("torrentConfirmNameTooLong")
    );
}

#[test]
fn torrent_names_reset_between_items_but_keep_edits_during_resolution() {
    let mut form = form();
    assert_eq!(form.begin_item(), "");
    assert_eq!(form.preview_name("", "first.mkv"), Some("first.mkv".into()));
    assert_eq!(form.preview_name("custom.mkv", "first.mkv"), None);
    form.pending.pop_front();
    form.pending.push_back(TorrentDraft {
        input: TorrentInput::File("second.torrent".into()),
        options: Default::default(),
        external: false,
    });
    let reset = form.begin_item();
    assert_eq!(reset, "");
    assert_eq!(
        form.preview_name(&reset, "second.mkv"),
        Some("second.mkv".into())
    );
    assert_eq!(
        form.preview_name("edited-while-loading.mkv", "second.mkv"),
        None
    );
    // 取消后重开同样从新一项的草稿开始，不沿用旧输入框里的内容。
    form.pending.front_mut().expect("item").options.file_name = "draft.mkv".into();
    assert_eq!(form.begin_item(), "draft.mkv");
}

#[test]
fn torrent_remote_magnets_remain_in_the_normal_dispatch_form() {
    let magnet = "magnet:?xt=urn:btih:abc";
    let (torrents, urls) = split_inputs(vec![magnet.into(), "https://e.test/a.zip".into()], false);
    assert!(torrents.is_empty());
    assert_eq!(urls, [magnet, "https://e.test/a.zip"]);
}

#[test]
fn torrent_confirmation_preserves_the_draft_and_only_overrides_confirmed_fields() {
    let mut form = form();
    let draft = &mut form.pending.front_mut().expect("item").options;
    draft.queue_id = "chosen-queue".into();
    draft.start_paused = true;
    draft.proxy_url = "socks5://localhost:1080".into();
    draft.user_agent = "custom-agent".into();
    draft.file_name = "renamed.mkv".into();
    draft.checksum = "sha256=abc".into();
    assert_eq!(form.begin_item(), "renamed.mkv");
    let params = form
        .confirmation(
            "/new-dir",
            form.confirmation_queue(true, None),
            true,
            "renamed.mkv".into(),
        )
        .expect("confirm");
    assert_eq!(params.queue_id, "chosen-queue");
    assert!(params.start_paused);
    assert_eq!(params.custom_name, "renamed.mkv");
    let options = params.options.expect("draft");
    assert_eq!(options.proxy_url, "socks5://localhost:1080");
    assert_eq!(options.user_agent, "custom-agent");
    assert_eq!(options.checksum, "sha256=abc");
}

#[test]
fn torrent_later_preserves_explicit_draft_queue_and_new_choices_override_it() {
    let mut form = form();
    form.pending.front_mut().expect("item").options.queue_id = "chosen".into();
    assert_eq!(form.confirmation_queue(false, None), "chosen");
    assert_eq!(
        form.confirmation_queue(true, None),
        fluxdown_protocol::LATER_QUEUE_ID
    );
    form.pending.front_mut().expect("item").options.start_paused = true;
    assert_eq!(form.confirmation_queue(true, None), "chosen");
    assert_eq!(
        form.confirmation_queue(true, Some("override".into())),
        "override"
    );
}
