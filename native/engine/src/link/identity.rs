//! 本机设备互联身份：持久 Ed25519 密钥对。
//!
//! 设备 ID = Ed25519 公钥指纹（`hex(sha256(pub))`），是 TOFU（首次信任即固定，
//! 参照 SSH known_hosts）与配对名册的唯一身份依据。私钥首次生成并持久化到引擎
//! `config` 表（key `link.identity_secret`，base64 的 32 字节 seed），此后不变。
//!
//! 配对握手中，每端用本身份私钥对「域分隔串 || 自身临时 X25519 公钥」签名，对端
//! 用其出示的 Ed25519 公钥验签——把长期身份与本次临时密钥绑定，杜绝身份冒充；
//! 再叠加 SAS 肉眼核对防中间人。

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use super::crypto::fingerprint;

/// 引擎 `config` 表中持久化身份私钥 seed 的键名。
pub const IDENTITY_CONFIG_KEY: &str = "link.identity_secret";

/// 本机设备互联身份。
#[derive(Clone)]
pub struct LinkIdentity {
    signing: SigningKey,
    public: [u8; 32],
    fingerprint: String,
}

impl LinkIdentity {
    /// 随机生成一个全新身份（32 字节 OS 随机 seed）。
    #[must_use]
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        Self::from_secret_bytes(&seed)
    }

    /// 由 32 字节 seed 恢复身份（持久化重启路径）。
    #[must_use]
    pub fn from_secret_bytes(seed: &[u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let public = signing.verifying_key().to_bytes();
        let fingerprint = fingerprint(&public);
        Self {
            signing,
            public,
            fingerprint,
        }
    }

    /// 导出私钥 seed（32 字节），供持久化。
    #[must_use]
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Ed25519 公钥（32 字节）。
    #[must_use]
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public
    }

    /// 本机设备指纹（设备 ID）。
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 用本身份私钥对 `msg` 签名（返回 64 字节）。
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }

    /// 用给定 Ed25519 公钥校验 `sig` 是否为 `msg` 的有效签名。
    ///
    /// 公钥/签名长度或点非法一律返回 `false`（不 panic）。
    #[must_use]
    pub fn verify(public_key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        let Ok(pk_arr): Result<[u8; 32], _> = public_key.try_into() else {
            return false;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
            return false;
        };
        let Ok(sig_arr): Result<[u8; 64], _> = sig.try_into() else {
            return false;
        };
        let signature = Signature::from_bytes(&sig_arr);
        vk.verify(msg, &signature).is_ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn generate_roundtrips_through_seed() {
        let id = LinkIdentity::generate();
        let restored = LinkIdentity::from_secret_bytes(&id.secret_bytes());
        assert_eq!(id.public_bytes(), restored.public_bytes());
        assert_eq!(id.fingerprint(), restored.fingerprint());
        assert_eq!(id.fingerprint().len(), 64);
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = LinkIdentity::generate();
        let msg = b"fluxdown-link-transcript";
        let sig = id.sign(msg);
        assert!(LinkIdentity::verify(&id.public_bytes(), msg, &sig));
        // 篡改消息 → 验签失败。
        assert!(!LinkIdentity::verify(&id.public_bytes(), b"tampered", &sig));
        // 换公钥 → 验签失败。
        let other = LinkIdentity::generate();
        assert!(!LinkIdentity::verify(&other.public_bytes(), msg, &sig));
    }

    #[test]
    fn verify_rejects_malformed_inputs() {
        let id = LinkIdentity::generate();
        let sig = id.sign(b"m");
        assert!(!LinkIdentity::verify(&[0u8; 10], b"m", &sig)); // 短公钥
        assert!(!LinkIdentity::verify(&id.public_bytes(), b"m", &[0u8; 10])); // 短签名
    }
}
