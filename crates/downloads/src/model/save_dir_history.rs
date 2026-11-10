//! 新建下载「保存目录」历史：最近使用的本机目录，最新在前、去重、封顶；另按分类记住用户
//! 手动选过的目录（同分类再次新建时下拉优先列出）。
//!
//! 偏好值为字符串数组 / 「分类 id → 字符串数组」对象（设备本地，不进云同步）；规则与 Web
//! `web/src/lib/saveDirHistory.ts` 逐条对齐，两端共用
//! `crates/downloads/tests/fixtures/save_dir_history_cases.json`。

use std::collections::BTreeMap;

use serde_json::Value;

/// 按分类 id 记住的目录（每个列表规则同全局历史）。
pub(crate) type CategorySaveDirHistory = BTreeMap<String, Vec<String>>;

/// 最多保留的历史条数。
pub(crate) const SAVE_DIR_HISTORY_LIMIT: usize = 10;

/// 解析偏好值：非数组为空；逐项 trim，丢弃空串 / 非字符串，保留首次出现，封顶。
pub(crate) fn read_save_dir_history(value: Option<&Value>) -> Vec<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut history: Vec<String> = Vec::with_capacity(items.len().min(SAVE_DIR_HISTORY_LIMIT));
    for dir in items.iter().filter_map(Value::as_str).map(str::trim) {
        if history.len() == SAVE_DIR_HISTORY_LIMIT {
            break;
        }
        if !dir.is_empty() && !history.iter().any(|known| known == dir) {
            history.push(dir.to_owned());
        }
    }
    history
}

/// 记录一次使用：目录移到最前（已存在则前移），超出上限丢弃最旧的。
/// 空目录或已在最前时返回 `None`（无需写偏好）。
pub(crate) fn record_save_dir(history: &[String], dir: &str) -> Option<Vec<String>> {
    let dir = dir.trim();
    if dir.is_empty() || history.first().is_some_and(|first| first == dir) {
        return None;
    }
    let mut next = Vec::with_capacity(SAVE_DIR_HISTORY_LIMIT);
    next.push(dir.to_owned());
    next.extend(
        history
            .iter()
            .filter(|known| *known != dir)
            .take(SAVE_DIR_HISTORY_LIMIT - 1)
            .cloned(),
    );
    Some(next)
}

/// 下拉候选：先列当前分类记住的目录，再列全局历史，去重（保留首次）并排除当前输入。
pub(crate) fn save_dir_choices<'a>(
    category: &'a [String],
    history: &'a [String],
    current: &str,
) -> Vec<&'a str> {
    let current = current.trim();
    let mut choices: Vec<&str> = Vec::with_capacity(category.len() + history.len());
    for dir in category.iter().chain(history).map(String::as_str) {
        if dir != current && !choices.contains(&dir) {
            choices.push(dir);
        }
    }
    choices
}

/// 解析分类目录记忆：非对象为空；空串键丢弃，每个列表按 [`read_save_dir_history`] 解析，
/// 解析后为空的键丢弃。
pub(crate) fn read_category_save_dir_history(value: Option<&Value>) -> CategorySaveDirHistory {
    let Some(entries) = value.and_then(Value::as_object) else {
        return CategorySaveDirHistory::new();
    };
    entries
        .iter()
        .filter(|(category, _)| !category.is_empty())
        .filter_map(|(category, dirs)| {
            let dirs = read_save_dir_history(Some(dirs));
            (!dirs.is_empty()).then(|| (category.clone(), dirs))
        })
        .collect()
}

/// 在分类 `category` 下记录一次手动选择的目录（规则同 [`record_save_dir`]，其余分类不变）。
/// 分类为空或该分类列表无需变化时返回 `None`。
pub(crate) fn record_category_save_dir(
    history: &CategorySaveDirHistory,
    category: &str,
    dir: &str,
) -> Option<CategorySaveDirHistory> {
    if category.is_empty() {
        return None;
    }
    let current = history.get(category).map_or(&[][..], Vec::as_slice);
    let dirs = record_save_dir(current, dir)?;
    let mut next = history.clone();
    next.insert(category.to_owned(), dirs);
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::{
        CategorySaveDirHistory, SAVE_DIR_HISTORY_LIMIT, read_category_save_dir_history,
        read_save_dir_history, record_category_save_dir, record_save_dir, save_dir_choices,
    };
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixture {
        limit: usize,
        read: Vec<ReadCase>,
        record: Vec<RecordCase>,
        choices: Vec<ChoicesCase>,
        category_read: Vec<CategoryReadCase>,
        category_record: Vec<CategoryRecordCase>,
    }

    #[derive(Deserialize)]
    struct ReadCase {
        name: String,
        value: Value,
        expected: Vec<String>,
    }

    #[derive(Deserialize)]
    struct RecordCase {
        name: String,
        history: Vec<String>,
        dir: String,
        expected: Option<Vec<String>>,
    }

    #[derive(Deserialize)]
    struct ChoicesCase {
        name: String,
        category: Vec<String>,
        history: Vec<String>,
        current: String,
        expected: Vec<String>,
    }

    #[derive(Deserialize)]
    struct CategoryReadCase {
        name: String,
        value: Value,
        expected: CategorySaveDirHistory,
    }

    #[derive(Deserialize)]
    struct CategoryRecordCase {
        name: String,
        history: CategorySaveDirHistory,
        category: String,
        dir: String,
        expected: Option<CategorySaveDirHistory>,
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/save_dir_history_cases.json"
        ))
        .expect("save dir history fixture")
    }

    #[test]
    fn shared_fixture_cases() {
        let fixture = fixture();
        assert_eq!(fixture.limit, SAVE_DIR_HISTORY_LIMIT);
        for case in fixture.read {
            let value = (!case.value.is_null()).then_some(&case.value);
            assert_eq!(read_save_dir_history(value), case.expected, "{}", case.name);
        }
        for case in fixture.record {
            assert_eq!(
                record_save_dir(&case.history, &case.dir),
                case.expected,
                "{}",
                case.name
            );
        }
        for case in fixture.choices {
            assert_eq!(
                save_dir_choices(&case.category, &case.history, &case.current),
                case.expected,
                "{}",
                case.name
            );
        }
        for case in fixture.category_read {
            let value = (!case.value.is_null()).then_some(&case.value);
            assert_eq!(
                read_category_save_dir_history(value),
                case.expected,
                "{}",
                case.name
            );
        }
        for case in fixture.category_record {
            assert_eq!(
                record_category_save_dir(&case.history, &case.category, &case.dir),
                case.expected,
                "{}",
                case.name
            );
        }
    }
}
