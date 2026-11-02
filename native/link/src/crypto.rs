//! 设备互联加密原语：指纹、临时公钥承诺、SAS 短认证串、链路密钥派生、数据面 HMAC
//! 鉴权 + AEAD 加密。
//!
//! 全部基于已在引擎中的 `sha2 0.10`（digest 0.10）+ `hkdf 0.12` + `hmac 0.12`，
//! 三者 digest 版本一致，避免类型 trait bound 冲突；数据面 body 加密另加
//! `chacha20poly1305 0.10`（RustCrypto 同族，与前三者独立但风格一致）。
//!
//! # 数据面 v2
//! - MAC 绑定方法、路径、时间戳、nonce、**收发双方指纹**与密文摘要，防止把 A→B 的请求
//!   反射回 A（见 [`link_auth_tag`]）。
//! - 请求体与**响应体**都用 ChaCha20-Poly1305 封装，AAD 分别绑定方向、方法、路径、双方指纹
//!   与请求 nonce（见 [`request_aad`] / [`response_aad`]）：响应无法被旧响应 / 另一方向的
//!   密文替换，发起方只在响应成功解封后才相信「对端确实是那台已配对设备」。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Mac, SimpleHmac};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// SAS 短认证串位数（6 位数字，双端肉眼核对）。
const SAS_DIGITS: u32 = 6;
const SAS_MODULO: u32 = 1_000_000; // 10^SAS_DIGITS

/// 数据面链路鉴权时间戳容忍窗口（秒）——防重放，两端时钟偏移容错。
pub const LINK_AUTH_SKEW_SECS: i64 = 120;

/// 数据面链路协议版本：`/ping` 的 `linkProtocol`。发起方在下发 / 交换信息 / 解除配对前
/// 若对端版本低于它，直接 [`crate::LinkError::PeerOutdated`]，不发请求。
pub const LINK_PROTOCOL_VERSION: u32 = 2;

/// 设备控制配对能力版本：`/ping` 的 `linkControl`。发起方请求「完全控制」时若对端 `/ping` 没有
/// 达到它，直接放弃（[`crate::LinkError::UnsupportedVersion`]），不发 hello——旧版对端会忽略
/// 未知的 `requestControl` 字段，签名也对不上。
pub const LINK_CONTROL_VERSION: u32 = 1;

/// `X-FluxLink-Enc` 请求头 / 响应头的取值：数据面方案 v2。缺失或其它值（含 v1）一律拒绝，
/// 不留降级回退路径。
pub const LINK_ENC_V2: &str = "v2";

/// 计算 Ed25519（或任意）公钥的展示指纹：`hex(sha256(pub))`（64 hex 小写）。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::fingerprint;
/// let fp = fingerprint(&[0u8; 32]);
/// assert_eq!(fp.len(), 64);
/// ```
#[must_use]
pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    hex::encode(digest)
}

/// 计算发起方临时公钥 + 随机数的承诺：`sha256(域分隔串 || eph_pub || nonce)`。
///
/// 配对握手里发起方先发承诺、响应方回出自己的临时公钥之后发起方才揭示
/// `eph_pub`/`nonce`（响应方用 [`verify_eph_commitment`] 复核）。承诺把发起方的
/// 临时密钥在响应方亮出自己的值之前钉死——中间人无法在看到一端的值之后再为另一端
/// 挑选能凑出相同 SAS 的临时密钥。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::{eph_commitment, verify_eph_commitment};
/// let (eph, nonce) = ([1u8; 32], [2u8; 32]);
/// let c = eph_commitment(&eph, &nonce);
/// assert!(verify_eph_commitment(&c, &eph, &nonce));
/// assert!(!verify_eph_commitment(&c, &[9u8; 32], &nonce));
/// ```
#[must_use]
pub fn eph_commitment(eph_pub: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"fluxdown-link-commit-v2");
    hasher.update(eph_pub);
    hasher.update(nonce);
    hasher.finalize().into()
}

