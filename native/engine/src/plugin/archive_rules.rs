use std::collections::HashSet;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{ArchiveContext, ArchiveMetadata};

use super::runtime::PluginError;

pub(crate) const MAX_PACK_BYTES: usize = 512 * 1024;
const MAX_RULES: usize = 500;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;
pub(crate) async fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, PluginError> {
    use tokio::io::AsyncReadExt;

    let file = tokio::fs::File::open(path).await.map_err(|error| {
        PluginError::ManifestInvalid(format!("archive rules seed read failed: {error}"))
    })?;
    let limit = u64::try_from(MAX_PACK_BYTES + 1)
        .map_err(|error| PluginError::ManifestInvalid(error.to_string()))?;
    let mut bytes = Vec::with_capacity(MAX_PACK_BYTES);
    file.take(limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            PluginError::ManifestInvalid(format!("archive rules seed read failed: {error}"))
        })?;
    if bytes.len() > MAX_PACK_BYTES {
        return Err(PluginError::ManifestInvalid(
            "archive rules seed exceeds 512 KiB".to_owned(),
        ));
    }
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedArchiveRules {
    sequence: u64,
    digest: String,
    rules: Vec<Rule>,
}

#[derive(Clone, Debug)]
pub(crate) struct ArchiveRuleMatch<'a> {
    pub id: &'a str,
    pub folder_name: &'a str,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Payload {
    format: String,
    schema_version: u64,
    sequence: u64,
    rules: Vec<Rule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Rule {
    id: String,
    folder_name: String,
    #[serde(rename = "match")]
    matcher: Matcher,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Matcher {
    file_name: Option<String>,
    extensions: Option<Vec<String>>,
    metadata: Option<MetadataMatch>,
    header_hex: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MetadataMatch {
    platform: Option<String>,
    media_kind: Option<String>,
    creator_id: Option<String>,
    creator_name: Option<String>,
    series_id: Option<String>,
    series_title: Option<String>,
    collection_id: Option<String>,
    collection_title: Option<String>,
}

impl VerifiedArchiveRules {
    pub(crate) fn verify(bytes: &[u8], public_key: &str) -> Result<Self, PluginError> {
        let invalid = |message: &str| PluginError::ManifestInvalid(message.to_owned());
        if bytes.len() > MAX_PACK_BYTES {
            return Err(invalid("archive rules pack exceeds 512 KiB"));
        }
        let key_bytes = decode_hex(public_key)
            .filter(|b| b.len() == 32)
            .ok_or_else(|| invalid("archive rules public key must be 64 hexadecimal characters"))?;
        let key_array: [u8; 32] = key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| invalid("invalid public key length"))?;
        let key = VerifyingKey::from_bytes(&key_array)
            .map_err(|_| invalid("invalid Ed25519 public key"))?;
        let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| {
            PluginError::ManifestInvalid(format!("invalid archive rules envelope: {e}"))
        })?;
        let signature_bytes = decode_hex(&envelope.signature)
            .filter(|b| b.len() == 64)
            .ok_or_else(|| invalid("archive rules signature must be 128 hexadecimal characters"))?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| invalid("invalid archive rules signature"))?;
        key.verify_strict(envelope.payload.as_bytes(), &signature)
            .map_err(|_| invalid("archive rules signature verification failed"))?;
        let payload: Payload = serde_json::from_str(&envelope.payload).map_err(|e| {
            PluginError::ManifestInvalid(format!("invalid archive rules payload: {e}"))
        })?;
        if payload.format != "fluxdown-archive-rules" || payload.schema_version != 1 {
            return Err(invalid(
                "unsupported archive rules format or schema version",
            ));
        }
        if payload.sequence == 0 || payload.sequence > MAX_SEQUENCE {
            return Err(invalid("archive rules sequence is out of range"));
        }
        if payload.rules.is_empty() || payload.rules.len() > MAX_RULES {
            return Err(invalid("archive rules must contain 1..=500 rules"));
        }
        let mut ids = HashSet::with_capacity(payload.rules.len());
        for rule in &payload.rules {
            if rule.id.is_empty()
                || rule.id.len() > 64
                || !rule
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                || !ids.insert(rule.id.as_str())
            {
                return Err(invalid("archive rule id is invalid or duplicated"));
            }
            if !crate::category_route::valid_archive_folder(&rule.folder_name) {
                return Err(invalid("archive rule folderName is unsafe"));
            }
            validate_matcher(&rule.matcher).map_err(invalid)?;
        }
        let digest = Sha256::digest(envelope.payload.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            sequence: payload.sequence,
            digest,
            rules: payload.rules,
        })
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub(crate) fn match_context(&self, context: &ArchiveContext) -> Option<ArchiveRuleMatch<'_>> {
        self.rules
            .iter()
            .find(|rule| rule_matches(rule, context))
            .map(|rule| ArchiveRuleMatch {
                id: &rule.id,
                folder_name: &rule.folder_name,
            })
    }
}

