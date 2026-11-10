//! 本机 eD2K 用户哈希（user hash）。
//!
//! 服务器与 eMule 系对端都以 user hash 识别客户端：同一哈希同时出现在多个
//! IP 会被服务器拒绝（"Already connected from another session"）、整体封禁，
//! 对端上传队列也会把同哈希的两个客户端一并移除。因此哈希必须**每个安装
//! 随机生成一次并持久化**（DB config `ed2k_user_hash`），登录、HELLO 与
//! HELLOANSWER 统一使用，不能写死。

use std::sync::OnceLock;

use rand::Rng;

use crate::downloader::DownloadError;

/// 持久化 user hash 的 config 键（32 位小写 hex）。
const CONFIG_KEY: &str = "ed2k_user_hash";

static USER_HASH: OnceLock<[u8; 16]> = OnceLock::new();

/// 生成带 eMule 客户端标记（第 5 字节 14、第 14 字节 111）的随机哈希。
fn generate() -> [u8; 16] {
    let mut hash = [0u8; 16];
    rand::rng().fill(&mut hash);
    hash[5] = 14;
    hash[14] = 111;
    hash
}

fn encode_hex(hash: &[u8; 16]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(text: &str) -> Option<[u8; 16]> {
    let text = text.trim();
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 进程内生效的 user hash。下载入口会先经 [`ensure_user_hash`] 载入持久值；
/// 未经 DB 的调用（测试、探针）得到一个进程级随机哈希。
#[must_use]
pub(crate) fn user_hash() -> [u8; 16] {
    *USER_HASH.get_or_init(generate)
}

/// 载入（缺失或损坏时生成并写回）持久化的 user hash，并固定为进程内的值。
///
/// # Errors
///
/// 读写 config 失败返回 [`DownloadError`]。
pub(crate) async fn ensure_user_hash(db: &crate::db::Db) -> Result<[u8; 16], DownloadError> {
    let stored = db
        .get_config(CONFIG_KEY)
        .await?
        .as_deref()
        .and_then(decode_hex);
    let active = *USER_HASH.get_or_init(|| stored.unwrap_or_else(generate));
    if stored != Some(active) {
        db.set_config(CONFIG_KEY, &encode_hex(&active)).await?;
    }
    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::{decode_hex, encode_hex, generate};

    #[test]
    fn generated_hash_carries_emule_markers_and_differs() {
        let a = generate();
        let b = generate();
        assert_eq!((a[5], a[14]), (14, 111));
        assert_ne!(a, b);
    }

    #[test]
    fn hex_round_trip_rejects_malformed() {
        let h = generate();
        assert_eq!(decode_hex(&encode_hex(&h)), Some(h));
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(decode_hex(&"g".repeat(32)), None);
    }
}