/// 常量时间校验承诺是否对应给定的临时公钥与随机数。
#[must_use]
pub fn verify_eph_commitment(commitment: &[u8; 32], eph_pub: &[u8; 32], nonce: &[u8; 32]) -> bool {
    ct_eq(commitment, &eph_commitment(eph_pub, nonce))
}

/// 配对转录摘要：对整段握手转录字节取 `sha256`。SAS 与链路密钥都以它作为
/// HKDF 的 info，转录里任意一个字段（身份公钥、临时公钥、随机数、配对码、自报信息）
/// 不同，两端派生出的 SAS 与密钥就不同。
#[must_use]
pub fn hash_transcript(transcript: &[u8]) -> [u8; 32] {
    Sha256::digest(transcript).into()
}

/// 从 X25519 ECDH 共享密钥 `z` + 完整握手转录摘要派生 6 位 SAS。
///
/// 转录按角色（发起方/响应方）有序，两端从各自持有的同一份转录得到同一摘要，
/// 因而算出**相同** SAS。中间人与两端各自建立的 `z` 与转录都不同 → 两端 SAS 不一致
/// → 用户肉眼核对即可发现；临时公钥承诺保证中间人无法事后凑出相同 SAS。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::derive_sas;
/// let z = [7u8; 32];
/// let transcript = [1u8; 32];
/// assert_eq!(derive_sas(&z, &transcript), derive_sas(&z, &transcript));
/// assert_ne!(derive_sas(&z, &transcript), derive_sas(&z, &[2u8; 32]));
/// assert_eq!(derive_sas(&z, &transcript).len(), 6);
/// ```
#[must_use]
pub fn derive_sas(z: &[u8], transcript_hash: &[u8; 32]) -> String {
    let hk = Hkdf::<Sha256>::new(Some(b"fluxdown-link-sas-v2"), z);
    let mut okm = [0u8; 4];
    // 长度固定 4 字节 << 255*32，expand 不会失败；仍显式处理错误不 unwrap。
    if hk.expand(transcript_hash, &mut okm).is_err() {
        return "000000".to_string();
    }
    let n = u32::from_be_bytes(okm) % SAS_MODULO;
    format!("{n:0width$}", width = SAS_DIGITS as usize)
}

/// 从 ECDH 共享密钥 + 完整握手转录摘要派生**每对设备独立**的 32 字节链路密钥
/// （数据面 HMAC 用）。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::derive_link_key;
/// let k = derive_link_key(&[9u8; 32], &[1u8; 32]);
/// assert_eq!(k.len(), 32);
/// assert_ne!(k, derive_link_key(&[9u8; 32], &[2u8; 32]));
/// ```
#[must_use]
pub fn derive_link_key(z: &[u8], transcript_hash: &[u8; 32]) -> Vec<u8> {
    let hk = Hkdf::<Sha256>::new(Some(b"fluxdown-link-key-salt-v2"), z);
    let mut info = Vec::with_capacity(20 + transcript_hash.len());
    info.extend_from_slice(b"fluxdown-link-key-v2");
    info.extend_from_slice(transcript_hash);
    let mut okm = [0u8; 32];
    if hk.expand(&info, &mut okm).is_err() {
        return z.to_vec();
    }
    okm.to_vec()
}

/// 从每对设备独立的 `link_secret` 派生数据面 AEAD 加密密钥。
///
/// **域分隔**：HKDF salt/info 标签与 [`derive_sas`]（`"fluxdown-link-sas-v2"`）、
/// [`derive_link_key`]（`"fluxdown-link-key-salt-v2"`/`"fluxdown-link-key-v2"`）
/// 均不同——AEAD 加密密钥绝不能等于 HMAC 鉴权用的 `link_secret` 本身或与
/// SAS 相关，否则一把密钥材料挪作多用途，任一用途的密码学分析结果都可能
/// 波及其余用途。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::derive_link_aead_key;
/// let k = derive_link_aead_key(&[6u8; 32]);
/// assert_eq!(k.len(), 32);
/// ```
#[must_use]
pub fn derive_link_aead_key(link_secret: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"fluxdown-link-aead-salt-v1"), link_secret);
    let mut okm = [0u8; 32];
    // 长度固定 32 字节 << 255*32，expand 不会失败；仍显式处理错误分支
    // （clippy 禁 unwrap/expect）。
    if hk.expand(b"fluxdown-link-aead-v1", &mut okm).is_err() {
        return [0u8; 32];
    }
    okm
}

