//! 设备互联加密原语：指纹、SAS 短认证串、链路密钥派生、数据面 HMAC 鉴权。
//!
//! 全部基于已在引擎中的 `sha2 0.10`（digest 0.10）+ `hkdf 0.12` + `hmac 0.12`，
//! 三者 digest 版本一致，避免类型 trait bound 冲突。

use hkdf::Hkdf;
use hmac::{Mac, SimpleHmac};
use sha2::{Digest, Sha256};

/// SAS 短认证串位数（6 位数字，双端肉眼核对）。
const SAS_DIGITS: u32 = 6;
const SAS_MODULO: u32 = 1_000_000; // 10^SAS_DIGITS

/// 数据面链路鉴权时间戳容忍窗口（秒）——防重放，两端时钟偏移容错。
pub const LINK_AUTH_SKEW_SECS: i64 = 120;

/// 计算 Ed25519（或任意）公钥的展示指纹：`hex(sha256(pub))`（64 hex 小写）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::link::crypto::fingerprint;
/// let fp = fingerprint(&[0u8; 32]);
/// assert_eq!(fp.len(), 64);
/// ```
#[must_use]
pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    hex::encode(digest)
}

/// 从 X25519 ECDH 共享密钥 `z` + 双方临时公钥派生 6 位 SAS。
///
/// 两端公钥排序后拼接（顺序无关），确保 initiator 与 responder 计算出**相同** SAS。
/// 中间人会与两端各自建立不同的 `z` → 两端 SAS 不一致 → 用户肉眼核对即可发现。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::link::crypto::derive_sas;
/// let z = [7u8; 32];
/// let a = [1u8; 32];
/// let b = [2u8; 32];
/// // 顺序无关：交换 a/b 得到同一 SAS。
/// assert_eq!(derive_sas(&z, &a, &b), derive_sas(&z, &b, &a));
/// assert_eq!(derive_sas(&z, &a, &b).len(), 6);
/// ```
#[must_use]
pub fn derive_sas(z: &[u8], pub_a: &[u8; 32], pub_b: &[u8; 32]) -> String {
    let (lo, hi) = if pub_a <= pub_b {
        (pub_a, pub_b)
    } else {
        (pub_b, pub_a)
    };
    let mut info = Vec::with_capacity(64);
    info.extend_from_slice(lo);
    info.extend_from_slice(hi);

    let hk = Hkdf::<Sha256>::new(Some(b"fluxdown-link-sas-v1"), z);
    let mut okm = [0u8; 4];
    // 长度固定 4 字节 << 255*32，expand 不会失败；仍显式处理错误不 unwrap。
    if hk.expand(&info, &mut okm).is_err() {
        return "000000".to_string();
    }
    let n = u32::from_be_bytes(okm) % SAS_MODULO;
    format!("{n:0width$}", width = SAS_DIGITS as usize)
}

/// 从 ECDH 共享密钥派生**每对设备独立**的 32 字节链路密钥（数据面 HMAC 用）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::link::crypto::derive_link_key;
/// let k = derive_link_key(&[9u8; 32]);
/// assert_eq!(k.len(), 32);
/// ```
#[must_use]
pub fn derive_link_key(z: &[u8]) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(Some(b"fluxdown-link-key-salt-v1"), z);
    let mut okm = [0u8; 32];
    if hk.expand(b"fluxdown-link-key-v1", &mut okm).is_err() {
        return z.to_vec();
    }
    okm.to_vec()
}

