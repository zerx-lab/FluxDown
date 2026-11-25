//! Manual HTTP(S) filename ranges; imported URLs and commands remain literal.

use super::new_download::UrlEntry;

pub(super) const MAX_GENERATED_URLS: usize = 1000;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UrlRangeExpansion {
    pub(crate) generated_count: usize,
    pub(crate) preview: Option<(usize, usize)>,
    pub(crate) limit_exceeded: bool,
}

impl UrlRangeExpansion {
    pub(super) fn push(&mut self, entries: &mut Vec<UrlEntry>, mut entry: UrlEntry, manual: bool) {
        if self.limit_exceeded {
            return;
        }
        let Some(range) = manual.then(|| filename_range(&entry.url)).flatten() else {
            entries.push(entry);
            return;
        };
        if range.count > MAX_GENERATED_URLS - self.generated_count {
            self.limit_exceeded = true;
            self.preview = None;
            entries.clear();
            return;
        }
        entries.reserve(range.count);
        let first = entries.len();
        let mut digits = range.start.as_bytes().to_vec();
        for index in 0..range.count {
            if index > 0 {
                increment(&mut digits);
            }
            let mut url =
                String::with_capacity(range.prefix.len() + digits.len() + range.suffix.len());
            url.push_str(range.prefix);
            url.extend(digits.iter().map(|digit| char::from(*digit)));
            url.push_str(range.suffix);
            // A literal out/checksum cannot describe every generated file.
            entries.push(UrlEntry {
                url,
                file_name: if range.count == 1 {
                    std::mem::take(&mut entry.file_name)
                } else {
                    String::new()
                },
                checksum: if range.count == 1 {
                    std::mem::take(&mut entry.checksum)
                } else {
                    String::new()
                },
            });
        }
        self.generated_count += range.count;
        self.preview = Some((
            self.preview.map_or(first, |(first, _)| first),
            entries.len() - 1,
        ));
    }
}

struct FilenameRange<'a> {
    prefix: &'a str,
    start: &'a str,
    suffix: &'a str,
    count: usize,
}

fn filename_range(url: &str) -> Option<FilenameRange<'_>> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let path_end = url.find(['?', '#']).unwrap_or(url.len());
    let path_url = url.get(..path_end)?;
    // An authority (including bracketed IPv6) is never a filename.
    let path_start = scheme.len() + 3 + rest.find('/')?;
    if path_start >= path_end {
        return None;
    }
    let file_start = path_url.rfind('/')? + 1;
    let filename = path_url.get(file_start..)?;
    if filename.bytes().filter(|byte| *byte == b'[').count() != 1
        || filename.bytes().filter(|byte| *byte == b']').count() != 1
    {
        return None;
    }
    let open = file_start + filename.find('[')?;
    let close = file_start + filename.find(']')?;
    let (start, end) = parse_decimal_bounds(url.get(open + 1..close)?)?;
    let lower = canonical_decimal(start);
    let upper = canonical_decimal(end);
    if (lower.len(), lower) > (upper.len(), upper) {
        return None;
    }
    Some(FilenameRange {
        prefix: url.get(..open)?,
        start,
        suffix: url.get(close + 1..)?,
        count: bounded_difference(lower, upper) + 1,
    })
}

pub(super) fn parse_decimal_bounds(text: &str) -> Option<(&str, &str)> {
    let (start, end) = text.split_once('-')?;
    let is_decimal = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    (is_decimal(start) && is_decimal(end)).then_some((start, end))
}

fn canonical_decimal(number: &str) -> &str {
    let number = number.trim_start_matches('0');
    if number.is_empty() { "0" } else { number }
}