/// 数据面一次往返的公共绑定量：MAC 与两个方向的 AEAD AAD 都由它派生。
///
/// `sender_fp` / `receiver_fp` 是**请求**的发送方 / 接收方指纹；响应方向的 AAD 把二者对调
/// （见 [`response_aad`]）。
#[derive(Debug, Clone, Copy)]
pub struct LinkExchange<'a> {
    /// HTTP 方法（大写）。
    pub method: &'a str,
    /// 请求路径（不含反代子路径与查询串），如 `/api/v1/link/tasks`。
    pub path: &'a str,
    /// 请求 nonce：既是防重放键，也把响应绑定到这一次请求。
    pub nonce: &'a str,
    /// 请求发送方指纹。
    pub sender_fp: &'a str,
    /// 请求接收方指纹。
    pub receiver_fp: &'a str,
}

/// 请求体 AEAD 的附加认证数据：
/// `"fluxlink-v2-req\n{METHOD}\n{path}\n{sender_fp}\n{receiver_fp}\n{nonce}"`。
#[must_use]
pub fn request_aad(exchange: &LinkExchange<'_>) -> Vec<u8> {
    format!(
        "fluxlink-v2-req\n{}\n{}\n{}\n{}\n{}",
        exchange.method, exchange.path, exchange.sender_fp, exchange.receiver_fp, exchange.nonce
    )
    .into_bytes()
}

/// 响应体 AEAD 的附加认证数据：
/// `"fluxlink-v2-resp\n{METHOD}\n{path}\n{responder_fp}\n{requester_fp}\n{请求 nonce}"`
/// （响应方 = 请求的接收方，请求方 = 请求的发送方）。
#[must_use]
pub fn response_aad(exchange: &LinkExchange<'_>) -> Vec<u8> {
    format!(
        "fluxlink-v2-resp\n{}\n{}\n{}\n{}\n{}",
        exchange.method, exchange.path, exchange.receiver_fp, exchange.sender_fp, exchange.nonce
    )
    .into_bytes()
}

/// 加密数据面消息体（ChaCha20-Poly1305）：随机 12 字节 nonce，`aad` 参与认证，返回
/// `nonce(12B) || ciphertext_with_tag`。
///
/// 每次调用生成独立随机 nonce——同一密钥绝不复用 nonce，否则 ChaCha20-
/// Poly1305 的机密性与完整性均被破坏。数据面请求量级远低于随机数生日
/// 碰撞有意义的阈值（2^32 条消息级别），随机 nonce 足够安全，不需要跨
/// 请求持久化计数器的额外状态同步复杂度。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::{open_link_body, seal_link_body};
/// let key = [1u8; 32];
/// let sealed = seal_link_body(&key, b"aad", b"hello");
/// assert_eq!(open_link_body(&key, b"aad", &sealed).as_deref(), Some(b"hello".as_slice()));
/// assert!(open_link_body(&key, b"other", &sealed).is_none());
/// ```
#[must_use]
pub fn seal_link_body(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let mut sealed = Vec::with_capacity(12 + plaintext.len() + 16);
    sealed.extend_from_slice(&nonce_bytes);
    // 对本用法（有界大小的下载任务 JSON，远小于 ChaCha20 计数器上限）加密
    // 实践中不会失败；仍显式处理错误分支（clippy 禁 unwrap/expect）——
    // 失败时只返回裸 nonce（无密文/tag），对端 `open_link_body` 因长度
    // 不足直接拒绝，不会误用无效密文。
    if let Ok(ciphertext) = cipher.encrypt(
        nonce,
        Payload {
            msg: plaintext,
            aad,
        },
    ) {
        sealed.extend_from_slice(&ciphertext);
    }
    sealed
}

