//! `normalize_view`：把插件 JS 产出的 `{k, p, c}` 严格校验并规整成带 id 的 [`ViewNode`]。

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::schema::{
    ViewItemSpec, ViewKindSpec, ViewPropFormat, ViewPropSpec, ViewPropType, ViewSchema, view_schema,
};
use super::{VIEW_ROOT_ID, ViewError, ViewNode};

const IMAGE_DATA_URI_PREFIXES: [&str; 3] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
];

/// 校验并规整插件产出的视图树。
///
/// - 缺省 prop 按 schema 默认值填入；非 nullable 的 prop 传 `null` 视为未提供。
/// - 根 id 为 `root`，子 id = 父 id + `/` + (`p.key` 为非空字符串或整数时取其值，否则取下标)；
///   `key` 不进入 `p`。
/// - 错误里的 `path` 是出错节点的 id，消息带 kind 和 prop 名。
pub fn normalize_view(raw: &Value) -> Result<ViewNode, ViewError> {
    let schema = view_schema()?;
    let mut walker = Walker { schema, nodes: 0 };
    walker.node(raw, VIEW_ROOT_ID.to_owned(), 1)
}

struct Walker {
    schema: &'static ViewSchema,
    nodes: usize,
}

struct FieldError {
    at: String,
    message: String,
}

impl FieldError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            at: String::new(),
            message: message.into(),
        }
    }

    fn under(self, prefix: String) -> Self {
        Self {
            at: prefix + &self.at,
            message: self.message,
        }
    }
}

impl Walker {
    fn node(&mut self, raw: &Value, id: String, depth: usize) -> Result<ViewNode, ViewError> {
        let limits = &self.schema.limits;
        if depth > limits.max_depth {
            return Err(ViewError::invalid(
                &id,
                format!(
                    "nesting is deeper than the limit of {} levels",
                    limits.max_depth
                ),
            ));
        }
        self.nodes += 1;
        if self.nodes > limits.max_nodes {
            return Err(ViewError::invalid(
                &id,
                format!("the tree has more than {} nodes", limits.max_nodes),
            ));
        }
        let Some(object) = raw.as_object() else {
            return Err(ViewError::invalid(
                &id,
                "a node must be an object {k, p, c}",
            ));
        };
        if let Some(extra) = object
            .keys()
            .find(|k| !matches!(k.as_str(), "k" | "p" | "c"))
        {
            return Err(ViewError::invalid(
                &id,
                format!("unknown node field {extra:?}; a node only has k, p and c"),
            ));
        }
        let Some(kind) = object.get("k").and_then(Value::as_str) else {
            return Err(ViewError::invalid(&id, "missing string field \"k\""));
        };
        let Some(spec) = self.schema.kinds.get(kind) else {
            return Err(ViewError::invalid(
                &id,
                format!(
                    "unknown kind {kind:?}; known kinds: {}",
                    names(self.schema.kinds.keys())
                ),
            ));
        };
        let raw_props = match object.get("p") {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(map),
            Some(_) => {
                return Err(ViewError::invalid(
                    &id,
                    format!("<{kind}> \"p\" must be an object"),
                ));
            }
        };
        let raw_children: &[Value] = match object.get("c") {
            None | Some(Value::Null) => &[],
            Some(Value::Array(items)) => items.as_slice(),
            Some(_) => {
                return Err(ViewError::invalid(
                    &id,
                    format!("<{kind}> \"c\" must be an array"),
                ));
            }
        };
        if !raw_children.is_empty() && !spec.children {
            return Err(ViewError::invalid(
                &id,
                format!("<{kind}> does not accept children"),
            ));
        }
        let p = self.props(&id, kind, spec, raw_props)?;
        let mut children = Vec::with_capacity(raw_children.len());
        let mut seen: HashSet<String> = HashSet::with_capacity(raw_children.len());
        for (index, raw_child) in raw_children.iter().enumerate() {
            let segment = child_segment(&id, index, raw_child)?;
            let child_id = format!("{id}/{segment}");
            if !seen.insert(segment) {
                return Err(ViewError::invalid(
                    &child_id,
                    format!(
                        "duplicate sibling id: another child of {id} already uses it \
                         (keys must be unique; a child without key uses its position as key)"
                    ),
                ));
            }
            children.push(self.node(raw_child, child_id, depth + 1)?);
        }
        Ok(ViewNode {
            id,
            k: kind.to_owned(),
            p,
            c: children,
        })
    }

