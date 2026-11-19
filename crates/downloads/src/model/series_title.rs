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
    LazyLock::new(|| Regex::new(r"\.[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,2}$").ok());

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
            name = SUBTITLE_LANGUAGE_SUFFIX
                .as_ref()
                .and_then(|regex| regex.find(stem))
                .and_then(|tag| stem.get(..tag.start()))
                .unwrap_or(stem);
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
