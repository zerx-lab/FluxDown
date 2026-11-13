//! 交互选择的 wire 契约：BT 文件选择的保存目录字段（#994）与旧端兼容。

use fluxdown_protocol::{SelectionKind, SelectionOutcome};
use serde_json::json;

#[test]
fn bt_selection_save_dir_is_optional_both_ways() {
    // 旧 daemon 的请求没有 saveDir：照常解析，预填为空。
    let legacy: SelectionKind =
        serde_json::from_value(json!({ "type": "bt", "files": [] })).expect("legacy bt kind");
    assert!(matches!(legacy, SelectionKind::Bt { ref save_dir, .. } if save_dir.is_empty()));

    let kind = SelectionKind::Bt {
        files: Vec::new(),
        save_dir: "/data/bt".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&kind).expect("serialize kind"),
        json!({ "type": "bt", "files": [], "saveDir": "/data/bt" })
    );

    // 旧客户端的答复没有 saveDir = 沿用当前目录；不改目录时也不发字段。
    let legacy: SelectionOutcome =
        serde_json::from_value(json!({ "kind": "bt", "indices": [1] })).expect("legacy outcome");
    assert_eq!(
        legacy,
        SelectionOutcome::Bt {
            indices: vec![1],
            save_dir: None,
            custom_name: None,
        }
    );
    assert_eq!(
        serde_json::to_value(&legacy).expect("serialize outcome"),
        json!({ "kind": "bt", "indices": [1] })
    );
    let moved = SelectionOutcome::Bt {
        indices: vec![0, 2],
        save_dir: Some("D:\\Movies".to_owned()),
        custom_name: None,
    };
    assert_eq!(
        serde_json::to_value(&moved).expect("serialize moved"),
        json!({ "kind": "bt", "indices": [0, 2], "saveDir": "D:\\Movies" })
    );
    // 改名只在用户改过时发送。
    let renamed = SelectionOutcome::Bt {
        indices: vec![1],
        save_dir: None,
        custom_name: Some("新名字".to_owned()),
    };
    let wire = json!({ "kind": "bt", "indices": [1], "customName": "新名字" });
    assert_eq!(
        serde_json::to_value(&renamed).expect("serialize renamed"),
        wire
    );
    assert_eq!(
        serde_json::from_value::<SelectionOutcome>(wire).expect("renamed outcome"),
        renamed
    );
}
