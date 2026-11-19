use std::sync::LazyLock;

use fluxdown_ui_icon_pack::FileKind;
use regex::Regex;

// Keep in sync with Web contentBucket; both run series_title_cases.json.
static EPISODE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?:[ ._\[\]()-][Ss][0-9]{2}[Ee][0-9]{2,3}|第[0-9]{1,3}[话話集])(?:$|[ ._\[\]()-])| - [0-9]{2,3}(?:[Vv][0-9]{1,2})?(?: +[Ee][Nn][Dd])?(?: *(?:\[[^\]]*\]|\([^)]*\)))*$",
    )
    .ok()
});
static SUBTITLE_LANGUAGE_SUFFIX: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\.[A-Za-z]{2,3}(?:[-_][A-Za-z0-9]{2,8}){0,2}$").ok());
static SUBTITLE_FLAGS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i-u)(?:\.(?:forced|default|sdh|cc|hi))+$").ok());

pub(crate) fn series_title(name: &str) -> Option<&str> {
    let mut name = name.rsplit(['/', '\\']).next()?.trim_start_matches(' ');
    while let Some((_, rest)) = name.strip_prefix('[').and_then(|tag| tag.split_once(']')) {
        name = rest.trim_start_matches(' ');
    }
    if name.contains(" - ")
        && let Some((stem, extension)) = name.rsplit_once('.')
    {
        if ["ass", "ssa", "srt", "vtt"]
            .iter()
            .any(|suffix| extension.eq_ignore_ascii_case(suffix))
        {
            name = subtitle_stem(stem);
        } else if extension.eq_ignore_ascii_case("nfo")
            || FileKind::of_name(name) == FileKind::Video
        {
            // Video extensions follow assets/icon-packs/kinds.json on both clients.
            name = stem;
        }
    }
    let marker = EPISODE.as_ref()?.find(name)?;
    let title = name
        .get(..marker.start())?
        .trim_matches([' ', '.', '_', '-']);
    (!title.is_empty()).then_some(title)
}

fn subtitle_stem(stem: &str) -> &str {
    let strip_suffix = |regex: &LazyLock<Option<Regex>>, name: &str| {
        regex.as_ref()?.find(name).map(|suffix| suffix.start())
    };
    let stem = strip_suffix(&SUBTITLE_FLAGS, stem)
        .and_then(|index| stem.get(..index))
        .unwrap_or(stem);
    // A subtitle can retain the video's extension (e.g. Show - 05.mkv.en.srt).
    let video = |name| FileKind::of_name(name) == FileKind::Video;
    let candidate = if video(stem) {
        stem
    } else {
        let Some(base) =
            strip_suffix(&SUBTITLE_LANGUAGE_SUFFIX, stem).and_then(|index| stem.get(..index))
        else {
            return stem;
        };
        if !video(base) {
            return base;
        }
        base
    };
    candidate
        .rsplit_once('.')
        .map_or(candidate, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    #[test]
    fn series_titles_match_the_web_fixtures() -> Result<(), serde_json::Error> {
        let cases: Vec<(String, Option<String>)> =
            serde_json::from_str(include_str!("../../tests/fixtures/series_title_cases.json"))?;
        for (name, title) in &cases {
            assert_eq!(super::series_title(name), title.as_deref(), "{name}");
        }
        Ok(())
    }
}
