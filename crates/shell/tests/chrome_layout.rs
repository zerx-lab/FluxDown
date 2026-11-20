//! 布局模型用例：与 Web `web/src/shell/chromeLayout.test.ts` 共用同一份 fixtures。

use fluxdown_ui_shell::{ChromeFamilies, ChromeFamily, ChromeItemSpec, ChromeLayout, ChromeZone};
use serde_json::Value;

const CASES: &str = include_str!("fixtures/chrome_layout_cases.json");

/// 用例目录（`catalog` 数组）；任一字段缺失或未知区域 / 承载条时为 `None`。
fn catalog(items: &Value) -> Option<Vec<ChromeItemSpec>> {
    items
        .as_array()?
        .iter()
        .map(|item| {
            let families = item["families"]
                .as_array()?
                .iter()
                .map(|family| family.as_str().and_then(ChromeFamily::from_key))
                .try_fold(ChromeFamilies::default(), |families, family| {
                    family.map(|family| families.with(family))
                })?;
            let spec = ChromeItemSpec::new(
                item["id"].as_str()?.to_owned(),
                ChromeZone::from_key(item["defaultZone"].as_str()?)?,
                families,
            );
            let spec = if item["fixed"].as_bool() == Some(true) {
                spec.fixed()
            } else {
                spec
            };
            Some(if item["defaultHidden"].as_bool() == Some(true) {
                spec.hidden_by_default()
            } else {
                spec
            })
        })
        .collect()
}

fn stored(case: &Value) -> Option<&Value> {
    Some(&case["stored"]).filter(|value| !value.is_null())
}

#[test]
fn normalize_cases() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture json");
    let catalog = catalog(&cases["catalog"]).expect("fixture catalog");
    for case in cases["normalize"].as_array().expect("normalize cases") {
        let layout = ChromeLayout::from_value(stored(case), &catalog);
        assert_eq!(layout.to_value(), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn default_hidden_cases() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture json");
    let section = &cases["defaultHidden"];
    let catalog = catalog(&section["catalog"]).expect("fixture catalog");
    for case in section["cases"].as_array().expect("default hidden cases") {
        let layout = ChromeLayout::from_value(stored(case), &catalog);
        assert_eq!(layout.to_value(), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn move_cases() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture json");
    let catalog = catalog(&cases["catalog"]).expect("fixture catalog");
    for case in cases["moves"].as_array().expect("move cases") {
        let mut layout = ChromeLayout::from_value(stored(case), &catalog);
        let movement = &case["move"];
        let zone = movement["zone"].as_str().and_then(ChromeZone::from_key);
        let changed = layout.move_item(
            &catalog,
            movement["id"].as_str().expect("move id"),
            zone.expect("move zone"),
            movement["target"].as_str(),
        );
        assert_eq!(Value::Bool(changed), case["changed"], "{}", case["name"]);
        assert_eq!(layout.to_value(), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn visibility_cases() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture json");
    let catalog = catalog(&cases["catalog"]).expect("fixture catalog");
    for case in cases["visibility"].as_array().expect("visibility cases") {
        let mut layout = ChromeLayout::from_value(stored(case), &catalog);
        let set = &case["set"];
        let changed = layout.set_hidden(
            &catalog,
            set["id"].as_str().expect("set id"),
            set["hidden"].as_bool().expect("set hidden"),
        );
        assert_eq!(Value::Bool(changed), case["changed"], "{}", case["name"]);
        assert_eq!(layout.to_value(), case["expected"], "{}", case["name"]);
    }
}

#[test]
fn serialized_layout_round_trips() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture json");
    let catalog = catalog(&cases["catalog"]).expect("fixture catalog");
    let mut layout = ChromeLayout::default_for(&catalog);
    assert!(layout.move_item(&catalog, "limit", ChromeZone::TitleLeading, None));
    assert!(layout.set_hidden(&catalog, "limit", true));
    let restored = ChromeLayout::from_value(Some(&layout.to_value()), &catalog);
    assert_eq!(restored, layout);
}
