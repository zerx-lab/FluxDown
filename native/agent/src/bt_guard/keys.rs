//! 云名单内置验签公钥。
//!
//! 主签发私钥由 FluxCloud 以环境变量 `FLUXCLOUD_BT_GUARD_SIGNING_KEY`（base64 seed）持有；
//! 备用轮换私钥由维护者离线保管，都不进仓库。服务端 `FLUXCLOUD_BT_GUARD_SIGNING_KEY_ID` 必须是
//! 下表中的 keyId。
//!
//! 轮换：生成新密钥后在此追加条目；旧条目保留到其签发的最后一份文档过期。

/// 签发云名单信封的公钥：主密钥与备用轮换密钥地位相同，任一签名通过即可。
pub(crate) const LIST_KEYS: &[(&str, [u8; 32])] = &[
    (
        // 1tzcRXxuXLPFvVaEeMutmVO0vRr4R/Vkv/pdEtVJ6Hc=
        "btguard-2026-1",
        [
            0xd6, 0xdc, 0xdc, 0x45, 0x7c, 0x6e, 0x5c, 0xb3, 0xc5, 0xbd, 0x56, 0x84, 0x78, 0xcb,
            0xad, 0x99, 0x53, 0xb4, 0xbd, 0x1a, 0xf8, 0x47, 0xf5, 0x64, 0xbf, 0xfa, 0x5d, 0x12,
            0xd5, 0x49, 0xe8, 0x77,
        ],
    ),
    (
        // n+GF8MUOyqGKNeMU0yEPd8kFSkd3wzhj5mLkWYiaHAk=
        "btguard-2026-2",
        [
            0x9f, 0xe1, 0x85, 0xf0, 0xc5, 0x0e, 0xca, 0xa1, 0x8a, 0x35, 0xe3, 0x14, 0xd3, 0x21,
            0x0f, 0x77, 0xc9, 0x05, 0x4a, 0x47, 0x77, 0xc3, 0x38, 0x63, 0xe6, 0x62, 0xe4, 0x59,
            0x88, 0x9a, 0x1c, 0x09,
        ],
    ),
];

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::LIST_KEYS;

    /// 内置字节必须与契约发布的 base64 公钥逐字节一致，且是合法的 ed25519 点。
    #[test]
    fn embedded_keys_match_the_published_base64() {
        let published = [
            (
                "btguard-2026-1",
                "1tzcRXxuXLPFvVaEeMutmVO0vRr4R/Vkv/pdEtVJ6Hc=",
            ),
            (
                "btguard-2026-2",
                "n+GF8MUOyqGKNeMU0yEPd8kFSkd3wzhj5mLkWYiaHAk=",
            ),
        ];
        assert_eq!(LIST_KEYS.len(), published.len());
        for ((id, bytes), (published_id, encoded)) in LIST_KEYS.iter().zip(published) {
            assert_eq!(*id, published_id);
            assert_eq!(STANDARD.decode(encoded).expect("base64"), bytes.as_slice());
            ed25519_dalek::VerifyingKey::from_bytes(bytes).expect("valid ed25519 public key");
        }
    }
}
