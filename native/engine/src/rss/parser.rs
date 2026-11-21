//! feed 解析封装：`bytes` → [`ParsedFeed`]。
//!
//! 用 [`feed_rs`] 统一覆盖 RSS 0.9x / 1.0 / 2.0 + Atom + JSON Feed，并顺带拿到
//! 编码探测（`quick-xml` 的 `encoding` feature，泛用 feed 里的 gbk/iso-8859）、
//! CDATA 与命名空间扩展（Mikan 的 `<torrent xmlns>`、media RSS、Dublin Core）。
//!
//! **解析永不 panic**：所有失败路径都收敛成 `Err(String)`，由调用方落进
//! `rss_sources.last_error`（引擎禁 `unwrap`/`expect` 的红线本就兜底）。

use std::collections::HashMap;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use feed_rs::model::{Entry, FeedType};
use feed_rs::parser as feed_parser;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer};
use quick_xml::Reader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use url::Url;

/// 单次抓取解析出的 feed。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedFeed {
    /// feed 标题（订阅名为空时用它回填）。
    pub title: String,
    /// feed 站点主页链接。
    pub link: String,
    /// 条目，保持 feed 内原始顺序（多数站点为「新→旧」）。
    pub items: Vec<ParsedItem>,
}

/// 一个 feed 条目的引擎投影。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedItem {
    /// 去重主键，见 [`parse_feed`] 的回退链说明。
    pub guid: String,
    /// 条目标题（空 = feed 未提供）。
    pub title: String,
    /// 条目页面链接。
    pub link: String,
    /// enclosure 直链（`.torrent`/媒体直链；空 = 无 enclosure）。
    pub enclosure_url: String,
    /// 下载目标声明的 MIME 类型（小写、不含参数；空 = 未声明）。来源优先级同
    /// enclosure 直链：enclosure / media:content，其次是回退链接的 `type`。
    pub enclosure_type: String,
    /// 可选二段解析标识；非空时由核心把条目 link 交给对应 resolver。
    pub resolver_item: String,
    /// enclosure 声明大小（字节，0 = 未知）。
    pub enclosure_length: i64,
    /// 发布时间（Unix 秒，0 = 未知）。
    pub pub_date: i64,
    /// Normalized external image URLs in display order.
    pub image_urls: Vec<String>,
}

/// 单个 feed 响应体的大小上限（8 MiB）。
///
/// 常见聚合 feed 在几十 KB 到几百 KB；8 MiB 已经是「这不是 feed」的量级，
/// 用于挡住误配成大文件直链的订阅地址把内存打爆。
pub const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;

/// feed 未给 `<guid>`/`<id>` 时，由本模块注入的自定义 ID 生成器写入的哨兵前缀。
///
/// `feed-rs` 只在 `entry.id` 为空时才调用生成器，因此「带前缀」精确等价于
/// 「原始 feed 没有 guid」——据此才能在不猜测其内部哈希实现的前提下应用设计
/// 文档 §2.2 的回退链（enclosure URL 优先于条目链接）。用 `\u{1}` 起头确保不
/// 会与任何真实 guid 撞车。
const GENERATED_ID_PREFIX: &str = "\u{1}fluxdown-noguid\u{1}";

/// 解析 feed 字节流。
///
/// # 去重主键（guid）回退链（设计文档 §2.2）
///
/// 1. feed 显式提供的 `<guid>`（RSS 2.0，`isPermaLink` 无关，取文本）或
///    `<id>`（Atom / JSON Feed）；
/// 2. 无 guid 且有 enclosure → `sha256(enclosure_url)`；
/// 3. 无 guid 无 enclosure → `sha256(条目链接)`；
/// 4. 三者皆无 → `sha256(标题)`——**绝不用随机 UUID**，否则每轮抓取都会把
///    同一条目当成新条目重复下载。
///
/// # Errors
///
/// 字节流为空、超过 [`MAX_FEED_BYTES`]、非法 XML/JSON、或不是任何已知 feed
/// 格式时返回人类可读的错误串（直接落 `last_error`）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::rss::parser::parse_feed;
///
/// let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
/// <rss version="2.0"><channel>
///   <title>Mikan Project</title>
///   <link>https://mikanani.me/</link>
///   <item>
///     <guid isPermaLink="false">abc123</guid>
///     <title>[ANi] Show - 02 [1080P]</title>
///     <link>https://mikanani.me/Home/Episode/abc123</link>
///     <enclosure url="https://mikanani.me/Download/x.torrent" length="438861824"
///                type="application/x-bittorrent"/>
///   </item>
/// </channel></rss>"#;
///
/// let feed = parse_feed(xml.as_bytes()).unwrap();
/// assert_eq!(feed.title, "Mikan Project");
/// assert_eq!(feed.items.len(), 1);
/// assert_eq!(feed.items[0].guid, "abc123");
/// assert_eq!(feed.items[0].enclosure_length, 438_861_824);
/// assert!(feed.items[0].enclosure_url.ends_with(".torrent"));
/// ```
pub fn parse_feed(bytes: &[u8]) -> Result<ParsedFeed, String> {
    parse_feed_at(bytes, None)
}