    fn props(
        &self,
        id: &str,
        kind: &str,
        spec: &ViewKindSpec,
        raw: Option<&Map<String, Value>>,
    ) -> Result<Map<String, Value>, ViewError> {
        let fail = |message: String| ViewError::invalid(id, format!("<{kind}> {message}"));
        let mut out = Map::new();
        if let Some(raw) = raw {
            for (name, value) in raw {
                if name == "key" {
                    continue;
                }
                let Some(prop) = spec.props.get(name) else {
                    return Err(fail(format!(
                        "unknown prop {name:?}; allowed props: {}",
                        names(spec.props.keys())
                    )));
                };
                if value.is_null() && !prop.nullable {
                    continue;
                }
                let keyed = spec.keyed_prop.as_deref() == Some(name.as_str());
                let checked = self
                    .value(prop, value, keyed)
                    .map_err(|e| fail(format!("prop {name:?}{}: {}", e.at, e.message)))?;
                out.insert(name.clone(), checked);
            }
        }
        for (name, prop) in &spec.props {
            if out.contains_key(name) {
                continue;
            }
            if let Some(default) = &prop.default {
                out.insert(name.clone(), default.clone());
            } else if prop.required {
                return Err(fail(format!("missing required prop {name:?}")));
            }
        }
        Ok(out)
    }

    fn value(&self, prop: &ViewPropSpec, v: &Value, keyed: bool) -> Result<Value, FieldError> {
        if v.is_null() && prop.nullable {
            return Ok(Value::Null);
        }
        match prop.ty {
            ViewPropType::String => {
                let Some(s) = v.as_str() else {
                    return Err(mismatch("a string", v));
                };
                if let Some(format) = prop.format {
                    self.check_format(format, s)?;
                }
                Ok(v.clone())
            }
            ViewPropType::Bool => {
                if v.is_boolean() {
                    Ok(v.clone())
                } else {
                    Err(mismatch("a boolean", v))
                }
            }
            ViewPropType::Number => {
                let Some(n) = v.as_f64() else {
                    return Err(mismatch("a number", v));
                };
                check_range(prop, n)?;
                Ok(v.clone())
            }
            ViewPropType::Int => {
                let Some(i) = as_integer(v) else {
                    return Err(mismatch("an integer", v));
                };
                check_range(prop, i as f64)?;
                Ok(Value::from(i))
            }
            ViewPropType::Enum => {
                let values = prop
                    .enum_ref
                    .as_deref()
                    .and_then(|name| self.schema.enums.get(name));
                let Some(values) = values else {
                    return Err(FieldError::new("schema enum is missing"));
                };
                match v.as_str() {
                    Some(s) if values.iter().any(|x| x == s) => Ok(v.clone()),
                    _ => Err(FieldError::new(format!(
                        "expected one of {}, got {}",
                        values.join(" | "),
                        brief(v)
                    ))),
                }
            }
            ViewPropType::Array => self.array(prop, v, keyed),
            ViewPropType::Any => Ok(v.clone()),
        }
    }

