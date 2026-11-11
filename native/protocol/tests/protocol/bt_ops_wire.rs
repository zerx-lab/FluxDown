//! BT 任务操作（文件优先级 / 重命名 / 重新校验 / 移动 / 限速 / 超级做种 / 端口映射状态）的 wire 契约：
//! 方法名、慢方法归属、DTO 的 camelCase 形状与旧 daemon 缺省字段的兼容。

use fluxdown_protocol::method::{
    ALL_METHODS, DAEMON_BT_PORT_STATUS, DAEMON_TASK_BT_FILES, DAEMON_TASK_MOVE_STORAGE,
    DAEMON_TASK_REANNOUNCE, DAEMON_TASK_RECHECK, DAEMON_TASK_RENAME_BT_FILE,
    DAEMON_TASK_SET_BT_LIMITS, DAEMON_TASK_SET_FILE_PRIORITIES, DAEMON_TASK_SET_SUPER_SEEDING,
    is_slow_daemon_method,
};
use fluxdown_protocol::{
    BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL,
    BT_FILE_PRIORITY_SKIP, BtFilePriorityDto, BtFilesDto, BtPortMappingDto, BtPortStatusDto,
    BtStatusDto, BtTaskFileDto, BtTrackerDto, BtWebSeedDto, DaemonSeedLocalParams,
    MoveTaskStorageParams, RenameBtFileParams, SeedLocalResultDto, SetTaskBtLimitsParams,
    SetTaskFilePrioritiesParams, SetTaskSuperSeedingParams, TaskDto, TorrentShareDto,
};
use serde_json::json;

#[test]
fn bt_task_methods_are_canonical_and_only_move_is_slow() {
    let expected = [
        (DAEMON_TASK_BT_FILES, "daemon.task.btFiles"),
        (
            DAEMON_TASK_SET_FILE_PRIORITIES,
            "daemon.task.setFilePriorities",
        ),
        (DAEMON_TASK_RENAME_BT_FILE, "daemon.task.renameBtFile"),
        (DAEMON_TASK_RECHECK, "daemon.task.recheck"),
        (DAEMON_TASK_MOVE_STORAGE, "daemon.task.moveStorage"),
        (DAEMON_TASK_SET_BT_LIMITS, "daemon.task.setBtLimits"),
        (DAEMON_TASK_SET_SUPER_SEEDING, "daemon.task.setSuperSeeding"),
        (DAEMON_TASK_REANNOUNCE, "daemon.task.reannounce"),
        (DAEMON_BT_PORT_STATUS, "daemon.bt.portStatus"),
    ];
    for (constant, literal) in expected {
        assert_eq!(constant, literal);
        assert_eq!(
            ALL_METHODS.iter().filter(|name| **name == literal).count(),
            1,
            "{literal} must appear exactly once in ALL_METHODS"
        );
        // 只有跨卷移动数据会长时间占用；其余都是改内存 / 落一行库。
        assert_eq!(
            is_slow_daemon_method(constant),
            constant == DAEMON_TASK_MOVE_STORAGE,
            "{literal}"
        );
    }
}

#[test]
fn file_priority_levels_match_qbittorrent() {
    assert_eq!(
        [
            BT_FILE_PRIORITY_SKIP,
            BT_FILE_PRIORITY_NORMAL,
            BT_FILE_PRIORITY_HIGH,
            BT_FILE_PRIORITY_MAXIMUM
        ],
        [0, 1, 6, 7]
    );
}