const MAX_IMAGE_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_IMAGE_HTML_BYTES_PER_FEED: usize = 256 * 1024;
const MAX_IMAGE_URL_BYTES_PER_FEED: usize = 512 * 1024;

struct ImageBudget {
    html_bytes: usize,
    url_bytes: usize,
}

pub(super) fn parse_feed_at(bytes: &[u8], base: Option<&Url>) -> Result<ParsedFeed, String> {
    if bytes.len() > MAX_FEED_BYTES {
        return Err(format!(
            "feed too large ({} bytes, limit {MAX_FEED_BYTES})",
            bytes.len()
        ));
    }
    if bytes.is_empty() {
        return Err("empty response".to_string());
    }
    let parser = feed_parser::Builder::new()
        .id_generator(|links, title, _uri| {
            let anchor = links
                .first()
                .map(|l| l.href.as_str())
                .filter(|h| !h.is_empty())
                .or_else(|| title.as_ref().map(|t| t.content.as_str()))
                .unwrap_or_default();
            format!("{GENERATED_ID_PREFIX}{anchor}")
        })
        .build();
    let feed = parser
        .parse(bytes)
        .map_err(|e| format!("failed to parse feed: {e}"))?;

    let rss2 = feed.feed_type == FeedType::RSS2;
    let mut image_budget = ImageBudget {
        html_bytes: MAX_IMAGE_HTML_BYTES_PER_FEED,
        url_bytes: MAX_IMAGE_URL_BYTES_PER_FEED,
    };
    let mut items: Vec<ParsedItem> = feed
        .entries
        .iter()
        .map(|entry| map_entry(entry, rss2, base, &mut image_budget))
        .collect();
    fill_missing_pub_dates(&mut items, bytes);
    Ok(ParsedFeed {
        title: feed.title.map(|t| t.content).unwrap_or_default(),
        link: feed
            .links
            .iter()
            .find(|l| l.rel.as_deref() != Some("self"))
            .or_else(|| feed.links.first())
            .map(|l| l.href.clone())
            .unwrap_or_default(),
        items,
    })
}

fn map_entry(
    entry: &Entry,
    rss2: bool,
    base: Option<&Url>,
    budget: &mut ImageBudget,
) -> ParsedItem {
    let (enclosure_url, enclosure_length, enclosure_mime) = extract_enclosure(entry);
    let link = entry
        .links
        .iter()
        .find(|l| l.rel.as_deref() != Some("enclosure"))
        .or_else(|| entry.links.first())
        .map(|l| l.href.clone())
        .unwrap_or_default();
    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .unwrap_or_default();

    let guid = match entry.id.strip_prefix(GENERATED_ID_PREFIX) {
        // feed 给了真实 guid/id：直接作主键。
        None => entry.id.clone(),
        Some(_) if !enclosure_url.is_empty() => sha256_hex(&enclosure_url),
        Some(_) if !link.is_empty() => sha256_hex(&link),
        Some(anchor) if !anchor.is_empty() => sha256_hex(anchor),
        Some(_) => sha256_hex(&title),
    };

    let enclosure_type = if enclosure_url.is_empty() {
        // 没有 enclosure 时下载回退到条目链接：声明在该链接上的类型才是目标类型。
        entry
            .links
            .iter()
            .find(|l| l.rel.as_deref() != Some("enclosure"))
            .or_else(|| entry.links.first())
            .and_then(|l| l.media_type.as_deref())
            .map(normalize_mime)
            .unwrap_or_default()
    } else {
        enclosure_mime
    };

    ParsedItem {
        guid,
        title,
        link,
        enclosure_url,
        enclosure_type,
        resolver_item: String::new(),
        enclosure_length,
        image_urls: match &entry.summary {
            Some(summary) if rss2 => extract_image_urls(&summary.content, base, budget),
            _ => Vec::new(),
        },
        pub_date: entry
            .published
            .or(entry.updated)
            .map(|d| d.timestamp())
            .unwrap_or(0),
    }
}