/// 解密数据面消息体：拆出前 12 字节 nonce，用 `aad` 解密其余部分。长度不足（不足以
/// 容纳 nonce + 至少一个认证 tag）、AAD 不符或解密失败（tag 不匹配/密钥不对）均返回
/// `None`，调用方一律按鉴权失败处理，不区分具体原因——避免向攻击者泄露
/// 「是长度错还是密钥/篡改错」这类旁路信息。
///
/// # Examples
///
/// 见 [`seal_link_body`] 的往返示例。
#[must_use]
pub fn open_link_body(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 12 + 16 {
        return None;
    }
    let (nonce_bytes, ciphertext) = sealed.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .ok()
}

/// 数据面链路鉴权标签（v2）：对
/// `"fluxlink-v2\n{METHOD}\n{path}\n{ts}\n{nonce}\n{sender_fp}\n{receiver_fp}\n{hex(sha256(密文 body))}"`
/// 做 `HMAC-SHA256(link_secret, …)` 的 hex。
///
/// 密钥永不上网络；只发送 HMAC 标签 + 明文 method/path/ts/nonce + 请求体，对端用存储
/// 的同一 `link_secret` 重算比对（常量时间）。body 摘要纳入签名，防止 on-path 攻击者
/// 保留头部改写请求体；**收发双方指纹**纳入签名，两个方向共用同一 `link_secret` 也无法把
/// 一方发出的请求原样反射给自己。
///
/// # Examples
///
/// ```
/// use fluxdown_link::crypto::{LinkExchange, link_auth_tag, verify_link_auth_tag};
/// let key = [3u8; 32];
/// let exchange = LinkExchange {
///     method: "POST",
///     path: "/api/v1/link/tasks",
///     nonce: "abc",
///     sender_fp: "aa",
///     receiver_fp: "bb",
/// };
/// let tag = link_auth_tag(&key, &exchange, 1000, b"{}");
/// assert!(verify_link_auth_tag(&key, &exchange, 1000, b"{}", &tag));
/// // 把收发双方对调（反射）→ 校验失败。
/// let reflected = LinkExchange { sender_fp: "bb", receiver_fp: "aa", ..exchange };
/// assert!(!verify_link_auth_tag(&key, &reflected, 1000, b"{}", &tag));
/// ```
#[must_use]
pub fn link_auth_tag(secret: &[u8], exchange: &LinkExchange<'_>, ts: i64, body: &[u8]) -> String {
    // HMAC 接受任意长度密钥，new_from_slice 对本用法永不返回 InvalidLength；
    // 仍显式处理错误分支（clippy 禁 unwrap/expect）——极端情况下返回空标签，
    // 校验侧 ct_eq 因长度不符必然 false，安全。
    let Ok(mut mac) = <SimpleHmac<Sha256> as Mac>::new_from_slice(secret) else {
        return String::new();
    };
    let message = format!(
        "fluxlink-v2\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        exchange.method,
        exchange.path,
        ts,
        exchange.nonce,
        exchange.sender_fp,
        exchange.receiver_fp,
        hex::encode(Sha256::digest(body)),
    );
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// 常量时间校验数据面链路鉴权标签。
#[must_use]
pub fn verify_link_auth_tag(
    secret: &[u8],
    exchange: &LinkExchange<'_>,
    ts: i64,
    body: &[u8],
    tag_hex: &str,
) -> bool {
    let expected = link_auth_tag(secret, exchange, ts, body);
    // hex::encode 输出等长，用 ct 比较避免时序侧信道。
    ct_eq(expected.as_bytes(), tag_hex.as_bytes())
}

/// 设备控制凭据（`/rpc` 的 `Authorization: Bearer`）的格式前缀。
pub const CONTROL_CREDENTIAL_PREFIX: &str = "fdl1";

/// 设备控制凭据 HMAC 的域分隔标签。
const CONTROL_CREDENTIAL_DOMAIN: &str = "fluxdown-rpc-control-v1";

/// 指纹形态：64 位小写 hex。
#[must_use]
pub fn is_fingerprint_shaped(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 控制凭据的 HMAC：`HMAC-SHA256(link_secret, "fluxdown-rpc-control-v1\n{发起方指纹}\n{响应方指纹}")`。
fn control_credential_mac(
    link_secret: &[u8],
    initiator_fp: &str,
    responder_fp: &str,
) -> Option<[u8; 32]> {
    let mut mac = <SimpleHmac<Sha256> as Mac>::new_from_slice(link_secret).ok()?;
    mac.update(CONTROL_CREDENTIAL_DOMAIN.as_bytes());
    mac.update(b"\n");
    mac.update(initiator_fp.as_bytes());
    mac.update(b"\n");
    mac.update(responder_fp.as_bytes());
    Some(mac.finalize().into_bytes().into())
}

/// 签发设备控制凭据：`fdl1.<发起方指纹>.<base64url 无 padding(HMAC)>`。
///
/// 凭据由发起方（控制端）用与响应方（被控主机）共享的每对设备 `link_secret` 派生，**绑定双方
/// 指纹**；响应方无需额外存储，按指纹找到名册记录后重算即可校验。链路密钥永不外泄，任何持有
/// 凭据的人都只能用它连接那一台主机的 `/rpc`。`link_secret` 不可用（空）时返回 `None`。
#[must_use]
pub fn control_credential(
    link_secret: &[u8],
    initiator_fp: &str,
    responder_fp: &str,
) -> Option<String> {
    if link_secret.is_empty() {
        return None;
    }
    let mac = control_credential_mac(link_secret, initiator_fp, responder_fp)?;
    Some(format!(
        "{CONTROL_CREDENTIAL_PREFIX}.{initiator_fp}.{}",
        URL_SAFE_NO_PAD.encode(mac)
    ))
}

/// 解析设备控制凭据，返回其中声明的发起方指纹（形态已校验）与 HMAC 字节。前缀不符、段数不对、
/// 指纹形态非法或 HMAC 不是 32 字节的 base64url 一律 `None`。**不**校验 HMAC 本身。
#[must_use]
pub fn parse_control_credential(token: &str) -> Option<(&str, [u8; 32])> {
    let mut parts = token.split('.');
    let prefix = parts.next()?;
    let fingerprint = parts.next()?;
    let encoded = parts.next()?;
    if parts.next().is_some()
        || prefix != CONTROL_CREDENTIAL_PREFIX
        || !is_fingerprint_shaped(fingerprint)
    {
        return None;
    }
    let mac = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let mac = <[u8; 32]>::try_from(mac.as_slice()).ok()?;
    Some((fingerprint, mac))
}

/// 常量时间校验设备控制凭据：解析出的发起方指纹必须等于 `initiator_fp`，且 HMAC 与用
/// `link_secret` + 双方指纹重算的一致。`link_secret` 为空一律 `false`。
#[must_use]
pub fn verify_control_credential(
    link_secret: &[u8],
    initiator_fp: &str,
    responder_fp: &str,
    token: &str,
) -> bool {
    if link_secret.is_empty() {
        return false;
    }
    let Some((claimed, presented)) = parse_control_credential(token) else {
        return false;
    };
    if claimed != initiator_fp {
        return false;
    }
    control_credential_mac(link_secret, initiator_fp, responder_fp)
        .is_some_and(|expected| ct_eq(&expected, &presented))
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
    fn sas_is_six_digits_and_deterministic() {
        let z = [42u8; 32];
        let transcript = [1u8; 32];
        let s1 = derive_sas(&z, &transcript);
        assert_eq!(s1, derive_sas(&z, &transcript));
        assert_eq!(s1.len(), 6);
        assert!(s1.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn sas_depends_on_shared_secret_and_transcript() {
        // 中间人场景：两端 z 或转录不同 → SAS 不同（用户可发现）。
        let t = [1u8; 32];
        assert_ne!(derive_sas(&[7u8; 32], &t), derive_sas(&[9u8; 32], &t));
        assert_ne!(
            derive_sas(&[7u8; 32], &[1u8; 32]),
            derive_sas(&[7u8; 32], &[2u8; 32])
        );
    }

    #[test]
    fn link_key_depends_on_shared_secret_and_transcript() {
        let t = [3u8; 32];
        assert_eq!(
            derive_link_key(&[5u8; 32], &t),
            derive_link_key(&[5u8; 32], &t)
        );
        assert_ne!(
            derive_link_key(&[5u8; 32], &t),
            derive_link_key(&[6u8; 32], &t)
        );
        assert_ne!(
            derive_link_key(&[5u8; 32], &[3u8; 32]),
            derive_link_key(&[5u8; 32], &[4u8; 32])
        );
    }

    #[test]
    fn commitment_binds_both_ephemeral_key_and_nonce() {
        let (eph, nonce) = ([1u8; 32], [2u8; 32]);
        let commitment = eph_commitment(&eph, &nonce);
        assert!(verify_eph_commitment(&commitment, &eph, &nonce));
        assert!(!verify_eph_commitment(&commitment, &[9u8; 32], &nonce));
        assert!(!verify_eph_commitment(&commitment, &eph, &[9u8; 32]));
        let mut flipped = commitment;
        flipped[31] ^= 1;
        assert!(!verify_eph_commitment(&flipped, &eph, &nonce));
    }

    fn exchange() -> LinkExchange<'static> {
        LinkExchange {
            method: "POST",
            path: "/api/v1/link/tasks",
            nonce: "nonce1",
            sender_fp: "sender",
            receiver_fp: "receiver",
        }
    }

    #[test]
    fn auth_tag_roundtrip_and_tamper_detection() {
        let key = [3u8; 32];
        let body = br#"{"url":"http://x/f"}"#;
        let ex = exchange();
        let tag = link_auth_tag(&key, &ex, 1000, body);
        assert!(verify_link_auth_tag(&key, &ex, 1000, body, &tag));
        // 篡改任一字段都失败。
        for tampered in [
            LinkExchange {
                method: "PUT",
                ..ex
            },
            LinkExchange { path: "/x", ..ex },
            LinkExchange {
                nonce: "nonce2",
                ..ex
            },
            LinkExchange {
                sender_fp: "other",
                ..ex
            },
            LinkExchange {
                receiver_fp: "other",
                ..ex
            },
        ] {
            assert!(!verify_link_auth_tag(&key, &tampered, 1000, body, &tag));
        }
        assert!(!verify_link_auth_tag(&key, &ex, 1001, body, &tag));
        // 篡改请求体（换 URL）→ 失败（body 已纳入签名）。
        assert!(!verify_link_auth_tag(
            &key,
            &ex,
            1000,
            br#"{"url":"http://evil/f"}"#,
            &tag
        ));
        // 换密钥失败。
        assert!(!verify_link_auth_tag(&[4u8; 32], &ex, 1000, body, &tag));
    }

    #[test]
    fn auth_tag_binds_direction_so_a_reflected_request_fails() {
        // A→B 的请求被嗅探后原样发回 A（Device 头改成 B）：收发双方对调，MAC 必须失败。
        let key = [3u8; 32];
        let a_to_b = LinkExchange {
            sender_fp: "A",
            receiver_fp: "B",
            ..exchange()
        };
        let reflected = LinkExchange {
            sender_fp: "B",
            receiver_fp: "A",
            ..exchange()
        };
        let tag = link_auth_tag(&key, &a_to_b, 1000, b"body");
        assert!(verify_link_auth_tag(&key, &a_to_b, 1000, b"body", &tag));
        assert!(!verify_link_auth_tag(&key, &reflected, 1000, b"body", &tag));
    }

    #[test]
    fn auth_tag_follows_the_documented_message_layout() {
        // 线格式是跨端契约：HMAC 的输入必须逐字等于文档里的字符串。
        let key = [5u8; 32];
        let ex = exchange();
        let body = b"ciphertext";
        let message = format!(
            "fluxlink-v2\nPOST\n/api/v1/link/tasks\n1000\nnonce1\nsender\nreceiver\n{}",
            hex::encode(Sha256::digest(body))
        );
        let mut mac = <SimpleHmac<Sha256> as Mac>::new_from_slice(&key).unwrap();
        mac.update(message.as_bytes());
        assert_eq!(
            link_auth_tag(&key, &ex, 1000, body),
            hex::encode(mac.finalize().into_bytes())
        );
    }

    #[test]
    fn request_and_response_aad_differ_and_follow_the_documented_layout() {
        let ex = exchange();
        assert_eq!(
            request_aad(&ex),
            b"fluxlink-v2-req\nPOST\n/api/v1/link/tasks\nsender\nreceiver\nnonce1".to_vec()
        );
        // 响应方向：响应方（原接收方）在前，请求方在后。
        assert_eq!(
            response_aad(&ex),
            b"fluxlink-v2-resp\nPOST\n/api/v1/link/tasks\nreceiver\nsender\nnonce1".to_vec()
        );
    }

    #[test]
    fn fingerprint_is_64_hex() {
        let fp = fingerprint(&[0xabu8; 32]);
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn seal_open_roundtrip() {
        let key = [9u8; 32];
        let aad = b"aad";
        let plaintext = br#"{"url":"http://x/f","saveDir":"","fileName":""}"#;
        let sealed = seal_link_body(&key, aad, plaintext);
        // nonce(12) + 明文 + tag(16) 的开销下限。
        assert_eq!(sealed.len(), plaintext.len() + 12 + 16);
        let opened = open_link_body(&key, aad, &sealed).expect("decrypt should succeed");
        assert_eq!(opened, plaintext);
        // 每次调用随机 nonce：同一明文两次密封结果不同。
        let sealed2 = seal_link_body(&key, aad, plaintext);
        assert_ne!(sealed, sealed2);
    }

    #[test]
    fn open_rejects_a_different_aad() {
        // 请求 AAD 封装的密文不能当响应解开（方向 / nonce / 指纹任一不同都失败）。
        let key = [9u8; 32];
        let ex = exchange();
        let sealed = seal_link_body(&key, &request_aad(&ex), b"hello");
        assert!(open_link_body(&key, &request_aad(&ex), &sealed).is_some());
        assert!(open_link_body(&key, &response_aad(&ex), &sealed).is_none());
        let other_nonce = LinkExchange {
            nonce: "nonce2",
            ..ex
        };
        assert!(open_link_body(&key, &request_aad(&other_nonce), &sealed).is_none());
    }

    #[test]
    fn open_rejects_tampered_ciphertext() {
        let key = [9u8; 32];
        let plaintext = b"hello link";
        let mut sealed = seal_link_body(&key, b"aad", plaintext);
        // 翻转密文最后一字节（落在 Poly1305 tag 内）。
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(open_link_body(&key, b"aad", &sealed).is_none());
    }

    #[test]
    fn open_rejects_wrong_key() {
        let key = [9u8; 32];
        let other = [8u8; 32];
        let plaintext = b"hello link";
        let sealed = seal_link_body(&key, b"aad", plaintext);
        assert!(open_link_body(&other, b"aad", &sealed).is_none());
        // 长度不足（只剩 nonce、没有 tag）也必须拒绝。
        assert!(open_link_body(&key, b"aad", &sealed[..12]).is_none());
    }

    #[test]
    fn aead_key_differs_from_sas_and_link_key() {
        // 用同一份 32 字节输入喂给三个派生函数：只要 HKDF salt/info 标签不同，
        // 输出就必然不同。纯回归测试——防止未来有人手滑复制了已有标签，导致
        // AEAD 密钥能被 SAS / 握手期链路密钥反推，破坏域分隔。
        let secret = [77u8; 32];
        let transcript = [1u8; 32];

        let aead_key = derive_link_aead_key(&secret);
        let link_key = derive_link_key(&secret, &transcript);
        let sas = derive_sas(&secret, &transcript);

        assert_ne!(aead_key.as_slice(), link_key.as_slice());
        assert_ne!(aead_key.as_slice(), sas.as_bytes());
    }

    #[test]
    fn control_credential_round_trips_and_binds_both_fingerprints() {
        let secret = [7u8; 32];
        let controller = "a".repeat(64);
        let host = "b".repeat(64);
        let token = control_credential(&secret, &controller, &host).unwrap();
        // fdl1.<发起方指纹>.<base64url 无 padding 的 32 字节 HMAC>
        assert_eq!(token.len(), 4 + 1 + 64 + 1 + 43);
        assert!(token.starts_with(&format!("fdl1.{controller}.")));
        assert!(!token.contains('='));
        assert!(verify_control_credential(
            &secret,
            &controller,
            &host,
            &token
        ));
        assert_eq!(
            parse_control_credential(&token).map(|(fingerprint, _)| fingerprint),
            Some(controller.as_str())
        );

        // 密钥不同 / 换了一台主机（跨主机重放）/ 角色对调 / 声明的指纹与记录不符。
        assert!(!verify_control_credential(
            &[8u8; 32],
            &controller,
            &host,
            &token
        ));
        assert!(!verify_control_credential(
            &secret,
            &controller,
            &"c".repeat(64),
            &token
        ));
        assert!(!verify_control_credential(
            &secret,
            &host,
            &controller,
            &token
        ));
        // 篡改 HMAC 的最后一个字符。
        let mut forged = token.clone();
        let last = forged.pop().unwrap();
        forged.push(if last == 'A' { 'B' } else { 'A' });
        assert!(!verify_control_credential(
            &secret,
            &controller,
            &host,
            &forged
        ));
        // 空密钥既不签发也不通过校验。
        assert!(control_credential(&[], &controller, &host).is_none());
        assert!(!verify_control_credential(&[], &controller, &host, &token));
    }

    #[test]
    fn malformed_control_credentials_are_rejected() {
        let secret = [7u8; 32];
        let controller = "a".repeat(64);
        let host = "b".repeat(64);
        let token = control_credential(&secret, &controller, &host).unwrap();
        let mac = token.rsplit('.').next().unwrap().to_owned();
        let upper = "A".repeat(64);
        let bad = [
            String::new(),
            "fdl1".to_owned(),
            "fdl1.".to_owned(),
            format!("fdl1.{controller}"),
            format!("fdl1.{controller}."),
            format!("fdl2.{controller}.{mac}"),
            format!("FDL1.{controller}.{mac}"),
            format!("fdl1.short.{mac}"),
            format!("fdl1.{upper}.{mac}"),
            format!("fdl1.{controller}.{mac}.extra"),
            format!("fdl1.{controller}.{mac}="),
            format!("fdl1.{controller}.!!!!"),
            // 长度不是 32 字节的 base64url。
            format!("fdl1.{controller}.{}", &mac[..mac.len() - 4]),
            format!("fdl1.{controller}.{mac}AAAA"),
            // Bearer 里常见的其它凭据形态。
            "agent-token".to_owned(),
            "flux2026".to_owned(),
        ];
        for token in bad {
            assert!(
                parse_control_credential(&token).is_none(),
                "{token:?} must not parse"
            );
            assert!(
                !verify_control_credential(&secret, &controller, &host, &token),
                "{token:?} must not verify"
            );
        }
    }
}