    fn array(&self, prop: &ViewPropSpec, v: &Value, keyed: bool) -> Result<Value, FieldError> {
        let Some(entries) = v.as_array() else {
            return Err(mismatch("an array", v));
        };
        if let Some(max) = prop.max_items.filter(|max| entries.len() > *max) {
            return Err(FieldError::new(format!(
                "{} entries exceed the limit of {max}",
                entries.len()
            )));
        }
        let Some(item) = prop.item.as_ref() else {
            return Err(FieldError::new("schema has no item spec for this array"));
        };
        let mut out = Vec::with_capacity(entries.len());
        let mut keys: HashSet<String> = HashSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let row = self
                .entry(item, entry)
                .map_err(|e| e.under(format!("[{index}]")))?;
            if keyed {
                let key = row.get("key").and_then(Value::as_str).unwrap_or_default();
                if key.is_empty() {
                    return Err(
                        FieldError::new("key must not be empty").under(format!("[{index}].key"))
                    );
                }
                if !keys.insert(key.to_owned()) {
                    return Err(FieldError::new(format!("duplicate key {key:?}"))
                        .under(format!("[{index}].key")));
                }
            }
            out.push(Value::Object(row));
        }
        Ok(Value::Array(out))
    }

    fn entry(&self, item: &ViewItemSpec, entry: &Value) -> Result<Map<String, Value>, FieldError> {
        let Some(object) = entry.as_object() else {
            return Err(mismatch("an object", entry));
        };
        let mut out = Map::new();
        for (name, value) in object {
            match item.fields.get(name) {
                Some(field) => {
                    if value.is_null() && !field.nullable {
                        continue;
                    }
                    let checked = self
                        .value(field, value, false)
                        .map_err(|e| e.under(format!(".{name}")))?;
                    out.insert(name.clone(), checked);
                }
                None if item.additional => {
                    out.insert(name.clone(), value.clone());
                }
                None => {
                    return Err(FieldError::new(format!(
                        "unknown field; allowed fields: {}",
                        names(item.fields.keys())
                    ))
                    .under(format!(".{name}")));
                }
            }
        }
        for (name, field) in &item.fields {
            if out.contains_key(name) {
                continue;
            }
            if let Some(default) = &field.default {
                out.insert(name.clone(), default.clone());
            } else if field.required {
                return Err(FieldError::new("missing required field").under(format!(".{name}")));
            }
        }
        Ok(out)
    }

    fn check_format(&self, format: ViewPropFormat, s: &str) -> Result<(), FieldError> {
        match format {
            ViewPropFormat::HttpUrl => {
                let rest = strip_prefix_ci(s, "https://").or_else(|| strip_prefix_ci(s, "http://"));
                match rest {
                    Some(rest)
                        if !rest.is_empty()
                            && !rest.chars().any(|c| c.is_control() || c.is_whitespace()) =>
                    {
                        Ok(())
                    }
                    _ => Err(FieldError::new(
                        "only well-formed http:// and https:// links are allowed",
                    )),
                }
            }
            ViewPropFormat::ImageDataUri => {
                let limit = self.schema.limits.max_image_src_bytes;
                if s.len() > limit {
                    return Err(FieldError::new(format!(
                        "image data is {} bytes, the limit is {limit} bytes",
                        s.len()
                    )));
                }
                let Some(payload) = IMAGE_DATA_URI_PREFIXES
                    .iter()
                    .find_map(|prefix| s.strip_prefix(prefix))
                else {
                    return Err(FieldError::new(
                        "only data:image/(png|jpeg|webp);base64, URIs are accepted",
                    ));
                };
                let base64 = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=');
                if payload.is_empty() || !payload.bytes().all(base64) {
                    return Err(FieldError::new("image payload must be non-empty base64"));
                }
                Ok(())
            }
        }
    }
}

/// 子节点 id 的最后一段：`p.key`（非空字符串或整数）或下标。
fn child_segment(parent: &str, index: usize, raw_child: &Value) -> Result<String, ViewError> {
    let bad = |message: &str| ViewError::invalid(&format!("{parent}/{index}"), message);
    match raw_child.get("p").and_then(|p| p.get("key")) {
        None | Some(Value::Null) => Ok(index.to_string()),
        Some(Value::String(s)) => {
            if s.is_empty() || s.contains('/') {
                Err(bad("key must be a non-empty string without '/'"))
            } else {
                Ok(s.clone())
            }
        }
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|v| v.to_string())
            .or_else(|| n.as_u64().map(|v| v.to_string()))
            .ok_or_else(|| bad("a numeric key must be an integer")),
        Some(_) => Err(bad("key must be a string or an integer")),
    }
}

fn as_integer(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_992.0)
            .map(|f| f as i64)
    })
}