fn extract_image_urls(html: &str, base: Option<&Url>, budget: &mut ImageBudget) -> Vec<String> {
    if html.len() > MAX_IMAGE_DESCRIPTION_BYTES
        || html.len() > budget.html_bytes
        || budget.url_bytes == 0
    {
        return Vec::new();
    }
    budget.html_bytes -= html.len();
    let input = BufferQueue::default();
    input.push_back(html.into());
    let tokenizer = Tokenizer::new(
        ImageSink {
            base,
            remaining_url_bytes: std::cell::Cell::new(budget.url_bytes),
            urls: RefCell::new(Vec::new()),
        },
        Default::default(),
    );
    while !input.is_empty() {
        match tokenizer.feed(&input) {
            html5ever::tokenizer::TokenizerResult::Done
            | html5ever::tokenizer::TokenizerResult::Script(()) => {}
        }
    }
    tokenizer.end();
    budget.url_bytes = tokenizer.sink.remaining_url_bytes.get();
    tokenizer.sink.urls.into_inner()
}

struct ImageSink<'a> {
    base: Option<&'a Url>,
    remaining_url_bytes: std::cell::Cell<usize>,
    urls: RefCell<Vec<String>>,
}

impl TokenSink for ImageSink<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let Token::TagToken(tag) = token else {
            return TokenSinkResult::Continue;
        };
        if tag.kind != html5ever::tokenizer::StartTag {
            return TokenSinkResult::Continue;
        }
        match tag.name.as_ref() {
            "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                return TokenSinkResult::RawData(RawKind::Rawtext);
            }
            "textarea" | "title" => return TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => return TokenSinkResult::Plaintext,
            "img" => {}
            _ => return TokenSinkResult::Continue,
        }
        let mut urls = self.urls.borrow_mut();
        if urls.len() >= 4 {
            return TokenSinkResult::Continue;
        }
        if let Some(src) = tag
            .attrs
            .iter()
            .find(|attr| attr.name.local.as_ref() == "src")
        {
            let value = src.value.trim();
            if !value.is_empty()
                && value.len() <= 4096
                && let Ok(mut url) = Url::options().base_url(self.base).parse(value)
                && matches!(url.scheme(), "https" | "http")
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
            {
                url.set_fragment(None);
                let length = url.as_str().len();
                let remaining = self.remaining_url_bytes.get();
                if length <= 4096
                    && length <= remaining
                    && !urls.iter().any(|old| old == url.as_str())
                {
                    self.remaining_url_bytes.set(remaining - length);
                    urls.push(url.into());
                }
            }
        }
        TokenSinkResult::Continue
    }
}

/// 标准字段没给发布时间时，从条目的扩展命名空间里补一次。
///
/// 起因是 Mikan：它的 `<item>` **没有**标准 `<pubDate>`，时间只藏在自定义命名
/// 空间的 `<torrent xmlns="https://mikanani.me/0.1/"><pubDate>` 里；而 feed-rs
/// 2.x 不再暴露通用扩展节点（`Entry` 只有 media/dc 等已建模字段），于是整条订阅
/// 每一行都显示不出时间，条目流也只能退化成按入库顺序排。
///
/// 做法是对原始字节再走一遍极轻的 pull 解析：按 `<item>`/`<entry>` 分段，收集段
/// 内任意深度、任意前缀的 `guid`/`link`/`id` 作为键，取第一个能解析的
/// `pubDate`/`published`/`updated` 作为值，只回填 `pub_date == 0` 的条目。
/// feed 本来就已完整解析过一次，这里不做任何结构校验——扫不到就维持 0。
fn fill_missing_pub_dates(items: &mut [ParsedItem], bytes: &[u8]) {
    if !items.iter().any(|i| i.pub_date == 0) {
        return;
    }
    let dates = scan_extension_pub_dates(bytes);
    if dates.is_empty() {
        return;
    }
    for item in items.iter_mut().filter(|i| i.pub_date == 0) {
        item.pub_date = dates
            .get(&item.guid)
            .or_else(|| dates.get(&item.link))
            .or_else(|| dates.get(&item.enclosure_url))
            .copied()
            .unwrap_or(0);
    }
}

