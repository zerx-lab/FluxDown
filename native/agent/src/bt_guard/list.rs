//! 云名单信封校验。
//!
//! agent 只做「这份文档确实由 FluxCloud 签发、格式与序号字段合法」；过期、金丝雀覆盖、
//! 差异防护与宽网段丢弃由 daemon 在 `daemon.bt.guard.cloud.apply` 里再判一遍（引擎是这些规则
//! 的事实源，拒绝原因经 `rejectedReason` 展示）。

use fluxdown_protocol::BtGuardCloudApplyParams;
use serde_json::Value;

use super::keys::LIST_KEYS;
use crate::signed_envelope::{EnvelopeError, SigningKeyEntry, open_envelope};

/// 云名单文档 `format` 字段。
pub(crate) const FORMAT: &str = "fluxdown-bt-guard-list/1";
/// 信封体积上限（含 base64 膨胀）。
pub(crate) const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
/// 载荷解码后的体积上限：16 MiB 信封里 base64 载荷最多解出的字节数。
const MAX_PAYLOAD_BYTES: usize = MAX_ENVELOPE_BYTES / 4 * 3;

/// 云名单验签公钥集合。生产使用 [`KeySet::production`]，测试注入自己的密钥。
#[derive(Clone, Debug)]
pub(crate) struct KeySet(Vec<SigningKeyEntry>);

impl KeySet {
    pub(crate) fn production() -> Self {
        Self(
            LIST_KEYS
                .iter()
                .map(|(id, key)| ((*id).to_owned(), *key))
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new(keys: Vec<SigningKeyEntry>) -> Self {
        Self(keys)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ListError {
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    #[error("invalid payload: {0}")]
    Payload(String),
    #[error("rollback: have sequence {have}, got {got}")]
    Rollback { have: u64, got: u64 },
}

/// 已验签的云名单文档，已经是 `daemon.bt.guard.cloud.apply` 的参数形态：原始信封文本落盘与交付
/// 时原样使用（重新加载时再次验签），已验签载荷只解析一次，不为交付再复制一份。
#[derive(Clone, Debug)]
pub(crate) struct VerifiedList {
    pub(crate) sequence: u64,
    pub(crate) apply: BtGuardCloudApplyParams,
}

/// 信封原始字节 → 文本（信封是 JSON，必为 UTF-8）。
pub(crate) fn text_of(bytes: Vec<u8>) -> Result<String, ListError> {
    String::from_utf8(bytes).map_err(|error| EnvelopeError::Envelope(error.to_string()).into())
}

/// 校验 `envelope` 是 [`KeySet`] 内某把公钥签发的云名单信封，且 `format` / `sequence` 合法。
/// 文本原样保留在结果里（不复制）。
pub(crate) fn verify_list(envelope: String, keys: &KeySet) -> Result<VerifiedList, ListError> {
    let payload = open_envelope(
        envelope.as_bytes(),
        MAX_ENVELOPE_BYTES,
        MAX_PAYLOAD_BYTES,
        keys.0.iter(),
    )?;
    let invalid = |message: &str| ListError::Payload(message.to_owned());
    if payload.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(invalid("format must be fluxdown-bt-guard-list/1"));
    }
    let sequence = payload
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("sequence must be u64"))?;
    Ok(VerifiedList {
        sequence,
        apply: BtGuardCloudApplyParams {
            envelope,
            sequence,
            verified_payload: payload,
        },
    })
}

/// 兄弟模块测试共用的签名夹具（测试自生成密钥，不涉及生产私钥）。
#[cfg(test)]
pub(crate) mod fixtures {
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    use super::{FORMAT, KeySet};
    use crate::signed_envelope::sign_envelope;

    pub(crate) fn signing(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// 测试公钥集合：`btguard-test-1`（种子 7）与备用 `btguard-test-2`（种子 8）。
    pub(crate) fn keys() -> KeySet {
        KeySet::new(vec![
            (
                "btguard-test-1".to_owned(),
                signing(7).verifying_key().to_bytes(),
            ),
            (
                "btguard-test-2".to_owned(),
                signing(8).verifying_key().to_bytes(),
            ),
        ])
    }

    pub(crate) fn payload(sequence: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": FORMAT, "sequence": sequence, "generatedAt": 1_761_900_000_i64,
            "expiresAt": 4_102_444_800_i64,
            "entries": [{"cidr": "1.2.3.4/32", "category": "community", "action": "ban",
                         "reason": "r", "expiresAt": 4_102_444_800_i64}],
            "clientRules": [], "canaries": [], "attribution": [],
        }))
        .expect("payload json")
    }

