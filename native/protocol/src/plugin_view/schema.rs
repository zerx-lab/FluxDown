//! `plugin_view_schema.json` 的 Rust 视图。JSON 同时被 TS 侧（SDK 类型生成、Web 渲染器）直接读取。

use std::{collections::BTreeMap, sync::LazyLock};

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::ViewError;

/// 节点表原文（单一事实源）。
pub const VIEW_SCHEMA_JSON: &str = include_str!("../plugin_view_schema.json");
/// 色调 → 主题 registry token 路径映射原文。
pub const VIEW_TONES_JSON: &str = include_str!("../plugin_view_tones.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewSchema {
    pub version: u32,
    pub limits: ViewLimits,
    pub enums: BTreeMap<String, Vec<String>>,
    pub kinds: BTreeMap<String, ViewKindSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_image_src_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewKindSpec {
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub children: bool,
    #[serde(default)]
    pub props: BTreeMap<String, ViewPropSpec>,
    /// 可增量更新的键控数组 prop 名（`table → rows`、`list → items`）。
    #[serde(default)]
    pub keyed_prop: Option<String>,
    #[serde(default)]
    pub events: BTreeMap<String, ViewEventSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewPropType {
    String,
    Bool,
    Number,
    Int,
    Enum,
    Array,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewPropFormat {
    HttpUrl,
    ImageDataUri,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewPropSpec {
    #[serde(rename = "type")]
    pub ty: ViewPropType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub nullable: bool,
    /// 缺省值；JSON 里显式写 `null` 表示默认值就是 null。
    #[serde(default, deserialize_with = "present")]
    pub default: Option<Value>,
    #[serde(default, rename = "enum")]
    pub enum_ref: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub format: Option<ViewPropFormat>,
    #[serde(default)]
    pub item: Option<ViewItemSpec>,
    #[serde(default)]
    pub max_items: Option<usize>,
    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewItemSpec {
    pub fields: BTreeMap<String, ViewPropSpec>,
    /// 允许 `fields` 之外的任意字段（表格单元格）。
    #[serde(default)]
    pub additional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewEventPayloadType {
    String,
    Bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewEventSpec {
    #[serde(default)]
    pub payload: BTreeMap<String, ViewEventPayloadType>,
    #[serde(default)]
    pub debounce_ms: Option<u32>,
}

fn present<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

static SCHEMA: LazyLock<Result<ViewSchema, String>> = LazyLock::new(|| {
    let schema: ViewSchema = serde_json::from_str(VIEW_SCHEMA_JSON)
        .map_err(|e| format!("plugin_view_schema.json: {e}"))?;
    schema.check()?;
    Ok(schema)
});

/// 内置节点表；只有构建缺陷（JSON 损坏）才会返回 `Err`。
pub fn view_schema() -> Result<&'static ViewSchema, ViewError> {
    match &*SCHEMA {
        Ok(schema) => Ok(schema),
        Err(message) => Err(ViewError::Schema(message.clone())),
    }
}

impl ViewSchema {
    pub fn kind(&self, name: &str) -> Option<&ViewKindSpec> {
        self.kinds.get(name)
    }

    /// 该 kind 的键控数组 prop 名。
    pub fn keyed_prop(&self, kind: &str) -> Option<&str> {
        self.kinds.get(kind)?.keyed_prop.as_deref()
    }

    fn check(&self) -> Result<(), String> {
        for (kind, spec) in &self.kinds {
            for (name, prop) in &spec.props {
                self.check_prop(&format!("{kind}.{name}"), prop, true)?;
            }
            if let Some(keyed) = &spec.keyed_prop {
                let prop = spec
                    .props
                    .get(keyed)
                    .ok_or_else(|| format!("{kind}: keyedProp {keyed:?} is not a prop"))?;
                let key_field = prop
                    .item
                    .as_ref()
                    .and_then(|item| item.fields.get("key"))
                    .filter(|field| field.ty == ViewPropType::String && field.required);
                if prop.ty != ViewPropType::Array || key_field.is_none() {
                    return Err(format!(
                        "{kind}.{keyed}: a keyed array needs items with a required string `key`"
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_prop(&self, at: &str, prop: &ViewPropSpec, allow_array: bool) -> Result<(), String> {
        if prop.required && prop.default.is_some() {
            return Err(format!("{at}: a required prop must not have a default"));
        }
        match prop.ty {
            ViewPropType::Enum => {
                let name = prop
                    .enum_ref
                    .as_deref()
                    .ok_or_else(|| format!("{at}: enum prop needs `enum`"))?;
                let values = self
                    .enums
                    .get(name)
                    .ok_or_else(|| format!("{at}: unknown enum {name:?}"))?;
                if let Some(Value::String(default)) = &prop.default
                    && !values.contains(default)
                {
                    return Err(format!("{at}: default {default:?} is not in enum {name:?}"));
                }
            }
            ViewPropType::Array => {
                if !allow_array {
                    return Err(format!("{at}: nested arrays are not supported"));
                }
                let item = prop
                    .item
                    .as_ref()
                    .ok_or_else(|| format!("{at}: array prop needs `item`"))?;
                for (field, spec) in &item.fields {
                    self.check_prop(&format!("{at}[].{field}"), spec, false)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{VIEW_TONES_JSON, view_schema};

    #[test]
    fn plugin_view_schema_loads_with_every_v1_kind() {
        let schema = view_schema().expect("schema loads");
        assert_eq!(schema.version, 1);
        let expected = [
            "stack", "scroll", "card", "section", "divider", "text", "heading", "markdown", "code",
            "link", "image", "badge", "progress", "spinner", "kv", "callout", "empty", "button",
            "switch", "checkbox", "select", "input", "textarea", "tabs", "table", "list",
        ];
        assert_eq!(schema.kinds.len(), expected.len());
        for kind in expected {
            assert!(schema.kinds.contains_key(kind), "missing kind {kind}");
        }
    }

    #[test]
    fn plugin_view_only_table_rows_and_list_items_are_keyed() {
        let schema = view_schema().unwrap();
        let keyed: BTreeMap<&str, &str> = schema
            .kinds
            .iter()
            .filter_map(|(kind, spec)| spec.keyed_prop.as_deref().map(|p| (kind.as_str(), p)))
            .collect();
        assert_eq!(
            keyed,
            BTreeMap::from([("list", "items"), ("table", "rows")])
        );
    }

    #[test]
    fn plugin_view_only_layout_kinds_take_children() {
        let schema = view_schema().unwrap();
        let containers: BTreeSet<&str> = schema
            .kinds
            .iter()
            .filter(|(_, spec)| spec.children)
            .map(|(kind, _)| kind.as_str())
            .collect();
        assert_eq!(
            containers,
            BTreeSet::from(["card", "scroll", "section", "stack"])
        );
    }

    #[test]
    fn plugin_view_tones_fixture_matches_schema_enum() {
        let schema = view_schema().unwrap();
        let tones: BTreeMap<String, String> = serde_json::from_str(VIEW_TONES_JSON).unwrap();
        let from_schema: BTreeSet<&str> = schema.enums["tone"].iter().map(String::as_str).collect();
        let from_fixture: BTreeSet<&str> = tones.keys().map(String::as_str).collect();
        assert_eq!(from_schema, from_fixture);
        assert!(tones.values().all(|path| path.starts_with("colors.")));
        assert_eq!(tones["danger"], "colors.destructive");
    }
}
