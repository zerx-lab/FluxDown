//! Device-local named User-Agent values shared by desktop settings and download forms.

use serde::{Deserialize, Serialize};

/// Agent preference containing an array of [`UserAgentPreset`] objects.
pub const PREF_KEY: &str = "download.user_agent_presets";

/// A reusable value; tasks copy `value` rather than retaining a preset reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAgentPreset {
    /// User-visible label.
    pub name: String,
    /// HTTP User-Agent header value.
    pub value: String,
}

/// Reads saved presets, skipping invalid entries and retaining the first occurrence of each value.
///
/// ```
/// use fluxdown_protocol::user_agent_presets::from_preference;
/// let value = serde_json::json!([{"name": " Example ", "value": " Agent/1 "}]);
/// assert_eq!(from_preference(Some(&value))[0].value, "Agent/1");
/// ```
#[must_use]
pub fn from_preference(value: Option<&serde_json::Value>) -> Vec<UserAgentPreset> {
    let Some(entries) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut presets: Vec<UserAgentPreset> = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(value) = entry.get("value").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty()
            || value.is_empty()
            || value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127)
            || presets.iter().any(|preset| preset.value == value)
        {
            continue;
        }
        presets.push(UserAgentPreset {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
    presets
}

#[cfg(test)]
mod tests {
    use super::from_preference;

    #[test]
    fn corrupt_entries_do_not_hide_valid_presets_or_duplicate_values() {
        let input = serde_json::json!([
            null,
            {"name": "", "value": "Empty name"},
            {"name": "bad", "value": "Injected\r\nHeader: value"},
            {"name": "bad", "value": "Control\u{7f}"},
            {"name": " First ", "value": " Agent/1 "},
            {"name": "Duplicate", "value": "Agent/1"},
            {"name": "第二个", "value": "Agent/2\tcomment"}
        ]);
        let presets = from_preference(Some(&input));
        assert_eq!(
            presets
                .iter()
                .map(|p| (p.name.as_str(), p.value.as_str()))
                .collect::<Vec<_>>(),
            vec![("First", "Agent/1"), ("第二个", "Agent/2\tcomment")]
        );
        assert!(from_preference(Some(&serde_json::json!({}))).is_empty());
    }
}
