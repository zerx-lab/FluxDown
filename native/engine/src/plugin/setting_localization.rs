//! Validation for manifest setting display-text overrides.

use std::collections::HashSet;

use super::{manifest::SettingField, runtime::PluginError};

pub(super) fn validate(field: &SettingField) -> Result<(), PluginError> {
    let invalid = |reason: &str| {
        PluginError::ManifestInvalid(format!("setting '{}': localizations {reason}", field.key))
    };
    if field.localizations.len() > 64 {
        return Err(invalid("must contain at most 64 locales"));
    }
    let mut seen = HashSet::with_capacity(field.localizations.len());
    for (locale, text) in &field.localizations {
        if !valid_locale(locale) || !seen.insert(locale.to_ascii_lowercase()) {
            return Err(invalid("contains an invalid or duplicate locale"));
        }
        if text.helper_label.is_some() && field.helper_script.is_none() {
            return Err(invalid("helperLabel requires helperScript"));
        }
        if text
            .options
            .keys()
            .any(|value| !field.options.iter().any(|option| option.value == *value))
        {
            return Err(invalid("options must refer to existing option values"));
        }
        let strings = [
            text.title.as_deref(),
            text.description.as_deref(),
            text.helper_label.as_deref(),
        ];
        if strings
            .into_iter()
            .flatten()
            .chain(text.options.values().map(String::as_str))
            .any(|value| value.chars().count() > 4096)
        {
            return Err(invalid("text must contain at most 4096 characters"));
        }
    }
    Ok(())
}

fn valid_locale(locale: &str) -> bool {
    let mut parts = locale.split('-');
    let language = parts.next().unwrap_or_default();
    (2..=8).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_alphabetic())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}
