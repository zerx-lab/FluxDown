//! Desktop size and rate labels. Both styles use binary scaling.

use std::sync::atomic::{AtomicBool, Ordering};

/// Device preference; deliberately absent from the settings sync catalog.
pub const PREF_KEY: &str = "appearance.file_size_units";

static IEC: AtomicBool = AtomicBool::new(false);

/// Applies the local preference and reports whether existing windows need a refresh.
pub fn apply(preference: Option<&str>) -> bool {
    let iec = preference == Some("iec");
    IEC.swap(iec, Ordering::Relaxed) != iec
}

/// Size suffixes in increasing powers of 1024, starting with bytes.
pub fn labels() -> &'static [&'static str; 6] {
    labels_for(IEC.load(Ordering::Relaxed))
}

fn labels_for(iec: bool) -> &'static [&'static str; 6] {
    if iec {
        &["B", "KiB", "MiB", "GiB", "TiB", "PiB"]
    } else {
        &["B", "KB", "MB", "GB", "TB", "PB"]
    }
}

/// Rate suffixes in increasing powers of 1024, starting with bytes per second.
pub fn rate_labels() -> &'static [&'static str; 6] {
    if IEC.load(Ordering::Relaxed) {
        &["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s", "PiB/s"]
    } else {
        &["B/s", "KB/s", "MB/s", "GB/s", "TB/s", "PB/s"]
    }
}

/// Format a byte count with one decimal above bytes and clamp the displayed scale to `max_power`.
pub fn format_bytes(bytes: u64, max_power: usize) -> String {
    format_with_labels(bytes, max_power, labels())
}

fn format_with_labels(bytes: u64, max_power: usize, units: &[&str; 6]) -> String {
    let mut value = bytes as f64;
    let mut label = "B";
    for next in units.iter().skip(1).take(max_power) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        label = next;
    }
    if label == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {label}")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_with_labels, labels_for};

    #[test]
    fn styles_keep_the_same_binary_boundaries_and_precision() {
        let legacy = labels_for(false);
        let iec = labels_for(true);
        assert_eq!(format_with_labels(1023, 5, iec), "1023 B");
        assert_eq!(format_with_labels(1024, 5, legacy), "1.0 KB");
        assert_eq!(format_with_labels(1024, 5, iec), "1.0 KiB");
        assert_eq!(format_with_labels(1536 * 1024, 5, iec), "1.5 MiB");
        assert_eq!(format_with_labels(1 << 40, 3, legacy), "1024.0 GB");
        assert_eq!(format_with_labels(1 << 40, 3, iec), "1024.0 GiB");
    }
}
