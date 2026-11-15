//! Client-local presentation of plugin settings. Never localize stored setting values.

use std::{borrow::Cow, collections::BTreeMap};

use serde::{Deserialize, Serialize};

use crate::SettingFieldDto;

/// Optional translated labels for one setting and one locale.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SettingLocalizationDto {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub helper_label: Option<String>,
    /// Existing option value → translated label.
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

impl SettingFieldDto {
    /// Resolve display text for a client locale, without changing keys or values.
    ///
    /// Missing/blank text falls back per label through locale parents, English,
    /// then the original manifest text. The original definition remains reusable
    /// when the client switches language; unchanged definitions are borrowed.
    ///
    /// ```
    /// use fluxdown_protocol::SettingFieldDto;
    /// let field: SettingFieldDto = serde_json::from_value(serde_json::json!({
    ///     "key": "quality", "title": "清晰度", "type": "string", "widget": "select",
    ///     "options": [{"value": "best", "label": "最高画质"}],
    ///     "localizations": {"en": {"title": "Quality", "options": {"best": "Best"}}}
    /// }))?;
    /// let english = field.localized("en-GB");
    /// assert_eq!(english.title, "Quality");
    /// assert_eq!(english.options[0].value, "best");
    /// assert_eq!(field.title, "清晰度");
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    #[must_use]
    pub fn localized(&self, locale: &str) -> Cow<'_, Self> {
        let mut result = Cow::Borrowed(self);
        if self.localizations.is_empty() {
            return result;
        }
        let locales = locale_candidates(locale);
        if let Some(title) = self.translated(&locales, |text| text.title.as_deref())
            && title != self.title
        {
            result.to_mut().title = title.to_owned();
        }
        if let Some(description) = self.translated(&locales, |text| text.description.as_deref())
            && description != self.description
        {
            result.to_mut().description = description.to_owned();
        }
        if let Some(label) = self.translated(&locales, |text| text.helper_label.as_deref())
            && Some(label) != self.helper_label.as_deref()
        {
            result.to_mut().helper_label = Some(label.to_owned());
        }
        for (index, option) in self.options.iter().enumerate() {
            if let Some(label) = self.translated(&locales, |text| {
                text.options.get(&option.value).map(String::as_str)
            }) && label != option.label
            {
                result.to_mut().options[index].label = label.to_owned();
            }
        }
        result
    }

    fn translated<'a>(
        &'a self,
        locales: &[String],
        get: impl Fn(&'a SettingLocalizationDto) -> Option<&'a str>,
    ) -> Option<&'a str> {
        locales.iter().find_map(|locale| {
            self.localizations
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(locale))
                .and_then(|(_, text)| get(text))
                .filter(|text| !text.trim().is_empty())
        })
    }
}

fn locale_candidates(locale: &str) -> Vec<String> {
    let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
    let mut candidates = Vec::new();
    let mut parent = normalized.as_str();
    let chinese = parent.split('-').next() == Some("zh");
    while !parent.is_empty() && (!chinese || parent != "zh") {
        candidates.push(parent.to_owned());
        parent = parent.rsplit_once('-').map_or("", |(prefix, _)| prefix);
    }
    if chinese {
        let mut subtags = normalized.split('-').skip(1);
        let script = subtags
            .clone()
            .find(|part| matches!(*part, "hans" | "hant"));
        let region = subtags.next();
        let aliases: &[&str] = match (script, region) {
            (Some("hant"), _) | (None, Some("tw" | "hk" | "mo")) => &["zh-hant", "zh-tw", "zh"],
            (Some("hans"), _) | (None, Some("cn" | "sg")) => &["zh-hans", "zh-cn", "zh"],
            (None, None) => &["zh", "zh-hans", "zh-cn"],
            _ => &["zh"],
        };
        for alias in aliases {
            if !candidates.iter().any(|candidate| candidate == alias) {
                candidates.push((*alias).to_owned());
            }
        }
    }
    if !candidates.iter().any(|candidate| candidate == "en") {
        candidates.push("en".to_owned());
    }
    candidates
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::SettingFieldDto;

    fn field() -> SettingFieldDto {
        serde_json::from_value(serde_json::json!({
            "key": "quality", "title": "清晰度", "description": "原始说明",
            "type": "string", "widget": "select", "default": "best", "required": true,
            "options": [{"value": "best", "label": "最高画质"}],
            "helperScript": "copy(document.cookie)", "helperLabel": "复制",
            "localizations": {
                "en": {"title": "Quality", "description": "Choose quality", "options": {"best": "Best"}, "helperLabel": "Copy"},
                "en-GB": {"title": " ", "description": "British description"},
                "zh": {"title": "中文标题"},
                "zh-Hant": {"title": "畫質", "options": {"best": "最高畫質"}},
                "zh-CN": {"title": "简体标题"},
                "zh-TW": {"title": "台灣標題"},
                "fr": {"title": "Qualité"}
            }
        })).unwrap()
    }

    #[test]
    fn labels_fall_back_individually_without_changing_saved_values() {
        let raw = field();
        let english = raw.localized(" EN_gb ");
        assert_eq!(english.title, "Quality");
        assert_eq!(english.description, "British description");
        assert_eq!(english.options[0].label, "Best");
        assert_eq!(english.options[0].value, "best");
        assert_eq!(english.default.as_deref(), Some("best"));
        assert_eq!(english.key, "quality");
        assert!(english.required);
        assert_eq!(english.helper_script, raw.helper_script);
        assert_eq!(english.helper_label.as_deref(), Some("Copy"));
        assert_eq!(raw.localized("fr-CA").title, "Qualité");
        assert_eq!(raw.localized("de-DE").title, "Quality");
        assert_eq!(raw.title, "清晰度");
        assert_eq!(raw.localized("en").title, "Quality");
    }

    #[test]
    fn chinese_scripts_override_conflicting_regions_and_preserve_regional_text() {
        let raw = field();
        for locale in ["zh-HK", "zh-MO", "zh-Hant-CN"] {
            assert_eq!(raw.localized(locale).title, "畫質", "{locale}");
        }
        assert_eq!(raw.localized("zh-TW").title, "台灣標題");
        assert_eq!(raw.localized("zh-Hans-TW").title, "简体标题");
        assert_eq!(raw.localized("zh-SG").title, "简体标题");
        assert_eq!(raw.localized("zh-Hant-TW").options[0].label, "最高畫質");
    }

    #[test]
    fn original_and_empty_translations_do_not_allocate_or_erase_labels() {
        let mut raw = field();
        raw.localizations.clear();
        assert!(matches!(raw.localized("en"), Cow::Borrowed(_)));
        raw.localizations
            .insert("en".to_owned(), Default::default());
        assert!(matches!(raw.localized("en-GB"), Cow::Borrowed(_)));
        assert_eq!(raw.localized("en-GB").title, "清晰度");
    }
}