/// 数据面链路鉴权标签：`HMAC-SHA256(link_secret, method\npath\nts\nnonce\nSHA256(body))` 的 hex。
///
/// 密钥永不上网络；只发送 HMAC 标签 + 明文 method/path/ts/nonce + 请求体，对端用存储
/// 的同一 `link_secret` 重算比对（常量时间）。body 摘要纳入签名，防止 on-path 攻击者
/// 保留头部改写请求体。适配任意传输（Direct/未来 iroh/relay）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::link::crypto::{link_auth_tag, verify_link_auth_tag};
/// let key = [3u8; 32];
/// let body = b"{}";
/// let tag = link_auth_tag(&key, "POST", "/api/v1/link/tasks", 1000, "abc", body);
/// assert!(verify_link_auth_tag(&key, "POST", "/api/v1/link/tasks", 1000, "abc", body, &tag));
/// assert!(!verify_link_auth_tag(&key, "GET", "/api/v1/link/tasks", 1000, "abc", body, &tag));
/// ```
#[must_use]
pub fn link_auth_tag(
    secret: &[u8],
    method: &str,
    path: &str,
    ts: i64,
    nonce: &str,
    body: &[u8],
) -> String {
    // HMAC 接受任意长度密钥，new_from_slice 对本用法永不返回 InvalidLength；
    // 仍显式处理错误分支（clippy 禁 unwrap/expect）——极端情况下返回空标签，
    // 校验侧 ct_eq 因长度不符必然 false，安全。
    let Ok(mut mac) = <SimpleHmac<Sha256> as Mac>::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(method.as_bytes());
    mac.update(b"\n");
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(ts.to_string().as_bytes());
    mac.update(b"\n");
    mac.update(nonce.as_bytes());
    mac.update(b"\n");
    // body 摘要纳入签名，防止 on-path 攻击者保留头部改写请求体（url/saveDir）。
    mac.update(&Sha256::digest(body));
    hex::encode(mac.finalize().into_bytes())
}

/// 常量时间校验数据面链路鉴权标签。
#[must_use]
pub fn verify_link_auth_tag(
    secret: &[u8],
    method: &str,
    path: &str,
    ts: i64,
    nonce: &str,
    body: &[u8],
    tag_hex: &str,
) -> bool {
    let expected = link_auth_tag(secret, method, path, ts, nonce, body);
    // hex::encode 输出等长，用 ct 比较避免时序侧信道。
    ct_eq(expected.as_bytes(), tag_hex.as_bytes())
}

/// 常量时间字节比较（长度不等直接 false）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sas_is_order_independent_and_six_digits() {
        let z = [42u8; 32];
        let a = [1u8; 32];
        let b = [200u8; 32];
        let s1 = derive_sas(&z, &a, &b);
        let s2 = derive_sas(&z, &b, &a);
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 6);
        assert!(s1.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn different_shared_secret_yields_different_sas() {
        // 中间人场景：两端 z 不同 → SAS 不同（用户可发现）。
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert_ne!(
            derive_sas(&[7u8; 32], &a, &b),
            derive_sas(&[9u8; 32], &a, &b)
        );
    }

    #[test]
    fn link_key_deterministic_per_secret() {
        assert_eq!(derive_link_key(&[5u8; 32]), derive_link_key(&[5u8; 32]));
        assert_ne!(derive_link_key(&[5u8; 32]), derive_link_key(&[6u8; 32]));
    }

    #[test]
    fn auth_tag_roundtrip_and_tamper_detection() {
        let key = [3u8; 32];
        let body = br#"{"url":"http://x/f"}"#;
        let tag = link_auth_tag(&key, "POST", "/api/v1/link/tasks", 1000, "nonce1", body);
        assert!(verify_link_auth_tag(
            &key,
            "POST",
            "/api/v1/link/tasks",
            1000,
            "nonce1",
            body,
            &tag
        ));
        // 篡改任一字段都失败。
        assert!(!verify_link_auth_tag(
            &key,
            "PUT",
            "/api/v1/link/tasks",
            1000,
            "nonce1",
            body,
            &tag
        ));
        assert!(!verify_link_auth_tag(
            &key, "POST", "/x", 1000, "nonce1", body, &tag
        ));
        assert!(!verify_link_auth_tag(
            &key,
            "POST",
            "/api/v1/link/tasks",
            1001,
            "nonce1",
            body,
            &tag
        ));
        assert!(!verify_link_auth_tag(
            &key,
            "POST",
            "/api/v1/link/tasks",
            1000,
            "nonce2",
            body,
            &tag
        ));
        // 篡改请求体（换 URL）→ 失败（body 已纳入签名）。
        assert!(!verify_link_auth_tag(
            &key,
            "POST",
            "/api/v1/link/tasks",
            1000,
            "nonce1",
            br#"{"url":"http://evil/f"}"#,
            &tag
        ));
        // 换密钥失败。
        assert!(!verify_link_auth_tag(
            &[4u8; 32],
            "POST",
            "/api/v1/link/tasks",
            1000,
            "nonce1",
            body,
            &tag
        ));
    }

    #[test]
    fn fingerprint_is_64_hex() {
        let fp = fingerprint(&[0xabu8; 32]);
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
