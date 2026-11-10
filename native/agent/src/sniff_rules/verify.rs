//! 签名信封校验：`{"payload": base64, "signatures": [{"keyId", "sig"}]}`。

use serde_json::Value;

use super::keys::{RELEASE_KEYS, REVOCATION_KEYS};
use crate::signed_envelope::{EnvelopeError, SigningKeyEntry, open_envelope};

pub(crate) const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_PACK_PAYLOAD_BYTES: usize = 400 * 1024;
const MAX_REVOCATION_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_RULES: usize = 2000;
const MAX_REVOCATION_ENTRIES: usize = 5000;

#[derive(Debug, thiserror::Error)]
pub enum SniffRuleError {
    #[error("document too large")]
    TooLarge,
    #[error("invalid envelope: {0}")]
    Envelope(String),
    #[error("signature verification failed")]
    Signature,
    #[error("invalid payload: {0}")]
    Payload(String),
    #[error("Rollback: have sequence {have}, got {got}")]
    Rollback { have: u64, got: u64 },
    #[error("io error: {0}")]
    Io(String),
    #[error("http error: {0}")]
    Http(String),
}

impl From<EnvelopeError> for SniffRuleError {
    fn from(error: EnvelopeError) -> Self {
        match error {
            EnvelopeError::TooLarge => Self::TooLarge,
            EnvelopeError::Envelope(message) => Self::Envelope(message),
            EnvelopeError::Signature => Self::Signature,
            EnvelopeError::Payload(message) => Self::Payload(message),
        }
    }
}

/// 文档种类，同时决定允许的签名密钥集合与 payload 结构。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DocKind {
    Official,
    Community,
    Revocations,
}

impl DocKind {
    pub(crate) const ALL: [Self; 3] = [Self::Official, Self::Community, Self::Revocations];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Official => 0,
            Self::Community => 1,
            Self::Revocations => 2,
        }
    }

    pub(crate) const fn file_name(self) -> &'static str {
        match self {
            Self::Official => "official.json",
            Self::Community => "community.json",
            Self::Revocations => "revocations.json",
        }
    }
}

/// 验签公钥集合。生产使用 [`KeySet::production`]，测试注入自己的密钥。
#[derive(Clone, Debug)]
pub(crate) struct KeySet {
    release: Vec<SigningKeyEntry>,
    revocation: Vec<SigningKeyEntry>,
}

impl KeySet {
    pub(crate) fn production() -> Self {
        let own = |keys: &[(&str, [u8; 32])]| {
            keys.iter()
                .map(|(id, key)| ((*id).to_owned(), *key))
                .collect()
        };
        Self {
            release: own(RELEASE_KEYS),
            revocation: own(REVOCATION_KEYS),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(release: Vec<SigningKeyEntry>, revocation: Vec<SigningKeyEntry>) -> Self {
        Self {
            release,
            revocation,
        }
    }

    /// 允许签发该种类文档的公钥：吊销列表 = 发布密钥 ∪ 吊销密钥，其余只认发布密钥。
    fn allowed(&self, kind: DocKind) -> impl Iterator<Item = &SigningKeyEntry> + Clone {
        let revocation: &[(String, [u8; 32])] = if kind == DocKind::Revocations {
            &self.revocation
        } else {
            &[]
        };
        self.release.iter().chain(revocation)
    }
}

pub(crate) struct VerifiedDoc {
    pub(crate) sequence: u64,
    pub(crate) payload: Value,
    /// 原始信封字节，落盘时原样保存，重新加载时再次验签。
    pub(crate) envelope: Vec<u8>,
}

pub(crate) fn verify_envelope(
    bytes: &[u8],
    kind: DocKind,
    keys: &KeySet,
) -> Result<VerifiedDoc, SniffRuleError> {
    let limit = if kind == DocKind::Revocations {
        MAX_REVOCATION_PAYLOAD_BYTES
    } else {
        MAX_PACK_PAYLOAD_BYTES
    };
    let payload = open_envelope(bytes, MAX_ENVELOPE_BYTES, limit, keys.allowed(kind))?;
    let sequence = validate_payload(&payload, kind)?;
    Ok(VerifiedDoc {
        sequence,
        payload,
        envelope: bytes.to_vec(),
    })
}

fn validate_payload(payload: &Value, kind: DocKind) -> Result<u64, SniffRuleError> {
    let invalid = |message: &str| SniffRuleError::Payload(message.to_owned());
    let sequence = payload
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("sequence must be u64"))?;
    match kind {
        DocKind::Official | DocKind::Community => {
            if payload.get("format").and_then(Value::as_str) != Some("fluxdown-sniff-pack") {
                return Err(invalid("format must be fluxdown-sniff-pack"));
            }
            let expected = if kind == DocKind::Official {
                "official"
            } else {
                "community"
            };
            if payload.get("packId").and_then(Value::as_str) != Some(expected) {
                return Err(invalid("packId does not match document kind"));
            }
            if payload.get("engine").and_then(Value::as_u64).is_none() {
                return Err(invalid("engine must be u64"));
            }
            let rules = payload
                .get("rules")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("rules must be an array"))?;
            if rules.len() > MAX_RULES {
                return Err(invalid("too many rules"));
            }
        }
        DocKind::Revocations => {
            if payload.get("format").and_then(Value::as_str) != Some("fluxdown-sniff-revocations") {
                return Err(invalid("format must be fluxdown-sniff-revocations"));
            }
            let entries = payload
                .get("entries")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("entries must be an array"))?;
            if entries.len() > MAX_REVOCATION_ENTRIES {
                return Err(invalid("too many entries"));
            }
        }
    }
    Ok(sequence)
}

