//! `ed2k://` 链接解析 —— 纯字符串处理，零 I/O、零网络。
//!
//! 只处理 file 基本形态 `ed2k://|file|<name>|<size>|<md4 32hex>|/`，
//! 容忍并忽略其后的扩展段（`|h=AICH|`、`|s=source|`、`|p=|` 等）。
//!
//! **不使用 [`url::Url::parse`]**：`|` 是 WHATWG opaque-host 禁止字符，
//! ed2k 链接会解析失败或产生垃圾 host。按 `|` 手工分段是唯一正确做法。

use crate::downloader::{DownloadError, decode_bytes_utf8_or_gbk, sanitize_filename};

/// 一条已解析的 `ed2k://|file|...` 链接的核心字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed2kLink {
    /// 经 percent-decode + GBK 回退 + 文件名净化后的展示文件名。
    pub file_name: String,
    /// 文件总字节数。
    pub total_bytes: u64,
    /// 16 字节 eD2K root hash（文件标识）。
    pub root_hash: [u8; 16],
}

/// URL 是否为 `ed2k://` 链接（前缀大小写不敏感）。
///
/// 镜像 `download_manager::is_ftp_url` 的写法，仅判前缀，不做完整解析。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::link::is_ed2k_url;
/// assert!(is_ed2k_url("ed2k://|file|a|1|00000000000000000000000000000000|/"));
/// assert!(is_ed2k_url("ED2K://|file|a|1|00000000000000000000000000000000|/"));
/// assert!(!is_ed2k_url("http://example.com/a"));
/// ```
#[must_use]
pub fn is_ed2k_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("ed2k://")
}

