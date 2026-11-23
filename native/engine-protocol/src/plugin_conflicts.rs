use std::collections::{HashMap, hash_map::Entry};

use fluxdown_engine::plugin::manifest::url_glob_match;
use fluxdown_engine::plugin::routing::site_key;
use fluxdown_protocol::daemon::{PluginDto, PluginResolverUrlConflictDto};

pub(super) fn annotate(mut plugins: Vec<PluginDto>) -> Vec<PluginDto> {
    let normalized: Vec<Vec<_>> = plugins
        .iter()
        .map(|plugin| {
            if plugin.enabled
                || (plugin.load_status == "Loaded" && plugin.disabled_reason == "ConsentRequired")
            {
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
                            warnings.push(conflict(original, &plugin.identity, raw, None));
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
            // One example per (pair, site): clients group the pairs by site and
            // offer the site default as the fix, without listing every
            // overlapping declaration of the two plugins.
            let mut seen: Vec<Option<String>> = Vec::new();
            for (pattern, normalized) in left {
                for (candidate, other_normalized) in right {
                    if !overlaps(normalized, other_normalized) {
                        continue;
                    }
                    let site = shared_site(normalized, other_normalized);
                    if seen.contains(&site) {
                        continue;
                    }
                    seen.push(site.clone());
                    warnings.push(conflict(pattern, &other.identity, candidate, site));
                }
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
    site: Option<String>,
) -> PluginResolverUrlConflictDto {
    PluginResolverUrlConflictDto {
        pattern: pattern.to_owned(),
        other_identity: other_identity.to_owned(),
        other_pattern: other_pattern.to_owned(),
        site,
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

/// A hierarchical URL glob split into the parts a plugin author scopes a
/// pattern by.
struct UrlGlob<'a> {
    scheme: &'a str,
    host: &'a str,
    rest: &'a str,
}

/// Splits `scheme://host/rest`. `None` for non-hierarchical patterns
/// (`magnet:*`, `*`) and for a host star without a following delimiter
/// (`https://x.test*`), which may stand for the path as well.
fn split_url_glob(pattern: &str) -> Option<UrlGlob<'_>> {
    let (scheme, after) = pattern.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|ch| ch == '*' || ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return None;
    }
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (host, rest) = after.split_at_checked(end)?;
    if rest.is_empty() && host.contains('*') {
        return None;
    }
    Some(UrlGlob { scheme, host, rest })
}

/// Whether a URL plausibly matches both patterns. A star stays inside the
/// scheme, host or path it was written in: `*://*123pan.com/s/*` is meant for
/// 123pan hosts, so a Bilibili link that carries `123pan.com/s/` in its query
/// is not a realistic conflict even though the whole-URL glob would accept it.
fn overlaps(left: &str, right: &str) -> bool {
    match (split_url_glob(left), split_url_glob(right)) {
        (Some(l), Some(r)) => {
            glob_overlap(l.scheme, r.scheme)
                && glob_overlap(l.host, r.host)
                && glob_overlap(l.rest, r.rest)
        }
        // `magnet:*` never matches a `scheme://host` link.
        (Some(_), None) if is_opaque(right) => false,
        (None, Some(_)) if is_opaque(left) => false,
        _ => glob_overlap(left, right),
    }
}

/// A literal scheme without `//` (`magnet:?xt=…`): links with no host part.
fn is_opaque(pattern: &str) -> bool {
    pattern.split_once(':').is_some_and(|(scheme, rest)| {
        !scheme.is_empty() && !scheme.contains('*') && !rest.starts_with("//")
    })
}

/// Site key (as used by site default plugins) of the overlap: the literal host
/// of either pattern. `None` when both hosts are wildcards or the patterns are
/// not hierarchical URLs, so no single site default settles the conflict.
fn shared_site(left: &str, right: &str) -> Option<String> {
    [left, right]
        .into_iter()
        .filter_map(split_url_glob)
        .find(|glob| !glob.host.is_empty() && !glob.host.contains('*'))
        .and_then(|glob| site_key(&format!("http://{}/", glob.host)))
}

fn glob_overlap(left: &str, right: &str) -> bool {
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
    // When both globs have a star, a shared string exists if their prefixes
    // and suffixes are compatible: put both sets of interior literals between
    // the longer anchors.
    (lp.starts_with(rp) || rp.starts_with(lp)) && (ls.ends_with(rs) || rs.ends_with(ls))
}

#[cfg(test)]
mod tests {
    use fluxdown_engine::plugin::manifest::url_glob_match;
    use fluxdown_protocol::daemon::PluginDto;
    use serde_json::json;

    use super::{annotate, normalize, overlaps};

    fn sites(plugin: &PluginDto) -> Vec<(&str, Option<&str>)> {
        plugin
            .resolver_url_conflicts
            .iter()
            .map(|c| (c.other_identity.as_str(), c.site.as_deref()))
            .collect()
    }

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
                {"pattern": "https://x.test/*", "otherIdentity": "specific@test", "otherPattern": "https://x.test/watch*", "site": "x.test"}
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
    fn reports_each_shared_site_once_per_pair() {
        let youtube = plugin(
            "youtube@test",
            true,
            "None",
            &[
                "*://www.youtube.com/watch*",
                "*://www.youtube.com/shorts/*",
                "*://m.youtube.com/watch*",
            ],
        );
        let any_host = plugin("anyhost@test", true, "None", &["*://*/*"]);
        let catch_all = plugin("all@test", true, "None", &["*"]);
        let magnet = plugin("magnet@test", true, "None", &["magnet:*"]);
        let plugins = annotate(vec![youtube, any_host, catch_all, magnet]);
        assert_eq!(
            sites(&plugins[0]),
            [
                ("anyhost@test", Some("youtube.com")),
                ("anyhost@test", Some("m.youtube.com")),
                ("all@test", Some("youtube.com")),
                ("all@test", Some("m.youtube.com")),
            ]
        );
        // Both hosts are wildcards, or the URL has no host: no site default fits.
        assert_eq!(sites(&plugins[1]), [("all@test", None)]);
        assert_eq!(sites(&plugins[2]), [("magnet@test", None)]);
        assert!(plugins[3].resolver_url_conflicts.is_empty());
    }

    #[test]
    fn overlap_scopes_stars_to_url_parts() {
        for (left, right, witness) in [
            (
                "https://*/download",
                "https://x.test/download",
                "https://x.test/download",
            ),
            (
                "https://*/*/download",
                "HTTPS://x.test/a/download",
                "https://x.test/a/download",
            ),
            (
                "*://*.youtube.com/*",
                "https://www.youtube.com/watch*",
                "https://www.youtube.com/watch?v=1",
            ),
            (
                "*://x.test/*",
                "https://X.test/watch*",
                "https://x.test/watch/42",
            ),
            (
                "https://x.test*",
                "https://x.test/path",
                "https://x.test/path",
            ),
            (
                "https://*.test/a*end",
                "*://x.test/*b*end",
                "https://x.test/abend",
            ),
            ("*", "https://x.test/path", "https://x.test/path"),
            ("MAGNET:**", "magnet:*", "magnet:?xt=urn:btih:aa"),
            (
                "magnet:*",
                "magnet:?xt=urn:btih:*",
                "magnet:?xt=urn:btih:aa",
            ),
            (
                "https://example.com*foo",
                "https://example.com/foo",
                "https://example.com/foo",
            ),
        ] {
            assert!(url_glob_match(left, witness), "{left} vs {witness}");
            assert!(url_glob_match(right, witness), "{right} vs {witness}");
            assert!(
                overlaps(&normalize(left), &normalize(right)),
                "{left} vs {right}"
            );
        }
    }

    #[test]
    fn incompatible_anchors_preclude_a_shared_url() {
        // No string can end with both literal suffixes, or start with both
        // literal prefixes. A single non-matching URL would not prove that.
        for (left, right) in [
            ("https://*/download", "https://x.test/*/upload"),
            ("https://*/download", "https://x.test/a/upload"),
            ("https://x.test/a*b", "https://x.test/a*bc"),
            ("https://x.test/watch*", "https://x.test/shorts/*"),
            ("https://x.test/*", "https://y.test/*"),
            ("https://x.test/*", "http://x.test/*"),
            ("https://x.test/*", "https://x.test:8443/*"),
            ("magnet:?xt=urn:btih:*", "magnet:?xt=urn:btmh:*"),
        ] {
            assert!(
                !overlaps(&normalize(left), &normalize(right)),
                "{left} vs {right}"
            );
        }
    }

    #[test]
    fn stars_do_not_leave_the_host_or_scheme() {
        // The whole-URL glob accepts a crafted URL for each pair (another site
        // in the query string), but the authors scoped them to different
        // hosts, so they are not reported.
        for (left, right, crafted) in [
            (
                "*://*123pan.com/s/*",
                "*://www.bilibili.com/bangumi/play/ss*",
                "https://www.bilibili.com/bangumi/play/ss1?u=123pan.com/s/x",
            ),
            (
                "*://*123pan.com/s/*",
                "*://www.youtube.com/watch*",
                "https://www.youtube.com/watch?u=//123pan.com/s/x",
            ),
            (
                "*://www.jbsou.cn/?*",
                "*://www.youtube.com/watch*",
                "https://www.youtube.com/watch?u=://www.jbsou.cn/?q",
            ),
            (
                "https://*/download",
                "https://x.test/*/download",
                "https://x.test/a/download",
            ),
        ] {
            assert!(url_glob_match(left, crafted), "{left} vs {crafted}");
            assert!(url_glob_match(right, crafted), "{right} vs {crafted}");
            assert!(
                !overlaps(&normalize(left), &normalize(right)),
                "{left} vs {right}"
            );
        }
    }
}