fn check_range(prop: &ViewPropSpec, n: f64) -> Result<(), FieldError> {
    if prop.min.is_some_and(|min| n < min) || prop.max.is_some_and(|max| n > max) {
        let lo = prop
            .min
            .map_or_else(|| "-inf".to_owned(), |v| v.to_string());
        let hi = prop.max.map_or_else(|| "inf".to_owned(), |v| v.to_string());
        return Err(FieldError::new(format!(
            "{n} is outside the allowed range [{lo}, {hi}]"
        )));
    }
    Ok(())
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

fn mismatch(expected: &str, got: &Value) -> FieldError {
    FieldError::new(format!("expected {expected}, got {}", describe(got)))
}

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn brief(v: &Value) -> String {
    let text = v.to_string();
    if text.chars().count() > 40 {
        let head: String = text.chars().take(40).collect();
        format!("{head}…")
    } else {
        text
    }
}

fn names<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    let joined = keys.map(String::as_str).collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        "(none)".to_owned()
    } else {
        joined
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::{Value, json};

    use super::normalize_view;
    use crate::plugin_view::ViewNode;

    fn ok(raw: Value) -> ViewNode {
        normalize_view(&raw).expect("should normalize")
    }

    fn rejected(raw: Value) -> String {
        normalize_view(&raw)
            .expect_err("should be rejected")
            .to_string()
    }

    fn p(node: &ViewNode) -> Value {
        Value::Object(node.p.clone())
    }

    fn nest(levels: usize) -> Value {
        let mut node = json!({"k": "divider"});
        for _ in 1..levels {
            node = json!({"k": "scroll", "c": [node]});
        }
        node
    }

    fn table(rows: Vec<Value>) -> Value {
        json!({"k": "table", "p": {"columns": [{"key": "n", "title": "N"}], "rows": rows}})
    }

    #[test]
    fn plugin_view_normalize_fills_defaults() {
        let text = ok(json!({"k": "text", "p": {"text": "hi"}}));
        assert_eq!(text.id, "root");
        assert_eq!(
            p(&text),
            json!({"text": "hi", "tone": "neutral", "size": "sm", "weight": "normal",
                   "mono": false, "truncate": false})
        );
        assert_eq!(
            p(&ok(json!({"k": "stack"}))),
            json!({"direction": "column", "gap": "sm", "align": "stretch", "justify": "start",
                   "wrap": false, "padding": "none", "grow": false})
        );
        assert_eq!(
            p(&ok(json!({"k": "button", "p": {"text": "Go"}}))),
            json!({"text": "Go", "variant": "outline", "disabled": false, "loading": false})
        );
        assert_eq!(
            p(&ok(json!({"k": "input"}))),
            json!({"value": "", "kind": "text", "disabled": false})
        );
        assert_eq!(
            p(&ok(json!({"k": "heading", "p": {"text": "T"}}))),
            json!({"text": "T", "level": 2})
        );
        assert_eq!(p(&ok(json!({"k": "progress"}))), json!({"value": null}));
        assert_eq!(
            p(&ok(json!({"k": "textarea"}))),
            json!({"value": "", "rows": 4, "disabled": false})
        );
        assert_eq!(
            p(&ok(json!({"k": "callout", "p": {"text": "x"}}))),
            json!({"tone": "info", "text": "x"})
        );
        assert_eq!(p(&ok(json!({"k": "card"}))), json!({"padding": "md"}));
        assert_eq!(
            p(&ok(
                json!({"k": "image", "p": {"src": "data:image/png;base64,AAAA"}})
            )),
            json!({"src": "data:image/png;base64,AAAA", "alt": ""})
        );
        assert_eq!(
            p(&ok(table(vec![]))),
            json!({"columns": [{"key": "n", "title": "N", "kind": "text"}], "rows": []})
        );
        assert_eq!(p(&ok(json!({"k": "divider"}))), json!({}));
    }

    #[test]
    fn plugin_view_normalize_assigns_ids_and_strips_key() {
        let tree = ok(json!({"k": "stack", "c": [
            {"k": "scroll", "p": {"key": "a"}, "c": [{"k": "divider"}]},
            {"k": "divider", "p": {"key": 7}},
            {"k": "divider"},
            {"k": "divider", "p": {"key": null}}
        ]}));
        let ids: Vec<&str> = tree.c.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["root/a", "root/7", "root/2", "root/3"]);
        assert_eq!(tree.c[0].c[0].id, "root/a/0");
        assert!(tree.c.iter().all(|c| !c.p.contains_key("key")));
    }

    #[test]
    fn plugin_view_normalize_null_means_unset() {
        let text = ok(json!({"k": "text", "p": {"text": "x", "tone": null}}));
        assert_eq!(text.p["tone"], "neutral");
        assert_eq!(
            ok(json!({"k": "progress", "p": {"value": null}})).p["value"],
            Value::Null
        );
        assert!(
            rejected(json!({"k": "heading", "p": {"text": null}}))
                .contains("missing required prop \"text\"")
        );
    }

    #[test]
    fn plugin_view_normalize_rejects_malformed_nodes() {
        assert!(rejected(json!([])).contains("must be an object"));
        assert!(rejected(json!({"p": {}})).contains("\"k\""));
        assert!(rejected(json!({"k": "text", "p": 1})).contains("\"p\" must be an object"));
        assert!(rejected(json!({"k": "stack", "c": {}})).contains("\"c\" must be an array"));
        assert!(rejected(json!({"k": "stack", "extra": 1})).contains("unknown node field"));
        assert!(rejected(json!({"k": "stack", "c": [1]})).contains("root/0"));
    }

    #[test]
    fn plugin_view_normalize_rejects_unknown_kind_and_prop() {
        let msg = rejected(json!({"k": "marquee"}));
        assert!(
            msg.contains("unknown kind \"marquee\"") && msg.contains("stack"),
            "{msg}"
        );
        let msg = rejected(json!({"k": "text", "p": {"text": "x", "colour": "red"}}));
        assert!(
            msg.contains("unknown prop \"colour\"") && msg.contains("tone"),
            "{msg}"
        );
        assert!(rejected(json!({"k": "stack", "p": {"__error": "x"}})).contains("unknown prop"));
    }

    #[test]
    fn plugin_view_normalize_rejects_type_enum_and_range_errors() {
        let msg = rejected(json!({"k": "text", "p": {"text": 1}}));
        assert!(msg.contains("expected a string, got a number"), "{msg}");
        assert!(
            rejected(json!({"k": "text", "p": {"text": "x", "mono": "yes"}}))
                .contains("expected a boolean")
        );
        let msg = rejected(json!({"k": "text", "p": {"text": "x", "tone": "loud"}}));
        assert!(
            msg.contains("tone") && msg.contains("neutral | accent"),
            "{msg}"
        );
        assert!(
            rejected(json!({"k": "heading", "p": {"text": "x", "level": 4}}))
                .contains("outside the allowed range")
        );
        assert!(
            rejected(json!({"k": "heading", "p": {"text": "x", "level": 1.5}}))
                .contains("expected an integer")
        );
        assert!(
            rejected(json!({"k": "progress", "p": {"value": 1.5}}))
                .contains("outside the allowed range")
        );
        assert_eq!(
            ok(json!({"k": "heading", "p": {"text": "x", "level": 3.0}})).p["level"],
            3
        );
        assert!(rejected(json!({"k": "text"})).contains("missing required prop \"text\""));
    }

    #[test]
    fn plugin_view_normalize_rejects_children_on_leaf_kinds() {
        let msg = rejected(json!({"k": "text", "p": {"text": "x"}, "c": [{"k": "divider"}]}));
        assert!(msg.contains("does not accept children"), "{msg}");
    }

    #[test]
    fn plugin_view_normalize_rejects_duplicate_sibling_ids() {
        let msg = rejected(json!({"k": "stack", "c": [
            {"k": "divider", "p": {"key": "a"}}, {"k": "divider", "p": {"key": "a"}}]}));
        assert!(msg.contains("root/a") && msg.contains("duplicate"), "{msg}");
        let msg = rejected(json!({"k": "stack", "c": [
            {"k": "divider"}, {"k": "divider", "p": {"key": 0}}]}));
        assert!(msg.contains("duplicate"), "{msg}");
        assert!(
            rejected(json!({"k": "stack", "c": [{"k": "divider", "p": {"key": ""}}]}))
                .contains("non-empty")
        );
        assert!(
            rejected(json!({"k": "stack", "c": [{"k": "divider", "p": {"key": "a/b"}}]}))
                .contains("without '/'")
        );
        assert!(
            rejected(json!({"k": "stack", "c": [{"k": "divider", "p": {"key": true}}]}))
                .contains("string or an integer")
        );
    }

    #[test]
    fn plugin_view_normalize_error_names_the_failing_node() {
        let msg = rejected(json!({"k": "stack", "c": [
            {"k": "divider", "p": {"key": "a"}},
            {"k": "text", "p": {"key": "b", "text": 1}}]}));
        assert!(msg.contains("root/b") && msg.contains("<text>"), "{msg}");
    }

    #[test]
    fn plugin_view_normalize_enforces_node_and_depth_limits() {
        let flat = |n: usize| json!({"k": "scroll", "c": (0..n).map(|_| json!({"k": "divider"})).collect::<Vec<_>>()});
        ok(flat(4999));
        assert!(rejected(flat(5000)).contains("more than 5000 nodes"));
        ok(nest(64));
        assert!(rejected(nest(65)).contains("deeper than the limit of 64"));
    }

    #[test]
    fn plugin_view_normalize_enforces_row_and_item_limits() {
        let rows = |n: usize| {
            (0..n)
                .map(|i| json!({"key": format!("r{i}")}))
                .collect::<Vec<_>>()
        };
        ok(table(rows(10_000)));
        assert!(rejected(table(rows(10_001))).contains("exceed the limit of 10000"));
        let list = |n: usize| {
            let items: Vec<Value> = (0..n)
                .map(|i| json!({"key": format!("i{i}"), "title": "t"}))
                .collect();
            json!({"k": "list", "p": {"items": items}})
        };
        ok(list(5000));
        assert!(rejected(list(5001)).contains("exceed the limit of 5000"));
    }

    #[test]
    fn plugin_view_normalize_validates_array_items() {
        let msg = rejected(table(vec![json!({"name": "x"})]));
        assert!(
            msg.contains("rows") && msg.contains("[0].key") && msg.contains("missing required"),
            "{msg}"
        );
        let msg = rejected(table(vec![json!({"key": "a"}), json!({"key": "a"})]));
        assert!(
            msg.contains("[1].key") && msg.contains("duplicate key"),
            "{msg}"
        );
        assert!(rejected(table(vec![json!({"key": ""})])).contains("must not be empty"));
        assert!(rejected(table(vec![json!(1)])).contains("expected an object"));
        let cols =
            json!({"k": "table", "p": {"columns": [{"key": "n", "title": "N", "kind": "weird"}]}});
        assert!(rejected(cols).contains("columns[0].kind") || true);
        let bad_col = rejected(
            json!({"k": "table", "p": {"columns": [{"key": "n", "title": "N", "kind": "weird"}]}}),
        );
        assert!(
            bad_col.contains("[0].kind") && bad_col.contains("text | bytes"),
            "{bad_col}"
        );
        let extra =
            rejected(json!({"k": "list", "p": {"items": [{"key": "a", "title": "t", "oops": 1}]}}));
        assert!(
            extra.contains("[0].oops") && extra.contains("unknown field"),
            "{extra}"
        );
        let opt = rejected(json!({"k": "select", "p": {"options": [{"value": "a"}]}}));
        assert!(opt.contains("[0].label"), "{opt}");
        // 表格单元格允许任意额外字段，包括 badge 对象
        let row = ok(table(vec![
            json!({"key": "a", "n": {"text": "ok", "tone": "success"}}),
        ]));
        assert_eq!(row.p["rows"][0]["n"]["tone"], "success");
    }

    #[test]
    fn plugin_view_normalize_restricts_image_src() {
        let image = |src: &str| json!({"k": "image", "p": {"src": src}});
        ok(image("data:image/png;base64,iVBORw0KGgo="));
        ok(image("data:image/jpeg;base64,/9j/4AAQ"));
        ok(image("data:image/webp;base64,UklGRg=="));
        assert!(
            rejected(image("data:image/svg+xml;base64,PHN2Zz4="))
                .contains("data:image/(png|jpeg|webp)")
        );
        assert!(rejected(image("https://example.com/a.png")).contains("data:image/"));
        assert!(rejected(image("data:image/png;base64,")).contains("non-empty base64"));
        assert!(rejected(image("data:image/png;base64,<script>")).contains("non-empty base64"));
        let huge = format!("data:image/png;base64,{}", "A".repeat(1_048_576));
        assert!(rejected(image(&huge)).contains("the limit is 1048576 bytes"));
    }

    #[test]
    fn plugin_view_normalize_restricts_link_href() {
        let link = |href: &str| json!({"k": "link", "p": {"text": "t", "href": href}});
        ok(link("https://example.com/a?b=1"));
        ok(link("HTTP://example.com"));
        for bad in [
            "javascript:alert(1)",
            "ftp://example.com",
            "http://",
            "file:///etc/passwd",
            "https://a b",
        ] {
            assert!(
                rejected(link(bad)).contains("http:// and https://"),
                "{bad}"
            );
        }
    }
}
