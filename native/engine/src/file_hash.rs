//! 已完成任务落盘文件的哈希（`daemon.task.hash` 与「下载完成后计算哈希」）。
//!
//! 结果以与任务 `checksum` 相同的 `algo=hex` 形式存入 `tasks.file_hash`，随任务行发布。
//! 只哈希单个常规文件：多文件 BT 的产物是目录，不支持。计算在阻塞线程上进行，写回只在
//! 宿主 actor 内由 [`crate::download_manager::DownloadManager::commit_file_hash`] 完成，
//! 以计算开始时的 `completed_at` 作代际：期间任务被重新下载则结果作废。

use std::io::Read;
use std::path::Path;

/// 支持的摘要算法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    /// 解析算法名：大小写不敏感，接受 `sha-256` / `sha256` 等写法。
    ///
    /// # Examples
    ///
    /// ```
    /// use fluxdown_engine::file_hash::HashAlgorithm;
    ///
    /// assert_eq!(HashAlgorithm::parse("SHA-256"), Some(HashAlgorithm::Sha256));
    /// assert_eq!(HashAlgorithm::parse("crc32"), None);
    /// ```
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "md5" => Some(Self::Md5),
            "sha1" | "sha-1" => Some(Self::Sha1),
            "sha256" | "sha-256" => Some(Self::Sha256),
            "sha512" | "sha-512" => Some(Self::Sha512),
            _ => None,
        }
    }

    /// 规范名（`algo=hex` 中的 `algo`）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    /// 十六进制摘要长度。
    #[must_use]
    pub fn hex_len(self) -> usize {
        match self {
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }
}

/// 哈希无法开始或中途终止。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileHashError {
    #[error("task not found")]
    NotFound,
    #[error("only completed tasks whose download is a single existing file can be hashed")]
    NotEligible,
    #[error("a hash is already being computed for this task")]
    Busy,
    #[error("the task changed while its file was being hashed")]
    Stale,
    #[error("hashing cancelled")]
    Cancelled,
    #[error("hashing failed: {0}")]
    Failed(String),
}

/// 一次哈希作业的结果，回流宿主 actor 由 `commit_file_hash` 写回。
#[derive(Debug)]
pub struct FileHashOutcome {
    pub(crate) task_id: String,
    /// 计算开始时任务的 `completed_at`（写回代际）。
    pub(crate) completed_at: String,
    /// 成功时为 `algo=hex`。
    pub(crate) result: Result<String, FileHashError>,
}

impl FileHashOutcome {
    /// 作业所属任务。
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

/// 读取缓冲：与校验和路径一致的 1 MiB。
const READ_CHUNK: usize = 1024 * 1024;

/// 阻塞地计算 `path` 的摘要，返回小写十六进制；每读一块轮询一次 `is_cancelled`。
/// 路径不存在或不是常规文件（目录产物）为 [`FileHashError::NotEligible`]。
pub(crate) fn hash_file(
    path: &Path,
    algorithm: HashAlgorithm,
    is_cancelled: impl Fn() -> bool,
) -> Result<String, FileHashError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(FileHashError::NotEligible);
        }
        // Windows 打开目录即失败：按目录产物处理，而不是报读盘错误。
        Err(_) if path.is_dir() => return Err(FileHashError::NotEligible),
        Err(error) => {
            return Err(FileHashError::Failed(format!(
                "open {}: {error}",
                path.display()
            )));
        }
    };
    let meta = file
        .metadata()
        .map_err(|error| FileHashError::Failed(format!("stat {}: {error}", path.display())))?;
    if !meta.is_file() {
        return Err(FileHashError::NotEligible);
    }
    match algorithm {
        HashAlgorithm::Md5 => digest_reader::<md5::Md5>(file, &is_cancelled),
        HashAlgorithm::Sha1 => digest_reader::<sha1::Sha1>(file, &is_cancelled),
        HashAlgorithm::Sha256 => digest_reader::<sha2::Sha256>(file, &is_cancelled),
        HashAlgorithm::Sha512 => digest_reader::<sha2::Sha512>(file, &is_cancelled),
    }
}

fn digest_reader<D: sha2::Digest>(
    mut reader: impl Read,
    is_cancelled: &impl Fn() -> bool,
) -> Result<String, FileHashError> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        if is_cancelled() {
            return Err(FileHashError::Cancelled);
        }
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FileHashError::Failed(format!("read: {error}"))),
        };
        hasher.update(buf.get(..read).unwrap_or_default());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `algo=hex` 形式的哈希记录。
#[must_use]
pub(crate) fn hash_spec(algorithm: HashAlgorithm, hex: &str) -> String {
    format!("{}={hex}", algorithm.as_str())
}

#[cfg(test)]
mod tests {
    use super::{FileHashError, HashAlgorithm, hash_file};

    /// 测试夹具目录：随返回值释放时删除。
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("cleanup {}: {error}", self.0.display());
            }
        }
    }

    fn temp_file(bytes: &[u8]) -> (TempDir, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("fluxdown-file-hash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("data.bin");
        std::fs::write(&path, bytes).expect("write");
        (TempDir(dir), path)
    }

    /// 已知测试向量：四种算法对 "abc" 的摘要。
    #[test]
    fn digests_match_reference_vectors() {
        let (_dir, path) = temp_file(b"abc");
        let cases = [
            (HashAlgorithm::Md5, "900150983cd24fb0d6963f7d28e17f72"),
            (
                HashAlgorithm::Sha1,
                "a9993e364706816aba3e25717850c26c9cd0d89d",
            ),
            (
                HashAlgorithm::Sha256,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                HashAlgorithm::Sha512,
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                 2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            ),
        ];
        for (algorithm, expected) in cases {
            let hex = hash_file(&path, algorithm, || false).expect("hash");
            assert_eq!(hex, expected, "{algorithm:?}");
            assert_eq!(hex.len(), algorithm.hex_len());
        }
    }

    /// 取消即停；文件不存在或是目录（多文件产物）不可哈希。
    #[test]
    fn cancelled_missing_and_directory_targets_are_rejected() {
        let (dir, path) = temp_file(b"abc");
        assert_eq!(
            hash_file(&path, HashAlgorithm::Sha256, || true),
            Err(FileHashError::Cancelled)
        );
        assert_eq!(
            hash_file(&dir.0.join("gone.bin"), HashAlgorithm::Sha256, || false),
            Err(FileHashError::NotEligible)
        );
        assert_eq!(
            hash_file(&dir.0, HashAlgorithm::Sha256, || false),
            Err(FileHashError::NotEligible)
        );
    }
}