// Decimal subtraction avoids machine-integer limits, including very wide padding.
fn bounded_difference(lower: &str, upper: &str) -> usize {
    let mut lower = lower.bytes().rev();
    let mut borrow = 0;
    let mut place = 1usize;
    let mut difference = 0usize;
    for digit in upper.bytes().rev() {
        let value =
            i16::from(digit - b'0') - i16::from(lower.next().unwrap_or(b'0') - b'0') - borrow;
        let (digit, next_borrow) = if value < 0 {
            (value + 10, 1)
        } else {
            (value, 0)
        };
        borrow = next_borrow;
        difference =
            (difference + usize::from(digit.unsigned_abs()) * place).min(MAX_GENERATED_URLS);
        place = (place * 10).min(MAX_GENERATED_URLS);
    }
    difference
}

fn increment(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit < b'9' {
            *digit += 1;
            return;
        }
        *digit = b'0';
    }
    digits.insert(0, b'1');
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::model::new_download::{
        DraftOptions, ParsedInput, UrlEntry, append_entries, build_requests, capture_entry,
        parse_entries, parse_input, parse_manual_input, retain_literal_urls,
    };

    fn manual(text: &str) -> ParsedInput {
        parse_manual_input(text, &HashSet::new())
    }

    #[test]
    fn url_range_preserves_padding_suffixes_order_and_preview() {
        let parsed = manual(
            "https://a/plain\nHTTPS://[::1]/目录/片[001-003].tar.gz?sig=%5B1-9%5D#part\n\
             https://b/other[9-10].zip\nhttps://c/tail",
        );
        assert_eq!(parsed.entries.len(), 7);
        assert_eq!(parsed.ranges.generated_count, 5);
        assert_eq!(parsed.ranges.preview, Some((1, 5)));
        assert_eq!(
            parsed.entries[1].url,
            "HTTPS://[::1]/目录/片001.tar.gz?sig=%5B1-9%5D#part"
        );
        assert_eq!(
            parsed.entries[3].url,
            "HTTPS://[::1]/目录/片003.tar.gz?sig=%5B1-9%5D#part"
        );
        assert_eq!(parsed.entries[5].url, "https://b/other10.zip");
        assert_eq!(parsed.entries[6].url, "https://c/tail");
    }

    #[test]
    fn url_range_only_expands_one_ascii_range_in_the_final_http_filename() {
        for url in [
            "https://a/[001-003]/file.zip",
            "https://a/file.zip?part=[001-003]",
            "https://a/file.zip#part=[001-003]",
            "https://a/file%5B001-003%5D.zip",
            "https://a/[003-001].zip",
            "https://a/[001-].zip",
            "https://a/[-003].zip",
            "https://a/[1-2-3].zip",
            "https://a/[１-３].zip",
            "https://a/[1-2][3-4].zip",
            "https://a/[[1-2]].zip",
            "https://a/[1-2.zip",
            "https://a/]1-2[.zip",
            "https://[::1]/",
            "ftp://a/[001-003].zip",
            "thunder://a/[001-003]",
            "magnet:?xt=urn:btih:[001-003]",
            "ed2k://|file|[001-003]|1|hash|/",
        ] {
            let parsed = manual(url);
            assert_eq!(parsed.entries.len(), 1, "{url}");
            assert_eq!(parsed.entries[0].url, url, "{url}");
            assert_eq!(parsed.ranges.generated_count, 0, "{url}");
            assert!(!parsed.ranges.limit_exceeded, "{url}");
        }
    }

    #[test]
    fn url_range_supports_arbitrary_decimal_width_and_numeric_carry() {
        let prefix = "0".repeat(80);
        let parsed = manual(&format!("https://a/[{prefix}009-{prefix}011].zip"));
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[2].url, format!("https://a/{prefix}011.zip"));
        let parsed =
            manual("https://a/[999999999999999999999999999998-1000000000000000000000000000000]");
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(
            parsed.entries[2].url,
            "https://a/1000000000000000000000000000000"
        );
        let parsed = manual("https://a/[000-2]");
        assert_eq!(parsed.entries[2].url, "https://a/002");
    }

    #[test]
    fn url_range_limit_is_cumulative_and_blocks_the_whole_form() {
        let boundary = manual("https://a/plain\nhttps://a/[001-500]\nhttps://b/[1-500]");
        assert_eq!(boundary.ranges.generated_count, 1000);
        assert_eq!(boundary.entries.len(), 1001);
        assert!(!boundary.ranges.limit_exceeded);
        for input in [
            "https://a/plain\nhttps://a/[001-501]\nhttps://b/[1-500]\nhttps://c/plain",
            "https://a/[0-1000]\naria2c https://a/after",
            "https://a/[0-99999999999999999999999999999999999999999999999999]",
        ] {
            let parsed = manual(input);
            assert!(parsed.ranges.limit_exceeded, "{input}");
            assert!(parsed.entries.is_empty(), "{input}");
            assert_eq!(parsed.ranges.preview, None);
        }
        let literals = "https://a/plain\n".repeat(1001);
        assert_eq!(manual(&literals).entries.len(), 1001);
    }

    #[test]
    fn url_range_does_not_reinterpret_commands_captures_or_txt_imports() {
        let url = "https://a/[001-003].zip";
        let digest = "ab".repeat(32);
        let command = manual(&format!(
            "aria2c -o literal.zip --checksum=sha-256={digest} '{url}'"
        ));
        assert_eq!(command.entries.len(), 1);
        assert_eq!(command.entries[0].url, url);
        assert_eq!(command.entries[0].file_name, "literal.zip");
        assert_eq!(command.entries[0].checksum, format!("sha-256={digest}"));
        assert_eq!(command.ranges.generated_count, 0);
        assert_eq!(parse_input(url, false).entries.len(), 1);
        assert_eq!(parse_entries(url, true).len(), 1);

        let capture = capture_entry(url, "captured.zip");
        let mut literals = HashSet::new();
        let text = append_entries("", [capture.clone()], &mut literals);
        let captured = parse_manual_input(&text, &literals);
        assert_eq!(captured.entries, vec![capture]);
        assert_eq!(captured.ranges.generated_count, 0);

        let url = "https://a/[001-003]";
        let imported = parse_entries(&format!("See: {url}.,\n  out=imported.zip"), true);
        let text = append_entries("https://b/file[7-8].zip", imported, &mut literals);
        let parsed = parse_manual_input(&text, &literals);
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[2].url, url);
        assert_eq!(parsed.entries[2].file_name, "imported.zip");
        assert_eq!(parsed.ranges.generated_count, 2);
    }

    #[test]
    fn url_range_ignored_imports_do_not_change_existing_manual_source() {
        let url = "https://a/[01-02]";
        let mut literals = HashSet::new();
        let imported = parse_entries(url, true);
        let text = append_entries(url, imported, &mut literals);
        assert_eq!(text, url);
        assert!(literals.is_empty());
        assert_eq!(
            parse_manual_input(&text, &literals).ranges.generated_count,
            2
        );

        let text = append_entries("", parse_entries(url, true), &mut literals);
        assert_eq!(parse_manual_input(&text, &literals).entries[0].url, url);
        retain_literal_urls("https://b/other.zip", &mut literals);
        assert!(literals.is_empty());
        assert_eq!(parse_manual_input(url, &literals).ranges.generated_count, 2);
    }

    #[test]
    fn url_range_never_clones_literal_output_names_or_checksums() {
        let parsed = manual("https://a/[001-003].zip\n  out=same.zip\n  checksum=sha-256=same");
        let requests = build_requests(
            &parsed.entries,
            &DraftOptions {
                rename: "single-only.zip".to_owned(),
                checksum: "sha-256=single-only".to_owned(),
                ..DraftOptions::default()
            },
        );
        assert_eq!(requests.items.len(), 3);
        for item in &requests.items {
            assert!(item.file_name.is_empty());
            assert!(item.checksum.is_empty());
        }
        let singleton = manual("https://a/[001-001].zip\n  out=one.zip\n  checksum=sha-256=one");
        assert_eq!(
            singleton.entries,
            vec![UrlEntry {
                url: "https://a/001.zip".to_owned(),
                file_name: "one.zip".to_owned(),
                checksum: "sha-256=one".to_owned(),
            }]
        );
    }
}