#[test]
fn params_use_camel_case_and_omit_unset_fields() {
    let priorities = SetTaskFilePrioritiesParams {
        task_id: "t1".to_owned(),
        priorities: vec![BtFilePriorityDto {
            index: 2,
            priority: BT_FILE_PRIORITY_HIGH,
        }],
    };
    assert_eq!(
        serde_json::to_value(&priorities).unwrap(),
        json!({"taskId": "t1", "priorities": [{"index": 2, "priority": 6}]})
    );

    let rename = RenameBtFileParams {
        task_id: "t1".to_owned(),
        index: 0,
        path: "字幕/第 1 集.srt".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&rename).unwrap(),
        json!({"taskId": "t1", "index": 0, "path": "字幕/第 1 集.srt"})
    );

    let moved = MoveTaskStorageParams {
        task_id: "t1".to_owned(),
        save_dir: r"D:\下载 2024\  深层".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&moved).unwrap(),
        json!({"taskId": "t1", "saveDir": r"D:\下载 2024\  深层"})
    );

    // 只改下载限速：上传限速字段不上线，daemon 侧保持原值。
    let limits: SetTaskBtLimitsParams =
        serde_json::from_value(json!({"taskId": "t1", "downloadLimitBps": 0})).unwrap();
    assert_eq!(limits.download_limit_bps, Some(0));
    assert_eq!(limits.upload_limit_bps, None);
    assert_eq!(
        serde_json::to_value(&limits).unwrap(),
        json!({"taskId": "t1", "downloadLimitBps": 0})
    );

    assert_eq!(
        serde_json::to_value(SetTaskSuperSeedingParams {
            task_id: "t1".to_owned(),
            enabled: true,
        })
        .unwrap(),
        json!({"taskId": "t1", "enabled": true})
    );
}

#[test]
fn files_and_port_status_round_trip() {
    let files = BtFilesDto {
        editable: true,
        files: vec![BtTaskFileDto {
            index: 3,
            path: "Season 1/E01.mkv".to_owned(),
            size: 5_000_000_000,
            downloaded: 4_294_967_296,
            priority: BT_FILE_PRIORITY_MAXIMUM,
        }],
    };
    let value = serde_json::to_value(&files).unwrap();
    assert_eq!(
        value,
        json!({"editable": true, "files": [{
            "index": 3, "path": "Season 1/E01.mkv", "size": 5_000_000_000u64,
            "downloaded": 4_294_967_296u64, "priority": 7
        }]})
    );
    assert_eq!(serde_json::from_value::<BtFilesDto>(value).unwrap(), files);

    let idle: BtPortStatusDto = serde_json::from_value(json!({"running": false})).unwrap();
    assert_eq!(idle, BtPortStatusDto::default());
    let status = BtPortStatusDto {
        running: true,
        listen_port: 6881,
        mappings: vec![
            BtPortMappingDto {
                protocol: "tcp".to_owned(),
                internal_port: 6881,
                external_port: 50123,
                method: "natPmp".to_owned(),
                external_ip: "203.0.113.7".to_owned(),
                state: "mapped".to_owned(),
                message: String::new(),
            },
            BtPortMappingDto {
                protocol: "udp".to_owned(),
                internal_port: 6881,
                state: "failed".to_owned(),
                message: "no gateway answered".to_owned(),
                ..Default::default()
            },
        ],
    };
    let value = serde_json::to_value(&status).unwrap();
    assert_eq!(value["listenPort"], 6881);
    assert_eq!(value["mappings"][0]["externalPort"], 50123);
    assert_eq!(value["mappings"][0]["method"], "natPmp");
    assert!(value["mappings"][1].get("externalPort").is_none());
    assert_eq!(
        serde_json::from_value::<BtPortStatusDto>(value).unwrap(),
        status
    );
}

/// 旧 daemon 不下发新字段：客户端反序列化必须落到「未设置」而不是失败。
#[test]
fn replies_from_an_older_daemon_still_parse() {
    let tracker: BtTrackerDto = serde_json::from_value(json!({
        "url": "udp://tracker.example:6969/announce",
        "source": "torrent",
        "status": "working",
    }))
    .unwrap();
    assert_eq!(tracker.tier, 0);
    assert_eq!(
        (tracker.seeders, tracker.leechers, tracker.downloaded),
        (None, None, None)
    );

    let status: BtStatusDto = serde_json::from_value(json!({
        "dhtEnabled": true,
        "peerCounts": {"live": 0, "connecting": 0, "queued": 0, "seen": 0, "dead": 0},
        "trackers": [],
        "peers": [],
    }))
    .unwrap();
    assert!(status.info_hash.is_empty() && status.info_hash_v2.is_empty());
    assert!(status.web_seeds.is_empty());

    let share: TorrentShareDto =
        serde_json::from_value(json!({"infoHash": "aa", "name": "n", "magnet": "magnet:?"}))
            .unwrap();
    assert!(share.info_hash_v2.is_empty());

    let seeded: SeedLocalResultDto = serde_json::from_value(json!({"taskId": "t1"})).unwrap();
    assert!(seeded.torrent_path.is_empty());
}