    /// 主测试密钥签发的 `sequence` 号文档信封。
    pub(crate) fn envelope(sequence: u64) -> Vec<u8> {
        sign_envelope(&payload(sequence), "btguard-test-1", &signing(7))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::fixtures::{keys, payload, signing};
    use super::*;
    use crate::signed_envelope::sign_envelope;

    fn check(bytes: &[u8]) -> Result<VerifiedList, ListError> {
        verify_list(String::from_utf8(bytes.to_vec()).expect("utf-8"), &keys())
    }

    #[test]
    fn signed_document_verifies_and_keeps_the_raw_envelope() {
        let bytes = sign_envelope(&payload(5), "btguard-test-1", &signing(7));
        let list = check(&bytes).expect("verified");
        assert_eq!(list.sequence, 5);
        assert_eq!(list.apply.envelope.as_bytes(), bytes.as_slice());
        assert_eq!(list.apply.sequence, 5);
        assert_eq!(
            list.apply.verified_payload["entries"][0]["cidr"],
            "1.2.3.4/32"
        );
    }

    #[test]
    fn rotation_key_signs_as_well() {
        let bytes = sign_envelope(&payload(1), "btguard-test-2", &signing(8));
        assert_eq!(check(&bytes).expect("verified").sequence, 1);
    }

    #[test]
    fn wrong_key_or_unknown_key_id_is_rejected() {
        let forged = sign_envelope(&payload(1), "btguard-test-1", &signing(9));
        assert!(matches!(
            check(&forged),
            Err(ListError::Envelope(EnvelopeError::Signature))
        ));
        let unknown = sign_envelope(&payload(1), "btguard-test-3", &signing(7));
        assert!(matches!(
            check(&unknown),
            Err(ListError::Envelope(EnvelopeError::Signature))
        ));
        // 生产公钥集合不认测试密钥。
        let valid_for_tests = sign_envelope(&payload(1), "btguard-2026-1", &signing(7));
        let text = String::from_utf8(valid_for_tests).expect("utf-8");
        assert!(matches!(
            verify_list(text, &KeySet::production()),
            Err(ListError::Envelope(EnvelopeError::Signature))
        ));
    }

    #[test]
    fn tampered_payload_fails_signature() {
        let bytes = sign_envelope(&payload(1), "btguard-test-1", &signing(7));
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let other = sign_envelope(&payload(2), "btguard-test-1", &signing(7));
        let other: serde_json::Value = serde_json::from_slice(&other).expect("json");
        envelope["payload"] = other["payload"].clone();
        let tampered = serde_json::to_vec(&envelope).expect("json");
        assert!(matches!(
            check(&tampered),
            Err(ListError::Envelope(EnvelopeError::Signature))
        ));
    }

    #[test]
    fn format_and_sequence_are_enforced_after_the_signature() {
        let wrong_format = serde_json::to_vec(&json!({
            "format": "fluxdown-sniff-pack", "sequence": 1,
        }))
        .expect("json");
        let bytes = sign_envelope(&wrong_format, "btguard-test-1", &signing(7));
        assert!(matches!(check(&bytes), Err(ListError::Payload(_))));
        for sequence in [json!("1"), json!(-1), json!(1.5), json!(null)] {
            let bad =
                serde_json::to_vec(&json!({"format": FORMAT, "sequence": sequence})).expect("json");
            let bytes = sign_envelope(&bad, "btguard-test-1", &signing(7));
            assert!(
                matches!(check(&bytes), Err(ListError::Payload(_))),
                "{sequence}"
            );
        }
        let missing = serde_json::to_vec(&json!({"format": FORMAT})).expect("json");
        let bytes = sign_envelope(&missing, "btguard-test-1", &signing(7));
        assert!(matches!(check(&bytes), Err(ListError::Payload(_))));
    }

    #[test]
    fn malformed_envelopes_are_rejected() {
        for bytes in [
            b"not json".as_slice(),
            br#"{"payload": 1, "signatures": []}"#,
            br#"{"payload": "!!!", "signatures": []}"#,
            br#"{"payload": "e30=", "signatures": []}"#,
        ] {
            assert!(check(bytes).is_err());
        }
    }

    #[test]
    fn non_utf8_download_is_rejected_before_verification() {
        assert!(matches!(
            text_of(vec![0xff, 0xfe]),
            Err(ListError::Envelope(EnvelopeError::Envelope(_)))
        ));
    }

    #[test]
    fn envelope_over_sixteen_mib_is_too_large_before_any_parsing() {
        let oversized = vec![b' '; MAX_ENVELOPE_BYTES + 1];
        assert!(matches!(
            check(&oversized),
            Err(ListError::Envelope(EnvelopeError::TooLarge))
        ));
        // 恰好在上限内的空白体不是超限，而是（非法的）信封。
        let at_limit = vec![b' '; MAX_ENVELOPE_BYTES];
        assert!(matches!(
            check(&at_limit),
            Err(ListError::Envelope(EnvelopeError::Envelope(_)))
        ));
    }

    #[test]
    fn payload_over_the_decoded_limit_is_too_large() {
        let mut big = payload(1);
        big.extend(std::iter::repeat_n(b' ', MAX_PAYLOAD_BYTES));
        let bytes = sign_envelope(&big, "btguard-test-1", &signing(7));
        // base64 膨胀后的信封自身也超出 16 MiB；任一上限先触发都应被判超限。
        assert!(matches!(
            check(&bytes),
            Err(ListError::Envelope(EnvelopeError::TooLarge))
        ));
    }
}