/// 测试用信封签名工具与 BT 防吸血云名单共用。
#[cfg(test)]
pub(crate) use crate::signed_envelope::sign_envelope;

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    use super::{sign_envelope as envelope, *};

    fn signing(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn keys() -> KeySet {
        KeySet::new(
            vec![("rel".to_owned(), signing(7).verifying_key().to_bytes())],
            vec![("rev".to_owned(), signing(9).verifying_key().to_bytes())],
        )
    }

    fn pack(pack_id: &str, sequence: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": "fluxdown-sniff-pack", "packId": pack_id, "sequence": sequence,
            "engine": 1, "issuedAt": "2026-10-01T00:00:00Z", "rules": [],
        }))
        .expect("pack json")
    }

    fn revocations(sequence: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": "fluxdown-sniff-revocations", "sequence": sequence,
            "issuedAt": "2026-10-01T00:00:00Z", "expiresAt": "2027-10-01T00:00:00Z",
            "entries": [],
        }))
        .expect("revocation json")
    }

    #[test]
    fn valid_envelope_passes() {
        let bytes = envelope(&pack("official", 3), "rel", &signing(7));
        let doc = verify_envelope(&bytes, DocKind::Official, &keys()).expect("verified");
        assert_eq!(doc.sequence, 3);
        assert_eq!(doc.envelope, bytes);
    }

    #[test]
    fn signer_outside_allowed_set_fails() {
        // 已知 keyId 但签名密钥不对；以及未知 keyId。
        let forged = envelope(&pack("official", 1), "rel", &signing(8));
        assert!(matches!(
            verify_envelope(&forged, DocKind::Official, &keys()),
            Err(SniffRuleError::Signature)
        ));
        let unknown = envelope(&pack("official", 1), "other", &signing(7));
        assert!(matches!(
            verify_envelope(&unknown, DocKind::Official, &keys()),
            Err(SniffRuleError::Signature)
        ));
    }

    #[test]
    fn revocations_accept_release_key() {
        let bytes = envelope(&revocations(1), "rel", &signing(7));
        verify_envelope(&bytes, DocKind::Revocations, &keys())
            .expect("release key signs revocations");
        let bytes = envelope(&revocations(1), "rev", &signing(9));
        verify_envelope(&bytes, DocKind::Revocations, &keys())
            .expect("revocation key signs revocations");
    }

    #[test]
    fn official_rejects_revocation_key() {
        let bytes = envelope(&pack("official", 1), "rev", &signing(9));
        assert!(matches!(
            verify_envelope(&bytes, DocKind::Official, &keys()),
            Err(SniffRuleError::Signature)
        ));
    }

    #[test]
    fn oversized_payload_fails() {
        let mut big = pack("official", 1);
        big.extend(std::iter::repeat_n(b' ', MAX_PACK_PAYLOAD_BYTES));
        let bytes = envelope(&big, "rel", &signing(7));
        assert!(matches!(
            verify_envelope(&bytes, DocKind::Official, &keys()),
            Err(SniffRuleError::TooLarge)
        ));
    }

    #[test]
    fn wrong_format_or_pack_id_fails() {
        let wrong_format = serde_json::to_vec(&json!({
            "format": "other", "packId": "official", "sequence": 1, "engine": 1, "rules": [],
        }))
        .expect("json");
        let bytes = envelope(&wrong_format, "rel", &signing(7));
        assert!(matches!(
            verify_envelope(&bytes, DocKind::Official, &keys()),
            Err(SniffRuleError::Payload(_))
        ));
        let bytes = envelope(&pack("community", 1), "rel", &signing(7));
        assert!(matches!(
            verify_envelope(&bytes, DocKind::Official, &keys()),
            Err(SniffRuleError::Payload(_))
        ));
    }

    /// 计划验证 5：`scripts/sniff-rules/sign.ts` 产出的信封格式（payload 原始字节签名、
    /// 标准 base64、`keyId`/`sig` 字段）与本模块一致；这里用同样的字段布局内联生成。
    #[test]
    fn envelope_layout_matches_sign_script() {
        let bytes = envelope(&pack("community", 2), "rel", &signing(7));
        let value: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(value["payload"].is_string());
        assert_eq!(value["signatures"][0]["keyId"], "rel");
        verify_envelope(&bytes, DocKind::Community, &keys()).expect("community pack");
    }
}