fn validate_matcher(m: &Matcher) -> Result<(), &'static str> {
    if m.file_name.is_none()
        && m.extensions.is_none()
        && m.metadata.is_none()
        && m.header_hex.is_none()
    {
        return Err("archive rule match has no predicates");
    }
    if let Some(pattern) = &m.file_name
        && (pattern.is_empty() || pattern.len() > 256 || !pattern.is_ascii())
    {
        return Err("fileName glob must be 1..=256 ASCII characters");
    }
    if let Some(extensions) = &m.extensions
        && (extensions.is_empty()
            || extensions.len() > 64
            || extensions.iter().any(|e| {
                e.is_empty() || e.len() > 32 || !e.bytes().all(|b| b.is_ascii_alphanumeric())
            }))
    {
        return Err("extensions must contain 1..=64 dotless ASCII extensions of 1..=32 characters");
    }
    if let Some(metadata) = &m.metadata {
        let values = [
            &metadata.platform,
            &metadata.media_kind,
            &metadata.creator_id,
            &metadata.creator_name,
            &metadata.series_id,
            &metadata.series_title,
            &metadata.collection_id,
            &metadata.collection_title,
        ];
        if values
            .iter()
            .filter_map(|value| value.as_ref())
            .any(|s| s.is_empty() || s.len() > 256)
            || values.iter().all(|value| value.is_none())
        {
            return Err("metadata predicates must be nonempty exact strings of at most 256 bytes");
        }
    }
    if let Some(header) = &m.header_hex
        && (header.len() < 2
            || header.len() > 8192
            || !header.len().is_multiple_of(2)
            || !header.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("headerHex must be 2..=8192 even hexadecimal characters");
    }
    Ok(())
}

fn rule_matches(rule: &Rule, context: &ArchiveContext) -> bool {
    let m = &rule.matcher;
    if m.file_name
        .as_ref()
        .is_some_and(|p| !glob_matches(p, &context.file_name))
    {
        return false;
    }
    if let Some(exts) = &m.extensions {
        let actual = context.file_name.rsplit_once('.').map(|(_, e)| e);
        if !actual.is_some_and(|actual| exts.iter().any(|e| e.eq_ignore_ascii_case(actual))) {
            return false;
        }
    }
    if let Some(expected) = &m.metadata {
        let Some(actual) = context.metadata.as_ref() else {
            return false;
        };
        if !metadata_matches(expected, actual) {
            return false;
        }
    }
    if let Some(header) = &m.header_hex {
        let Some(actual) = context.header_hex.as_deref() else {
            return false;
        };
        if actual.len() < header.len()
            || !actual.is_ascii()
            || !actual.as_bytes()[..header.len()].eq_ignore_ascii_case(header.as_bytes())
        {
            return false;
        }
    }
    true
}

