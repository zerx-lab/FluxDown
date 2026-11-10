//! 已完成任务的文件哈希：`algo=hex` 解析、展示名，以及与用户校验和的比对。
//!
//! 展示名是技术名（MD5 / SHA-1 / SHA-256 / SHA-512），不进 i18n。设置页有一份相同的
//! 四值映射：设置 crate 不依赖本 crate。

use fluxdown_protocol::HASH_ALGORITHMS;

/// 尚未计算、或当前记录的算法无法识别时，算法选择的缺省值。
pub(crate) const DEFAULT_HASH_ALGORITHM: &str = "sha256";

/// 算法展示名。已知算法（[`HASH_ALGORITHMS`]）给出规范名；其余把输入原样大写。
#[must_use]
pub(crate) fn hash_algorithm_label(algo: &str) -> String {
    match algo {
        "md5" => "MD5".to_owned(),
        "sha1" => "SHA-1".to_owned(),
        "sha256" => "SHA-256".to_owned(),
        "sha512" => "SHA-512".to_owned(),
        other => other.to_ascii_uppercase(),
    }
}

/// `file_hash` 里的算法（已规范成 [`HASH_ALGORITHMS`] 之一）。空或无法解析时为 `None`。
#[must_use]
pub(crate) fn file_hash_algorithm(file_hash: &str) -> Option<&'static str> {
    split_hash(file_hash).map(|(algo, _)| algo)
}

/// 落盘哈希是否与用户填写的校验和一致。
///
/// 两边都按 `algo=hex` 解析：算法去空白、小写、去掉 `-`，且必须是
/// [`HASH_ALGORITHMS`] 之一。校验和的 hex 若带与该算法相同的 `<algo>:` 前缀则剥掉。
/// hex 去外层空白后忽略大小写比较。任一侧为空 / 无法解析，或算法不同，返回 `None`。
#[must_use]
pub(crate) fn file_hash_matches_checksum(file_hash: &str, checksum: &str) -> Option<bool> {
    let (file_algo, file_hex) = split_hash(file_hash)?;
    let (checksum_algo, checksum_hex) = split_hash(checksum)?;
    if file_algo != checksum_algo {
        return None;
    }
    let checksum_hex = strip_checksum_prefix(checksum_hex, checksum_algo);
    if file_hex.is_empty() || checksum_hex.is_empty() {
        return None;
    }
    Some(file_hex.eq_ignore_ascii_case(checksum_hex))
}

fn split_hash(spec: &str) -> Option<(&'static str, &str)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (algo, hex) = spec.split_once('=')?;
    let algo = canonical_algorithm(algo)?;
    let hex = hex.trim();
    if hex.is_empty() {
        return None;
    }
    Some((algo, hex))
}

/// 校验和 hex 上的冗余 `<algo>:` 前缀：只在前缀规范成与左侧算法相同的名字时剥掉。
fn strip_checksum_prefix<'a>(hex: &'a str, algo: &str) -> &'a str {
    let Some((prefix, rest)) = hex.split_once(':') else {
        return hex;
    };
    if canonical_algorithm(prefix).is_some_and(|prefix| prefix == algo) {
        rest.trim()
    } else {
        hex
    }
}

fn canonical_algorithm(algo: &str) -> Option<&'static str> {
    let mut normalized = String::new();
    for ch in algo.trim().chars() {
        if ch != '-' {
            normalized.extend(ch.to_lowercase());
        }
    }
    HASH_ALGORITHMS
        .iter()
        .copied()
        .find(|known| *known == normalized)
}

#[cfg(test)]
mod tests {
    use super::{file_hash_matches_checksum, hash_algorithm_label};

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const HEX_UPPER: &str = "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    const MD5: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn shared_cases() {
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), &format!("SHA-256={HEX_UPPER}")),
            Some(true)
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), &format!("sha256=sha256:{HEX}")),
            Some(true)
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), &format!("sha256={OTHER}")),
            Some(false)
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), &format!("md5={MD5}")),
            None
        );
        assert_eq!(
            file_hash_matches_checksum("", &format!("sha256={HEX}")),
            None
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), ""),
            None
        );
    }

    #[test]
    fn checksum_prefix_is_stripped_only_when_it_names_the_same_algorithm() {
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), &format!("sha256=md5:{HEX}")),
            Some(false)
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha1={MD5}"), &format!("SHA-1=sha-1:{MD5}")),
            Some(true)
        );
        assert_eq!(
            file_hash_matches_checksum(&format!("sha256={HEX}"), "sha256=sha256:"),
            None
        );
    }

    #[test]
    fn labels_known_algorithms_and_uppercases_the_rest() {
        assert_eq!(hash_algorithm_label("md5"), "MD5");
        assert_eq!(hash_algorithm_label("sha1"), "SHA-1");
        assert_eq!(hash_algorithm_label("sha256"), "SHA-256");
        assert_eq!(hash_algorithm_label("sha512"), "SHA-512");
        assert_eq!(hash_algorithm_label("blake3"), "BLAKE3");
    }
}