/// 单个十六进制字符（ASCII）转 0..=15 的 nibble，非法字节返回 `None`。
///
/// 按字节解析避免对 `&str` 切片，消除 `%` 后紧跟多字节 UTF-8 时的
/// char-boundary panic（与 `ftp_downloader::hex_nibble` 同惯例）。
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// percent-decode 到原始字节序列（`%XX`→字节；非法转义原样保留 `%`）。
fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// 解析 `ed2k://|file|<name>|<size>|<md4 32hex>|/`。
///
/// - 前缀 `ed2k://|file|` 大小写不敏感。
/// - `name` 经 percent-decode → UTF-8/GBK 解码 → [`sanitize_filename`]。
/// - `size` 解析为 [`u64`]（非数字/溢出 → `Err`）。
/// - `hash` 必须恰为 32 位十六进制（大小写不敏感）→ 16 字节。
/// - `name`/`size`/`hash` 之后的扩展段一律忽略，不报错。
///
/// 此函数**幂等且是 name/size/hash 提取的唯一权威实现** —— 建任务阶段
/// （`create_task`）与下载编排（`run_ed2k_download`）都调用它，保证两处解析
/// 结果一致。
///
/// # Errors
///
/// 前缀不符 / 字段缺失 / size 非法 / hash 非 32 hex 时返回
/// [`DownloadError::Ed2k`]。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::link::parse_ed2k_link;
/// let link = parse_ed2k_link(
///     "ed2k://|file|movie.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/",
/// )
/// .unwrap();
/// assert_eq!(link.file_name, "movie.iso");
/// assert_eq!(link.total_bytes, 5_044_211_712);
/// assert_eq!(link.root_hash[0], 0x15);
/// ```
pub fn parse_ed2k_link(url: &str) -> Result<Ed2kLink, DownloadError> {
    let trimmed = url.trim();
    // 前缀大小写不敏感校验（保留原串以便切分保留原始大小写的字段）。
    let lower = trimmed.to_ascii_lowercase();
    let prefix = "ed2k://|file|";
    if !lower.starts_with(prefix) {
        return Err(DownloadError::Ed2k(format!("not an ed2k file link: {url}")));
    }
    let rest = &trimmed[prefix.len()..];
    // 按 `|` 分段：[name, size, hash, <扩展段...>]。
    let parts: Vec<&str> = rest.split('|').collect();
    if parts.len() < 3 {
        return Err(DownloadError::Ed2k(format!(
            "ed2k link missing name/size/hash fields: {url}"
        )));
    }

    let raw_name = parts[0];
    if raw_name.is_empty() {
        return Err(DownloadError::Ed2k("ed2k link has empty file name".into()));
    }
    let decoded_bytes = percent_decode_bytes(raw_name);
    let name_str = decode_bytes_utf8_or_gbk(&decoded_bytes)
        .map_err(|e| DownloadError::Ed2k(format!("ed2k file name decode failed: {e}")))?;
    let file_name = sanitize_filename(&name_str);
    if file_name.is_empty() {
        return Err(DownloadError::Ed2k(
            "ed2k file name empty after sanitize".into(),
        ));
    }

    let total_bytes: u64 = parts[1]
        .parse()
        .map_err(|_| DownloadError::Ed2k(format!("ed2k link invalid size: {}", parts[1])))?;

    let hash_hex = parts[2];
    if hash_hex.len() != 32 {
        return Err(DownloadError::Ed2k(format!(
            "ed2k link hash must be 32 hex chars, got {}",
            hash_hex.len()
        )));
    }
    let mut root_hash = [0u8; 16];
    let hb = hash_hex.as_bytes();
    for (i, out) in root_hash.iter_mut().enumerate() {
        let hi = hex_nibble(hb[i * 2])
            .ok_or_else(|| DownloadError::Ed2k(format!("ed2k link hash not hex: {hash_hex}")))?;
        let lo = hex_nibble(hb[i * 2 + 1])
            .ok_or_else(|| DownloadError::Ed2k(format!("ed2k link hash not hex: {hash_hex}")))?;
        *out = (hi << 4) | lo;
    }

    Ok(Ed2kLink {
        file_name,
        total_bytes,
        root_hash,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{is_ed2k_url, parse_ed2k_link};

    const VALID: &str = "ed2k://|file|movie.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/";

    #[test]
    fn parses_valid_link() {
        let link = parse_ed2k_link(VALID).unwrap();
        assert_eq!(link.file_name, "movie.iso");
        assert_eq!(link.total_bytes, 5_044_211_712);
        assert_eq!(
            link.root_hash,
            [
                0x15, 0x55, 0xB7, 0xDC, 0xA0, 0x52, 0xB5, 0x95, 0x8E, 0xE6, 0x8D, 0xB5, 0x8A, 0x42,
                0x40, 0x8D,
            ]
        );
    }

    #[test]
    fn idempotent() {
        let a = parse_ed2k_link(VALID).unwrap();
        let b = parse_ed2k_link(VALID).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn case_insensitive_prefix() {
        assert!(parse_ed2k_link(&VALID.replace("ed2k", "ED2K")).is_ok());
    }

    #[test]
    fn ignores_extension_segments() {
        let with_ext =
            "ed2k://|file|a.bin|100|00000000000000000000000000000000|h=ABCD|s=http://x/y|/";
        assert!(parse_ed2k_link(with_ext).is_ok());
    }

    #[test]
    fn rejects_wrong_prefix() {
        assert!(parse_ed2k_link("http://example.com/a").is_err());
        assert!(parse_ed2k_link("ed2k://|server|1.2.3.4|4661|/").is_err());
    }

    #[test]
    fn rejects_missing_fields() {
        assert!(parse_ed2k_link("ed2k://|file|only_name|/").is_err());
        assert!(parse_ed2k_link("ed2k://|file|name|100|/").is_err()); // hash 字段是 "/" 非 32hex
    }

    #[test]
    fn rejects_bad_hash_length() {
        // 31 位
        assert!(parse_ed2k_link("ed2k://|file|a|1|0000000000000000000000000000000|/").is_err());
        // 33 位
        assert!(parse_ed2k_link("ed2k://|file|a|1|000000000000000000000000000000000|/").is_err());
    }

    #[test]
    fn rejects_non_hex_hash() {
        assert!(parse_ed2k_link("ed2k://|file|a|1|zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz|/").is_err());
    }

    #[test]
    fn rejects_bad_size() {
        assert!(
            parse_ed2k_link("ed2k://|file|a|notanumber|00000000000000000000000000000000|/")
                .is_err()
        );
        // u64 溢出
        assert!(
            parse_ed2k_link(
                "ed2k://|file|a|99999999999999999999999|00000000000000000000000000000000|/"
            )
            .is_err()
        );
    }

    #[test]
    fn size_zero_ok() {
        let link =
            parse_ed2k_link("ed2k://|file|empty|0|00000000000000000000000000000000|/").unwrap();
        assert_eq!(link.total_bytes, 0);
    }

    #[test]
    fn percent_decoded_name() {
        let link =
            parse_ed2k_link("ed2k://|file|a%20b.txt|1|00000000000000000000000000000000|/").unwrap();
        assert_eq!(link.file_name, "a b.txt");
    }

    #[test]
    fn sanitizes_illegal_chars() {
        // 含 Windows 非法字符 : ? * 被 sanitize_filename 处理（不 panic，产出非空名）。
        let link =
            parse_ed2k_link("ed2k://|file|a%3Ab%3Fc.txt|1|00000000000000000000000000000000|/")
                .unwrap();
        assert!(!link.file_name.is_empty());
        assert!(!link.file_name.contains(':'));
        assert!(!link.file_name.contains('?'));
    }

    #[test]
    fn is_ed2k_url_prefix() {
        assert!(is_ed2k_url("ed2k://|file|a|1|x|/"));
        assert!(is_ed2k_url("ED2K://anything"));
        assert!(!is_ed2k_url("http://example.com"));
        assert!(!is_ed2k_url("magnet:?xt=urn:btih:abc"));
    }
}
