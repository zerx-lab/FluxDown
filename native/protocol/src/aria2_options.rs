//! Strict aria2.conf-style import/export for the options FluxDown can honor.
//!
//! This edits native defaults, not an overlay and not an aria2 process. Unsupported
//! options never silently succeed. The daemon commits the returned values through
//! its normal revision-checked config patch path.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::daemon_config::{daemon_config_default, normalize_daemon_config_value};

/// No text exports current settings; text previews an import without writing.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Aria2OptionsRequest {
    pub text: Option<String>,
}

/// A validated native patch and portable text, tied to the current config revision.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aria2OptionsResponse {
    pub text: String,
    pub defaults: String,
    pub values: BTreeMap<String, String>,
    pub revision: u64,
    /// Draft validation failure, kept in the result so gateways preserve line/key details.
    /// When present, `values` is empty and clients must not submit a config patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// Only semantically equivalent options belong here. In particular aria2's
// max-tries, connection timeout, per-server limits, seed stopping logic and DHT6
// do not have equivalent native defaults and must not be accepted as no-ops.
const OPTIONS: &[(&str, &str)] = &[
    ("dir", "default_save_dir"),
    ("split", "default_segments"),
    ("max-concurrent-downloads", "max_concurrent_tasks"),
    ("max-overall-download-limit", "speed_limit_bytes"),
    ("max-overall-upload-limit", "upload_limit_bytes"),
    ("user-agent", "global_user_agent"),
    ("remote-time", "use_server_time"),
    ("enable-dht", "bt_enable_dht"),
    ("enable-upnp", "bt_enable_upnp"),
    ("bt-enable-lpd", "bt_enable_lsd"),
    ("bt-max-peers", "bt_max_connections_per_torrent"),
];

fn unit_bytes(value: &str) -> Result<String, String> {
    let (digits, multiplier) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (value.get(..value.len() - 1), 1024_i64),
        Some(b'm' | b'M') => (value.get(..value.len() - 1), 1024_i64.pow(2)),
        Some(b'g' | b'G') => (value.get(..value.len() - 1), 1024_i64.pow(3)),
        _ => (Some(value), 1),
    };
    digits
        .and_then(|digits| digits.parse::<i64>().ok())
        .filter(|number| *number >= 0)
        .and_then(|number| number.checked_mul(multiplier))
        .map(|number| number.to_string())
        .ok_or_else(|| "expected non-negative bytes with optional K/M/G suffix (1024-based)".into())
}

fn insert_value(
    values: &mut BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let value = normalize_daemon_config_value(key, value).map_err(|error| error.to_string())?;
    values.insert(key.to_owned(), value);
    Ok(())
}

fn parse_option(
    key: &str,
    value: &str,
    values: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    if key == "listen-port" {
        let (start, end) = value.split_once('-').unwrap_or((value, value));
        let start = start
            .parse::<u16>()
            .map_err(|_| "expected port or start-end")?;
        let end = end
            .parse::<u16>()
            .map_err(|_| "expected port or start-end")?;
        if start == 0 || start > end {
            return Err("expected 1 <= start <= end <= 65535".into());
        }
        insert_value(values, "bt_port_start", &start.to_string())?;
        return insert_value(values, "bt_port_end", &end.to_string());
    }
    if key == "bt-tracker" {
        let trackers = value
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n");
        return insert_value(values, "bt_custom_trackers", &trackers);
    }
    let Some((_, native)) = OPTIONS.iter().find(|(option, _)| *option == key) else {
        return Err("unsupported option; RPC, process, script and security options are not configurable here".into());
    };
    if matches!(
        key,
        "max-overall-download-limit" | "max-overall-upload-limit"
    ) {
        insert_value(values, native, &unit_bytes(value)?)
    } else {
        insert_value(values, native, value)
    }
}

