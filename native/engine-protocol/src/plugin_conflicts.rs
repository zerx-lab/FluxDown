use std::collections::{HashMap, hash_map::Entry};

use fluxdown_engine::plugin::manifest::url_glob_match;
use fluxdown_protocol::daemon::{PluginDto, PluginResolverUrlConflictDto};

pub(super) fn annotate(mut plugins: Vec<PluginDto>) -> Vec<PluginDto> {
    let normalized: Vec<Vec<_>> = plugins
        .iter()
        .map(|plugin| {
            if plugin.enabled || plugin.disabled_reason == "ConsentRequired" {
                plugin
                    .resolver_urls
                    .iter()
                    .map(|raw| normalize(raw))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect();
    let mut warnings = vec![Vec::new(); plugins.len()];
    let unique: Vec<Vec<_>> = plugins
        .iter()
        .zip(&normalized)
        .zip(&mut warnings)
        .map(|((plugin, normalized), warnings)| {
            let mut first = HashMap::new();
            let mut unique = Vec::new();
            for (raw, key) in plugin.resolver_urls.iter().zip(normalized) {
                match first.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(Some(raw));
                        unique.push((raw, key));
                    }
                    Entry::Occupied(mut entry) => {
                        if let Some(original) = entry.get_mut().take() {
                            warnings.push(conflict(original, &plugin.identity, raw));
                        }
                    }
                }
            }
            unique
        })
        .collect();
    for (index, (left, warnings)) in unique.iter().zip(&mut warnings).enumerate() {
        if left.is_empty() {
            continue;
        }
        for (other, right) in plugins.iter().zip(&unique).skip(index + 1) {
            // One example per pair avoids multiplying the plugin list by the
            // number of overlapping declaration pairs.
            if let Some((pattern, candidate)) = left.iter().find_map(|(pattern, normalized)| {
                right
                    .iter()
                    .find(|(_, other_normalized)| overlaps(normalized, other_normalized))
                    .map(|(candidate, _)| (pattern, candidate))
            }) {
                warnings.push(conflict(pattern, &other.identity, candidate));
            }
        }
    }
    for (plugin, conflicts) in plugins.iter_mut().zip(warnings) {
        plugin.resolver_url_conflicts = conflicts;
    }
    plugins
}

fn conflict(
    pattern: &str,
    other_identity: &str,
    other_pattern: &str,
) -> PluginResolverUrlConflictDto {
    PluginResolverUrlConflictDto {
        pattern: pattern.to_owned(),
        other_identity: other_identity.to_owned(),
        other_pattern: other_pattern.to_owned(),
    }
}

fn normalize(pattern: &str) -> String {
    let mut normalized = String::with_capacity(pattern.len());
    for ch in pattern.chars() {
        if ch != '*' || !normalized.ends_with('*') {
            normalized.push(ch.to_ascii_lowercase());
        }
    }
    normalized
}

fn overlaps(left: &str, right: &str) -> bool {
    let Some((lp, ltail)) = left.split_once('*') else {
        return if right.contains('*') {
            url_glob_match(right, left)
        } else {
            left == right
        };
    };
    let Some((rp, rtail)) = right.split_once('*') else {
        return url_glob_match(left, right);
    };
    let ls = ltail.rsplit('*').next().unwrap_or(ltail);
    let rs = rtail.rsplit('*').next().unwrap_or(rtail);
    // Match the whole URL, as the resolver does. When both globs have a star,
    // a shared string exists if their prefixes and suffixes are compatible:
    // put both sets of interior literals between the longer anchors.
    (lp.starts_with(rp) || rp.starts_with(lp)) && (ls.ends_with(rs) || rs.ends_with(ls))
}

#[cfg(test)]
mod tests {
    use fluxdown_engine::plugin::manifest::url_glob_match;
    use fluxdown_protocol::daemon::PluginDto;
    use serde_json::json;

    use super::{annotate, normalize, overlaps};

    fn plugin(identity: &str, enabled: bool, disabled_reason: &str, urls: &[&str]) -> PluginDto {
        serde_json::from_value(json!({
            "identity": identity, "name": identity, "version": "1",
            "enabled": enabled, "disabledReason": disabled_reason, "devMode": false,
            "settings": [], "settingsValues": {}, "resolverUrls": urls,
        }))
        .unwrap()
    }

    #[test]
    fn lists_original_patterns_and_clears_cross_plugin_warning_when_disabled() {
        let generic = plugin(
            "generic@test",
            true,
            "None",
            &["https://x.test/*", "HTTPS://X.test/**"],
        );
        let specific = plugin(
            "specific@test",
            false,
            "ConsentRequired",
            &["https://x.test/watch*", "https://x.test/shorts/*"],
        );
        let unrelated = plugin("other@test", true, "None", &["https://y.test/*"]);
        let mut plugins = annotate(vec![generic, specific, unrelated]);
        let wire = serde_json::to_value(&plugins).unwrap();
        assert_eq!(
            wire[0]["resolverUrlConflicts"],
            json!([
                {"pattern": "https://x.test/*", "otherIdentity": "generic@test", "otherPattern": "HTTPS://X.test/**"},
                {"pattern": "https://x.test/*", "otherIdentity": "specific@test", "otherPattern": "https://x.test/watch*"}
            ])
        );
        assert!(wire[1].get("resolverUrlConflicts").is_none());
        assert!(wire[2].get("resolverUrlConflicts").is_none());

        plugins[1].disabled_reason = "Manual".into();
        let plugins = annotate(plugins);
        assert_eq!(plugins[0].resolver_url_conflicts.len(), 1);
        assert_eq!(
            plugins[0].resolver_url_conflicts[0].other_identity,
            "generic@test"
        );
    }

    #[test]
    fn overlap_follows_whole_url_glob_semantics() {
        let witness = "https://x.test/a/download";
        for pattern in ["https://*/download", "https://x.test/*/download"] {
            assert!(url_glob_match(pattern, witness));
        }
        for (left, right, expected) in [
            ("https://*/download", "https://x.test/*/download", true),
            ("https://*/download", "https://x.test/*/upload", false),
            ("https://*/download", "HTTPS://x.test/a/download", true),
            ("https://*/download", "https://x.test/a/upload", false),
            ("*://x.test/*", "https://X.test/watch*", true),
            ("https://x.test*", "https://x.test/path", true),
            ("https://*.test/a*end", "*://x.test/*b*end", true),
            ("https://x.test/a*b", "https://x.test/a*bc", false),
            ("https://x.test/watch*", "https://x.test/shorts/*", false),
            ("https://x.test/*", "https://y.test/*", false),
            ("https://x.test/*", "http://x.test/*", false),
            ("https://x.test/*", "https://x.test:8443/*", false),
            ("*", "https://x.test/path", true),
            ("MAGNET:**", "magnet:*", true),
        ] {
            assert_eq!(
                overlaps(&normalize(left), &normalize(right)),
                expected,
                "{left} vs {right}"
            );
        }
    }
}