fn metadata_matches(e: &MetadataMatch, a: &ArchiveMetadata) -> bool {
    fn eq(expected: &Option<String>, actual: &str) -> bool {
        expected.as_ref().is_none_or(|v| v == actual)
    }
    eq(&e.platform, &a.platform)
        && eq(&e.media_kind, &a.media_kind)
        && eq(&e.creator_id, &a.creator_id)
        && eq(&e.creator_name, &a.creator_name)
        && eq(&e.series_id, &a.series_id)
        && eq(&e.series_title, &a.series_title)
        && eq(&e.collection_id, &a.collection_id)
        && eq(&e.collection_title, &a.collection_title)
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    // Patterns are ASCII, but `*` and `?` must still operate on Unicode filename characters.
    let pattern = pattern.as_bytes();
    let (mut pattern_index, mut value_index, mut star, mut retry) = (0, 0, None, 0);
    while value_index < value.len() {
        let Some(value_character) = value
            .get(value_index..)
            .and_then(|remaining| remaining.chars().next())
        else {
            return false;
        };
        let value_next = value_index + value_character.len_utf8();
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?'
                || pattern[pattern_index].eq_ignore_ascii_case(&(value_character as u8))
                    && value_character.is_ascii())
        {
            pattern_index += 1;
            value_index = value_next;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            retry = value_index;
        } else if let Some(star_index) = star {
            let Some(next_character) = value.get(retry..).and_then(|s| s.chars().next()) else {
                return false;
            };
            retry += next_character.len_utf8();
            value_index = retry;
            pattern_index = star_index + 1;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    hex::decode(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(matcher: Matcher, id: &'static str, folder: &'static str) -> Rule {
        Rule {
            id: id.to_owned(),
            folder_name: folder.to_owned(),
            matcher,
        }
    }

    #[test]
    fn glob_is_full_name_case_insensitive() {
        assert!(glob_matches("*.MKV", "Episode-01.mkv"));
        assert!(glob_matches("ep?sode*", "EPISODE 01"));
        assert!(!glob_matches("*.mkv", "x.mkv.bak"));
        assert!(!glob_matches("a?c", "ac"));
    }

    #[test]
    fn glob_matches_unicode_names_without_splitting_characters() {
        assert!(glob_matches("?.mkv", "片.mkv"));
        assert!(glob_matches("*-01.mkv", "番組-01.mkv"));
        assert!(!glob_matches("??.mkv", "片.mkv"));
    }

    #[test]
    fn matching_combines_predicates_and_first_rule_wins() {
        let context = ArchiveContext {
            file_name: "Episode-01.MKV".into(),
            metadata: Some(ArchiveMetadata {
                platform: "site".into(),
                ..Default::default()
            }),
            is_group: false,
            header_hex: Some("1A2B00".into()),
            ..Default::default()
        };
        let matcher = Matcher {
            file_name: Some("episode-*.mkv".into()),
            extensions: Some(vec!["mkv".into()]),
            metadata: Some(MetadataMatch {
                platform: Some("site".into()),
                ..empty_metadata()
            }),
            header_hex: Some("1a2b".into()),
        };
        let rules = [
            rule(matcher.clone(), "first", "First"),
            rule(matcher, "second", "Second"),
        ];
        let selected = rules.iter().find(|r| rule_matches(r, &context));
        assert_eq!(selected.map(|r| r.id.as_str()), Some("first"));
        let mut missing_header = context.clone();
        missing_header.header_hex = None;
        assert!(!rule_matches(&rules[0], &missing_header));
    }

    #[test]
    fn rejects_malformed_format_and_signature_inputs() {
        assert!(VerifiedArchiveRules::verify(b"{", &"00".repeat(32)).is_err());
        assert!(
            VerifiedArchiveRules::verify(
                b"{\"payload\":\"{}\",\"signature\":\"00\"}",
                &"00".repeat(32)
            )
            .is_err()
        );
        assert!(
            VerifiedArchiveRules::verify(&vec![0; MAX_PACK_BYTES + 1], &"00".repeat(32)).is_err()
        );
    }

    #[test]
    fn verifies_exact_payload_and_detects_tampering() {
        let payload = "{\"format\":\"fluxdown-archive-rules\",\"schemaVersion\":1,\"sequence\":1,\"rules\":[{\"id\":\"first\",\"folderName\":\"Series\",\"match\":{\"extensions\":[\"mkv\"]}}]}";
        let signature = "887d638fd517b304863762f7bb28d77366d47ff9631e76a0ced3c8d7191edb4664863ac908417e4880cbff89df6cee94cbad21afe9429573694530d6920db402";
        let public_key = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";
        let envelope = format!(
            "{{\"payload\":{},\"signature\":\"{}\"}}",
            serde_json::to_string(payload).unwrap_or_default(),
            signature
        );
        let verified = VerifiedArchiveRules::verify(envelope.as_bytes(), public_key)
            .unwrap_or_else(|e| panic!("valid fixture rejected: {e}"));
        assert_eq!(verified.sequence(), 1);
        assert_eq!(verified.rule_count(), 1);
        assert_eq!(
            verified
                .match_context(&ArchiveContext {
                    file_name: "a.MKV".into(),
                    ..Default::default()
                })
                .map(|m| m.id),
            Some("first")
        );

        let tampered = envelope.replace("\\\"sequence\\\":1", "\\\"sequence\\\":2");
        assert_ne!(tampered, envelope);
        assert!(VerifiedArchiveRules::verify(tampered.as_bytes(), public_key).is_err());
        assert!(VerifiedArchiveRules::verify(envelope.as_bytes(), &"00".repeat(32)).is_err());
    }

    #[test]
    fn rejects_unsafe_folders_and_invalid_predicates() {
        assert!(!crate::category_route::valid_archive_folder("../unsafe"));
        assert!(glob_matches("*.mkv", "a.mkv"));
        assert!(
            validate_matcher(&Matcher {
                file_name: None,
                extensions: None,
                metadata: None,
                header_hex: None
            })
            .is_err()
        );
        assert!(
            validate_matcher(&Matcher {
                file_name: None,
                extensions: Some(vec![".mkv".into()]),
                metadata: None,
                header_hex: None
            })
            .is_err()
        );
    }

    fn empty_metadata() -> MetadataMatch {
        MetadataMatch {
            platform: None,
            media_kind: None,
            creator_id: None,
            creator_name: None,
            series_id: None,
            series_title: None,
            collection_id: None,
            collection_title: None,
        }
    }
}
