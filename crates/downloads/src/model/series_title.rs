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

pub(crate) fn series_title(name: &str) -> Option<&str> {
    let mut name = name.rsplit(['/', '\\']).next()?.trim_start_matches(' ');
    while let Some((_, rest)) = name.strip_prefix('[').and_then(|tag| tag.split_once(']')) {
        name = rest.trim_start_matches(' ');
    }
    // A dash-number marker must end the name, apart from release suffixes and media extensions.
    if name.contains(" - ")
        && let Some((stem, extension)) = name.rsplit_once('.')
        && (FileKind::of_name(name) == FileKind::Video
            || ["ass", "ssa", "srt", "vtt", "nfo"]
                .iter()
                .any(|suffix| extension.eq_ignore_ascii_case(suffix)))
    {
        name = stem;
    }
    let marker = EPISODE.as_ref()?.find(name)?;
    let title = name
        .get(..marker.start())?
        .trim_matches([' ', '.', '_', '-']);
    (!title.is_empty()).then_some(title)
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