#[test]
fn tracker_scrape_numbers_and_web_seeds_are_carried() {
    let tracker = BtTrackerDto {
        url: "https://tracker.example/announce".to_owned(),
        tier: 2,
        seeders: Some(0),
        leechers: Some(41),
        downloaded: Some(1_000_000),
        ..Default::default()
    };
    let value = serde_json::to_value(&tracker).unwrap();
    // 「0 个做种」是事实，必须上线；与「未知」（缺省）区分。
    assert_eq!(value["seeders"], 0);
    assert_eq!(value["tier"], 2);
    assert_eq!(
        serde_json::from_value::<BtTrackerDto>(value).unwrap(),
        tracker
    );

    let seed = BtWebSeedDto {
        url: "https://mirror.example/pub/".to_owned(),
        state: "backingOff".to_owned(),
        downloaded: 0,
        message: "HTTP 503".to_owned(),
    };
    let value = serde_json::to_value(&seed).unwrap();
    assert_eq!(value["state"], "backingOff");
    assert!(value.get("downloaded").is_none());
    assert_eq!(serde_json::from_value::<BtWebSeedDto>(value).unwrap(), seed);
}

#[test]
fn seed_local_torrent_options_default_to_todays_behaviour() {
    let legacy: DaemonSeedLocalParams =
        serde_json::from_value(json!({"path": "/data/movie.mkv"})).unwrap();
    assert!(legacy.version.is_empty() && !legacy.private && legacy.piece_length == 0);
    assert!(!legacy.create_only && !legacy.skip_check && !legacy.super_seeding);
    assert!(legacy.torrent_output.is_empty() && legacy.web_seeds.is_empty());
    // 旧形状原样往返：新字段都不上线。
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        json!({"path": "/data/movie.mkv", "unlimited": false})
    );

    let full = DaemonSeedLocalParams {
        path: "/data/show".to_owned(),
        version: "hybrid".to_owned(),
        private: true,
        piece_length: 4 * 1024 * 1024,
        web_seeds: vec!["https://mirror.example/show/".to_owned()],
        comment: "c".to_owned(),
        source: "SITE".to_owned(),
        super_seeding: true,
        torrent_output: "/data/show.torrent".to_owned(),
        create_only: true,
        ..Default::default()
    };
    let value = serde_json::to_value(&full).unwrap();
    assert_eq!(value["pieceLength"], 4_194_304);
    assert_eq!(value["webSeeds"][0], "https://mirror.example/show/");
    assert_eq!(value["torrentOutput"], "/data/show.torrent");
    assert_eq!(value["createOnly"], true);
    assert_eq!(
        serde_json::from_value::<DaemonSeedLocalParams>(value).unwrap(),
        full
    );
}

#[test]
fn task_row_keeps_bt_limits_off_the_wire_when_unset() {
    let mut task: TaskDto = serde_json::from_value(json!({
        "taskId": "t1", "url": "magnet:?xt=urn:btih:aa", "fileName": "f", "saveDir": "/d",
        "status": 1, "downloadedBytes": 0, "totalBytes": 0, "errorMessage": "",
        "createdAt": "0",
    }))
    .unwrap();
    assert_eq!(task.bt_download_limit_bps, 0);
    assert!(!task.bt_super_seeding);
    let value = serde_json::to_value(&task).unwrap();
    assert!(value.get("btDownloadLimitBps").is_none());
    assert!(value.get("btSuperSeeding").is_none());

    task.bt_download_limit_bps = 512_000;
    task.bt_super_seeding = true;
    let value = serde_json::to_value(&task).unwrap();
    assert_eq!(value["btDownloadLimitBps"], 512_000);
    assert_eq!(value["btSuperSeeding"], true);
}
