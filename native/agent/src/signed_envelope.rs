//! 签名信封校验：`{"payload": base64, "signatures": [{"keyId", "sig"}]}`。
//!
//! 嗅探规则包与 BT 防吸血云名单共用同一信封格式：`sig` 是对 `payload` 解码后**原始字节**的
//! ed25519 签名（`verify_strict`），公钥集合与大小上限由调用方给定。本模块只做密码学与体积层面的
//! 校验并返回解析后的载荷；载荷的语义（`format`、序号……）由各文档种类自己校验。

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::Value;

/// 验签公钥：`(keyId, 原始 32 字节公钥)`。
pub(crate) type SigningKeyEntry = (String, [u8; 32]);

/// 信封层面的失败原因。
#[derive(Debug, thiserror::Error)]
pub(crate) enum EnvelopeError {
    #[error("document too large")]
    TooLarge,
    #[error("invalid envelope: {0}")]
    Envelope(String),
    #[error("signature verification failed")]
    Signature,
    #[error("invalid payload: {0}")]
    Payload(String),
}

/// 信封允许携带的签名条数上限（现有密钥集合至多 3 把，留出轮换余量）。
pub(crate) const MAX_SIGNATURES: usize = 4;

#[derive(Deserialize)]
struct Envelope {
    payload: String,
    signatures: Vec<EnvelopeSignature>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeSignature {
    key_id: String,
    sig: String,
}

/// 校验 `bytes` 是由 `allowed` 中任一公钥签发的信封，返回解析后的载荷。
///
/// 顺序固定：信封体积 → 信封 JSON → 载荷 base64 → 载荷体积 → 签名 → 载荷 JSON。
/// 签名先于载荷解析，不可信数据不会在验签前被当作 JSON 展开。
pub(crate) fn open_envelope<'k, I>(
    bytes: &[u8],
    max_envelope_bytes: usize,
    max_payload_bytes: usize,
    allowed: I,
) -> Result<Value, EnvelopeError>
where
    I: Iterator<Item = &'k SigningKeyEntry> + Clone,
{
    if bytes.len() > max_envelope_bytes {
        return Err(EnvelopeError::TooLarge);
    }
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|error| EnvelopeError::Envelope(error.to_string()))?;
    if envelope.signatures.len() > MAX_SIGNATURES {
        return Err(EnvelopeError::Envelope(format!(
            "too many signatures (max {MAX_SIGNATURES})"
        )));
    }
    let payload_bytes = STANDARD
        .decode(envelope.payload.as_bytes())
        .map_err(|error| EnvelopeError::Envelope(error.to_string()))?;
    if payload_bytes.len() > max_payload_bytes {
        return Err(EnvelopeError::TooLarge);
    }
    if !signed_by_allowed_key(&envelope.signatures, &payload_bytes, allowed) {
        return Err(EnvelopeError::Signature);
    }
    serde_json::from_slice(&payload_bytes)
        .map_err(|error| EnvelopeError::Payload(error.to_string()))
}

fn signed_by_allowed_key<'k, I>(
    signatures: &[EnvelopeSignature],
    payload: &[u8],
    allowed: I,
) -> bool
where
    I: Iterator<Item = &'k SigningKeyEntry> + Clone,
{
    // 同一 keyId 只验第一条：重复条目不会放大验签开销。条数已被 MAX_SIGNATURES 限制。
    signatures.iter().enumerate().any(|(index, candidate)| {
        if signatures
            .iter()
            .take(index)
            .any(|earlier| earlier.key_id == candidate.key_id)
        {
            return false;
        }
        let Some((_, key)) = allowed.clone().find(|(id, _)| *id == candidate.key_id) else {
            return false;
        };
        let Ok(verifying) = VerifyingKey::from_bytes(key) else {
            return false;
        };
        let Ok(raw) = STANDARD.decode(candidate.sig.as_bytes()) else {
            return false;
        };
        let Ok(raw): Result<[u8; 64], _> = raw.try_into() else {
            return false;
        };
        verifying
            .verify_strict(payload, &Signature::from_bytes(&raw))
            .is_ok()
    })
}

/// 测试用：以 `key` 对 `payload` 原始字节签名并封装成信封。
#[cfg(test)]
pub(crate) fn sign_envelope(
    payload: &[u8],
    key_id: &str,
    key: &ed25519_dalek::SigningKey,
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let signature = key.sign(payload);
    serde_json::to_vec(&serde_json::json!({
        "payload": STANDARD.encode(payload),
        "signatures": [{"keyId": key_id, "sig": STANDARD.encode(signature.to_bytes())}],
    }))
    .expect("envelope json")
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn allowed() -> Vec<SigningKeyEntry> {
        vec![
            ("k1".to_owned(), key(1).verifying_key().to_bytes()),
            ("k2".to_owned(), key(2).verifying_key().to_bytes()),
        ]
    }

    fn envelope(payload: &[u8], signatures: &[(&str, &SigningKey)]) -> Vec<u8> {
        let signatures: Vec<_> = signatures
            .iter()
            .map(|(id, signer)| {
                json!({"keyId": id, "sig": STANDARD.encode(signer.sign(payload).to_bytes())})
            })
            .collect();
        serde_json::to_vec(&json!({
            "payload": STANDARD.encode(payload),
            "signatures": signatures,
        }))
        .expect("envelope json")
    }

    fn open(bytes: &[u8]) -> Result<Value, EnvelopeError> {
        open_envelope(bytes, 1 << 20, 1 << 20, allowed().iter())
    }

    #[test]
    fn up_to_the_signature_limit_is_accepted() {
        let (k1, k2, stranger) = (key(1), key(2), key(9));
        // 前几条是无效 / 陌生签名，最后一条（第 4 条）有效。
        let bytes = envelope(
            b"{}",
            &[
                ("x", &stranger),
                ("y", &stranger),
                ("k1", &stranger),
                ("k2", &k2),
            ],
        );
        assert_eq!(MAX_SIGNATURES, 4);
        assert!(open(&bytes).is_ok());
        let single = envelope(b"{}", &[("k1", &k1)]);
        assert!(open(&single).is_ok());
    }

    #[test]
    fn more_signatures_than_the_limit_are_rejected_even_if_one_is_valid() {
        let (k1, stranger) = (key(1), key(9));
        let bytes = envelope(
            b"{}",
            &[
                ("x", &stranger),
                ("y", &stranger),
                ("z", &stranger),
                ("w", &stranger),
                ("k1", &k1),
            ],
        );
        assert!(
            matches!(open(&bytes), Err(EnvelopeError::Envelope(message)) if message.contains("too many signatures"))
        );
    }

    #[test]
    fn a_key_id_is_only_verified_once_so_a_later_duplicate_cannot_rescue_it() {
        let (k1, stranger) = (key(1), key(9));
        // 第一条 k1 签名无效；同 keyId 的第二条有效，但不再被验证。
        let bytes = envelope(b"{}", &[("k1", &stranger), ("k1", &k1)]);
        assert!(matches!(open(&bytes), Err(EnvelopeError::Signature)));
        // 重复条目不影响其他 keyId 的有效签名。
        let k2 = key(2);
        let bytes = envelope(b"{}", &[("k1", &stranger), ("k1", &stranger), ("k2", &k2)]);
        assert!(open(&bytes).is_ok());
    }
}