/// Parse the whole draft atomically, rejecting duplicate keys and unknown options.
///
/// Comments start with `#` on their own line. Values may contain `=` and `#`.
/// Missing keys leave native settings unchanged. `split=0` retains FluxDown's
/// automatic connection selection, rather than aria2's factory default.
///
/// ```
/// let patch = fluxdown_protocol::aria2_options::parse("--split=8\nmax-overall-download-limit=2M")?;
/// assert_eq!(patch["speed_limit_bytes"], "2097152");
/// # Ok::<(), String>(())
/// ```
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, String> {
    if text.len() > 64 * 1024 {
        return Err("configuration exceeds 64 KiB".into());
    }
    let mut values = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let error = |message: &str| format!("line {}: {message}", index + 1);
        let Some((key, value)) = line.strip_prefix("--").unwrap_or(line).split_once('=') else {
            return Err(error("expected key=value or --key=value"));
        };
        let key = key.trim();
        if !seen.insert(key) {
            return Err(error(&format!("{key}: duplicate option")));
        }
        let value = value.trim();
        if value.chars().any(char::is_control) {
            return Err(error(&format!("{key}: control characters are not allowed")));
        }
        parse_option(key, value, &mut values)
            .map_err(|message| error(&format!("{key}: {message}")))?;
    }
    Ok(values)
}

/// Export only supported options; absent values use FluxDown's canonical defaults.
/// Unrepresentable multiline values fail rather than injecting extra options.
///
/// ```
/// let text = fluxdown_protocol::aria2_options::export(&Default::default())?;
/// assert!(text.contains("enable-dht=true"));
/// # Ok::<(), String>(())
/// ```
pub fn export(values: &BTreeMap<String, String>) -> Result<String, String> {
    let get = |key: &str| {
        values
            .get(key)
            .map(String::as_str)
            .unwrap_or_else(|| daemon_config_default(key))
    };
    let mut lines = Vec::with_capacity(OPTIONS.len() + 2);
    for (option, key) in OPTIONS {
        let value = get(key);
        if value.trim() != value || value.chars().any(char::is_control) {
            return Err(format!(
                "{option}: current value cannot be represented as a single config line"
            ));
        }
        lines.push(format!("{option}={value}"));
    }
    lines.push(format!(
        "listen-port={}-{}",
        get("bt_port_start"),
        get("bt_port_end")
    ));
    let trackers = get("bt_custom_trackers");
    if trackers
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\r')
    {
        return Err("bt-tracker: current value contains control characters".into());
    }
    lines.push(format!(
        "bt-tracker={}",
        trackers
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(",")
    ));
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{export, parse};

    #[test]
    fn imports_units_ports_comments_and_literal_values() {
        let values = parse("\u{feff}# comment\r\n--split=8\r\nmax-overall-download-limit=2M\nlisten-port=6900-6910\nuser-agent=agent=#1\nbt-tracker=https://a.test/announce,udp://b.test:80/announce").unwrap();
        assert_eq!(values["default_segments"], "8");
        assert_eq!(values["speed_limit_bytes"], "2097152");
        assert_eq!(values["bt_port_start"], "6900");
        assert_eq!(values["bt_port_end"], "6910");
        assert_eq!(values["global_user_agent"], "agent=#1");
        assert_eq!(
            values["bt_custom_trackers"],
            "https://a.test/announce\nudp://b.test:80/announce"
        );
        let roundtrip = parse(&export(&values).unwrap()).unwrap();
        for (key, value) in values {
            assert_eq!(roundtrip[&key], value);
        }
    }

    #[test]
    fn rejects_unsafe_unknown_duplicates_ranges_and_overflow_with_line_numbers() {
        for draft in [
            "split=8\n--split=4",
            "split=8\nrpc-secret=secret",
            "split=8\non-download-complete=cmd",
            "split=8\nconnect-timeout=5",
            "split=8\nmax-concurrent-downloads=0",
            "split=8\nlisten-port=3-2",
            "split=8\nlisten-port=0",
            "split=8\nmax-overall-download-limit=9223372036854775807G",
            "split=8\nmax-overall-download-limit=-1",
            "split=8\nenable-dht=maybe",
        ] {
            assert!(parse(draft).unwrap_err().starts_with("line 2:"), "{draft}");
        }
    }

    #[test]
    fn exported_defaults_restore_all_supported_native_defaults() {
        let values = parse(&export(&BTreeMap::new()).unwrap()).unwrap();
        for (key, value) in values {
            assert_eq!(
                value,
                crate::daemon_config::daemon_config_default(&key),
                "{key}"
            );
        }
        assert!(parse("# only comments").unwrap().is_empty());
        assert!(
            export(&BTreeMap::from([(
                "global_user_agent".into(),
                "x\nsplit=8".into()
            )]))
            .is_err()
        );
    }

    use std::collections::BTreeMap;
}