/// 条目标识（guid / link / enclosure 直链原文）→ Unix 秒。
fn scan_extension_pub_dates(bytes: &[u8]) -> HashMap<String, i64> {
    /// 当前正在累积的文本归属。
    #[derive(PartialEq, Eq)]
    enum Slot {
        None,
        Key,
        Date,
    }

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out: HashMap<String, i64> = HashMap::new();
    let mut in_item = false;
    let mut slot = Slot::None;
    let mut keys: Vec<String> = Vec::new();
    let mut date = 0i64;

    // 解析出错即停：feed-rs 已经成功解析过一遍，这里只是补充信息，
    // 任何异常都不该让整轮抓取失败。
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                slot = match e.local_name().as_ref() {
                    b"item" | b"entry" => {
                        in_item = true;
                        keys.clear();
                        date = 0;
                        Slot::None
                    }
                    b"guid" | b"link" | b"id" if in_item => Slot::Key,
                    b"pubDate" | b"pubdate" | b"published" | b"updated" if in_item => Slot::Date,
                    _ => Slot::None,
                };
            }
            Event::Text(t) => {
                if let Ok(text) = t.decode() {
                    match slot {
                        Slot::Key => push_key(&mut keys, text.as_ref()),
                        // 日期只认第一个能解析出来的：Mikan 的 `<torrent>` 里
                        // `<link>` 与 `<pubDate>` 并列，别被后续兄弟节点覆盖。
                        Slot::Date if date == 0 => date = parse_loose_datetime(text.as_ref()),
                        _ => {}
                    }
                }
            }
            Event::CData(t) => {
                if slot == Slot::Key
                    && let Ok(text) = t.decode()
                {
                    push_key(&mut keys, text.as_ref());
                }
            }
            Event::End(e) => {
                if matches!(e.local_name().as_ref(), b"item" | b"entry") {
                    if date > 0 {
                        for k in keys.drain(..) {
                            out.insert(k, date);
                        }
                    }
                    in_item = false;
                }
                slot = Slot::None;
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// 收一个条目标识（空串不进表，否则会把不同条目串到一起）。
fn push_key(keys: &mut Vec<String>, text: &str) {
    let key = text.trim();
    if !key.is_empty() {
        keys.push(key.to_string());
    }
}

/// 宽松解析发布时间 → Unix 秒（0 = 解析不出来）。
///
/// RFC 2822 / RFC 3339 之外还要吃「不带时区的本地时间」，Mikan 的
/// `2025-06-22T01:30:54.145714` 就是这种。这类值按**机器本地时区**解释：站点
/// 与读者通常同区，比一律当 UTC 少 8 小时的显示错更接近事实。
fn parse_loose_datetime(raw: &str) -> i64 {
    let raw = raw.trim();
    if raw.is_empty() {
        return 0;
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(raw) {
        return d.timestamp();
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(raw) {
        return d.timestamp();
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return local_timestamp(naive);
        }
    }
    if let Ok(day) = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        && let Some(naive) = day.and_hms_opt(0, 0, 0)
    {
        return local_timestamp(naive);
    }
    0
}

/// 本地时区解释一个无时区时间；夏令时折叠时取较早的那个解。
fn local_timestamp(naive: NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// 取 enclosure 直链、大小与声明的 MIME 类型。
///
/// 两种来源都要覆盖：
/// - RSS 2.0 的 `<enclosure>` 被 feed-rs 归一成 media RSS 的 `MediaContent`；
/// - Atom 用 `<link rel="enclosure" length="…" type="…">`。
fn extract_enclosure(entry: &Entry) -> (String, i64, String) {
    if let Some(content) = entry
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .find(|c| c.url.is_some())
        && let Some(url) = &content.url
    {
        return (
            url.to_string(),
            content.size.unwrap_or(0).min(i64::MAX as u64) as i64,
            content
                .content_type
                .as_ref()
                .map(|m| normalize_mime(m.as_ref()))
                .unwrap_or_default(),
        );
    }
    entry
        .links
        .iter()
        .find(|l| l.rel.as_deref() == Some("enclosure"))
        .map(|l| {
            (
                l.href.clone(),
                l.length.unwrap_or(0).min(i64::MAX as u64) as i64,
                l.media_type
                    .as_deref()
                    .map(normalize_mime)
                    .unwrap_or_default(),
            )
        })
        .unwrap_or_default()
}

/// MIME 规整：去参数（`; charset=…`）、去空白、转小写。
fn normalize_mime(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn description_images_survive_cdata_and_xml_escaping() -> Result<(), String> {
        let base = url::Url::parse("https://cdn.example.test/feed/latest.xml")
            .map_err(|e| e.to_string())?;
        for description in [
            r#"<![CDATA[<img src="../cover.png"><img src="../cover.png#dup"><img src="data:image/png;base64,x">]]>"#,
            r#"&lt;img src="../cover.png"&gt;"#,
        ] {
            let xml = format!(
                "<rss version=\"2.0\"><channel><title>Feed</title><item><guid>item</guid><title>Item</title><description>{description}</description></item></channel></rss>"
            );
            let feed = super::parse_feed_at(xml.as_bytes(), Some(&base))?;
            assert_eq!(
                feed.items.first().map(|item| item.image_urls.as_slice()),
                Some(["https://cdn.example.test/cover.png".to_owned()].as_slice())
            );
        }
        Ok(())
    }

    #[test]
    fn image_budget_limits_work_without_losing_feed_items() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::{
            MAX_IMAGE_DESCRIPTION_BYTES, MAX_IMAGE_HTML_BYTES_PER_FEED,
            MAX_IMAGE_URL_BYTES_PER_FEED,
        };
        use std::fmt::Write as _;

        let base = url::Url::parse(&format!(
            "https://cdn.example.test/{}/feed.xml",
            "a".repeat(3500)
        ))?;
        let image = "<img src='cover.png'>";
        let image_url_len = base.join("cover.png")?.as_str().len();
        let short = MAX_IMAGE_URL_BYTES_PER_FEED / image_url_len;
        let long = MAX_IMAGE_HTML_BYTES_PER_FEED / MAX_IMAGE_DESCRIPTION_BYTES;
        for (entries, padding, expected) in [
            (short + 2, 0, short),
            (long + 2, MAX_IMAGE_DESCRIPTION_BYTES - image.len(), long),
        ] {
            let mut xml = String::from("<rss version='2.0'><channel><title>Feed</title>");
            let oversized = "x".repeat(MAX_IMAGE_DESCRIPTION_BYTES);
            write!(
                xml,
                "<item><guid>oversized</guid><title>Large description</title><description><![CDATA[{image}{oversized}]]></description></item>"
            )?;
            let tail = "x".repeat(padding);
            for index in 0..entries {
                write!(
                    xml,
                    "<item><guid>{index}</guid><title>Item</title><description><![CDATA[{image}{tail}]]></description></item>"
                )?;
            }
            xml.push_str("</channel></rss>");
            let parsed = super::parse_feed_at(xml.as_bytes(), Some(&base))?;
            assert_eq!(parsed.items.len(), entries + 1);
            assert!(parsed.items[0].image_urls.is_empty());
            assert_eq!(
                parsed
                    .items
                    .iter()
                    .map(|item| item.image_urls.len())
                    .sum::<usize>(),
                expected
            );
        }
        Ok(())
    }

    use super::{MAX_FEED_BYTES, parse_feed, parse_loose_datetime};

    /// Mikan Project 的真实 feed 形状（issue #97 样例的最小复刻）。
    const MIKAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Mikan Project - 我的番组</title>
    <link>https://mikanani.me/RSS/MyBangumi?token=x</link>
    <description>Mikan Project - 我的番组</description>
    <item>
      <guid isPermaLink="false">a1b2c3</guid>
      <title>[ANi] 幼女战记 2 - 02 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4]</title>
      <link>https://mikanani.me/Home/Episode/a1b2c3</link>
      <torrent xmlns="https://mikanani.me/0.1/">
        <link>https://mikanani.me/Home/Episode/a1b2c3</link>
        <contentLength>438861824</contentLength>
        <pubDate>2026-07-27T05:31:00</pubDate>
      </torrent>
      <enclosure type="application/x-bittorrent" length="438861824"
                 url="https://mikanani.me/Download/20260727/a1b2c3.torrent"/>
    </item>
    <item>
      <guid isPermaLink="false">d4e5f6</guid>
      <title>[LoliHouse] Yani Neko - 02 [WebRip 1080p HEVC-10bit AAC][简繁内封]</title>
      <link>https://mikanani.me/Home/Episode/d4e5f6</link>
      <enclosure type="application/x-bittorrent" length="692944896"
                 url="https://mikanani.me/Download/20260710/d4e5f6.torrent"/>
      <pubDate>Fri, 10 Jul 2026 18:33:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_mikan_rss2_with_torrent_enclosures() {
        let feed = parse_feed(MIKAN.as_bytes()).unwrap();
        assert_eq!(feed.title, "Mikan Project - 我的番组");
        assert_eq!(feed.items.len(), 2);

        let first = &feed.items[0];
        assert_eq!(first.guid, "a1b2c3");
        assert!(first.title.starts_with("[ANi] 幼女战记 2 - 02"));
        assert_eq!(
            first.enclosure_url,
            "https://mikanani.me/Download/20260727/a1b2c3.torrent"
        );
        assert_eq!(first.enclosure_length, 438_861_824);
        assert_eq!(first.link, "https://mikanani.me/Home/Episode/a1b2c3");

        assert!(feed.items[1].pub_date > 0, "pubDate must be parsed");
    }

    /// Mikan 的 item 没有标准 `<pubDate>`，时间只在 `<torrent>` 扩展里；不补
    /// 这一手整条订阅就一行时间都显示不出来（也没法按发布时间排序）。
    #[test]
    fn fills_pub_date_from_mikan_torrent_extension() {
        let feed = parse_feed(MIKAN.as_bytes()).unwrap();
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 7, 27)
            .and_then(|d| d.and_hms_opt(5, 31, 0))
            .and_then(|naive| {
                chrono::TimeZone::from_local_datetime(&chrono::Local, &naive).earliest()
            })
            .unwrap()
            .timestamp();
        assert_eq!(feed.items[0].pub_date, expected);
    }

    #[test]
    fn parses_atom_with_enclosure_link() {
        let atom = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Podcast</title>
  <link href="https://example.com/"/>
  <updated>2026-07-01T00:00:00Z</updated>
  <entry>
    <id>tag:example.com,2026:ep1</id>
    <title>Episode 1</title>
    <link href="https://example.com/ep1"/>
    <link rel="enclosure" type="audio/mpeg" length="12345678"
          href="https://cdn.example.com/ep1.mp3"/>
    <updated>2026-07-01T00:00:00Z</updated>
  </entry>
</feed>"#;
        let feed = parse_feed(atom.as_bytes()).unwrap();
        assert_eq!(feed.items.len(), 1);
        let item = &feed.items[0];
        assert_eq!(item.guid, "tag:example.com,2026:ep1");
        assert_eq!(item.enclosure_url, "https://cdn.example.com/ep1.mp3");
        assert_eq!(item.enclosure_length, 12_345_678);
        assert_eq!(item.link, "https://example.com/ep1");
    }

    #[test]
    fn guid_falls_back_to_enclosure_hash_and_stays_stable_across_title_edits() {
        let make = |title: &str| {
            format!(
                r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><title>{title}</title><link>https://x.test/page</link>
<enclosure url="https://x.test/f.torrent" length="10" type="application/x-bittorrent"/></item>
</channel></rss>"#
            )
        };
        let a = parse_feed(make("original title").as_bytes()).unwrap();
        let b = parse_feed(make("EDITED title").as_bytes()).unwrap();
        assert_eq!(
            a.items[0].guid, b.items[0].guid,
            "no <guid> → key must come from the stable enclosure URL, not the title"
        );
        assert_eq!(a.items[0].guid.len(), 64, "sha256 hex");
    }

    #[test]
    fn guid_falls_back_to_link_hash_without_enclosure() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><title>only a link</title><link>https://x.test/page</link></item>
</channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(feed.items[0].guid.len(), 64);
        assert!(feed.items[0].enclosure_url.is_empty());
    }

    #[test]
    fn guid_is_deterministic_across_repeated_parses() {
        // 关键回归：guid 绝不能随每次抓取变化（否则每轮都当成全新条目重复下载）。
        let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><title>no link no enclosure</title></item></channel></rss>"#;
        let a = parse_feed(xml.as_bytes()).unwrap();
        let b = parse_feed(xml.as_bytes()).unwrap();
        assert_eq!(a.items[0].guid, b.items[0].guid);
        assert!(!a.items[0].guid.is_empty());
    }

    #[test]
    fn enclosure_mime_is_extracted_lowercased_without_parameters() {
        // RSS 2.0 <enclosure>（PT 站形态：URL 不带 .torrent）、Atom rel=enclosure 链接、
        // 以及无 enclosure 时回退链接上声明的类型。
        let rss = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
<item><guid>a</guid><title>pt</title><link>https://pt.test/details?id=1</link>
<enclosure url="https://pt.test/download.php?id=1" length="1" type="Application/X-BitTorrent; charset=utf-8"/></item>
<item><guid>b</guid><title>plain</title><link>https://x.test/page</link>
<enclosure url="https://x.test/a.mp4" length="1" type="video/mp4"/></item>
<item><guid>c</guid><title>none</title><link>https://x.test/none</link></item>
</channel></rss>"#;
        let feed = parse_feed(rss.as_bytes()).unwrap();
        assert_eq!(feed.items[0].enclosure_type, "application/x-bittorrent");
        assert_eq!(feed.items[1].enclosure_type, "video/mp4");
        assert_eq!(feed.items[2].enclosure_type, "");

        let atom = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>t</title><id>urn:x</id>
<entry><id>urn:e1</id><title>e1</title>
<link rel="alternate" href="https://pt.test/details/1"/>
<link rel="enclosure" href="https://pt.test/dl/1" length="10" type="application/x-bittorrent"/>
</entry>
<entry><id>urn:e2</id><title>e2</title>
<link href="https://pt.test/dl/2" type="application/x-bittorrent"/>
</entry></feed>"#;
        let feed = parse_feed(atom.as_bytes()).unwrap();
        assert_eq!(feed.items[0].enclosure_type, "application/x-bittorrent");
        assert_eq!(feed.items[1].enclosure_type, "application/x-bittorrent");
        assert!(feed.items[1].enclosure_url.is_empty());
    }

    #[test]
    fn malformed_input_returns_error_instead_of_panicking() {
        assert!(parse_feed(b"").is_err());
        assert!(parse_feed(b"<html><body>login required</body></html>").is_err());
        assert!(parse_feed(b"\xff\xfe\x00garbage").is_err());
        assert!(parse_feed(&vec![b'x'; MAX_FEED_BYTES + 1]).is_err());
    }

    #[test]
    fn loose_datetime_covers_the_shapes_feeds_actually_emit() {
        // RFC 2822（RSS 2.0 标准写法）与 RFC 3339 都带时区，结果是绝对时刻。
        assert_eq!(
            parse_loose_datetime("Fri, 10 Jul 2026 18:33:00 GMT"),
            1_783_708_380
        );
        assert_eq!(parse_loose_datetime("2026-07-10T18:33:00Z"), 1_783_708_380);
        // Mikan 的小数秒无时区形式：只要求能解析出来（值随本地时区而定）。
        assert!(parse_loose_datetime("2025-06-22T01:30:54.145714") > 0);
        assert!(parse_loose_datetime("2025-06-22") > 0);
        // 解析不出来一律 0，绝不 panic、绝不瞎猜。
        assert_eq!(parse_loose_datetime(""), 0);
        assert_eq!(parse_loose_datetime("  "), 0);
        assert_eq!(parse_loose_datetime("昨天"), 0);
    }
}
