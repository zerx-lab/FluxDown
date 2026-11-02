//! 插件安装 —— zip 解压（防 zip-slip / 防压缩炸弹 / 单层剥壳）与目录拷贝。
//!
//! zip 安全：每个成员路径规整后必须落在目标目录内（拒 `..`/绝对路径）；累计解压
//! ≤50MB 且条目 ≤200（防压缩炸弹）；zip 内 manifest.json 须在根，单层子目录包裹时
//! 自动剥壳。

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::manifest::PluginManifest;
use super::runtime::PluginError;

/// 累计解压上限（50MB）。
const MAX_TOTAL_UNCOMPRESSED: u64 = 50 * 1024 * 1024;
/// 条目数上限。
const MAX_ENTRIES: usize = 200;
/// Signed archive rule seeds are read through a hard byte cap before verification.
const MAX_ARCHIVE_RULE_SEED_BYTES: usize = super::archive_rules::MAX_PACK_BYTES;

/// 从 zip 字节安装到 `<root>/<identity>/`，返回 identity。
pub fn install_from_zip(root: &Path, bytes: &[u8]) -> Result<String, PluginError> {
    let outcome = install_from_zip_with_backup(root, bytes, |_| Ok(()))?;
    if let Err(error) = commit_install(&outcome) {
        if let Err(rollback_error) = rollback_install(&outcome) {
            crate::logger::report_error("plugin", "rollback_install", &rollback_error);
        }
        return Err(error);
    }
    Ok(outcome.identity().to_string())
}

/// 安装插件但保留旧版本，供 manager 在源码校验后提交或回滚。
///
/// `check_manifest` 在 manifest 解析+校验通过后、替换任何已安装目录之前运行；
/// 返回 `Err` 则中止安装（旧版本不动，暂存目录照常清理）。
pub(crate) fn install_from_zip_with_backup(
    root: &Path,
    bytes: &[u8],
    check_manifest: impl FnOnce(&PluginManifest) -> Result<(), PluginError>,
) -> Result<InstallOutcome, PluginError> {
    let tmp = root.join(format!(".tmp_install_{}", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::create_dir_all(&tmp)
            .map_err(|e| PluginError::ManifestInvalid(format!("创建临时目录失败: {e}")))?;
        extract_zip(bytes, &tmp)?;
        // 剥壳：若根无 manifest.json 但唯一子目录含，则以该子目录为根。
        let src_root = resolve_pkg_root(&tmp)?;
        let manifest = read_manifest(&src_root)?;
        validate_archive_rules_seed(&manifest, &src_root)?;
        check_manifest(&manifest)?;
        let identity = manifest.identity.clone();
        install_staged_dir(root, &src_root, identity)
    })();
    // 清理临时目录（无论成败）。
    if let Err(error) = std::fs::remove_dir_all(&tmp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("plugin", "remove_install_staging", &error);
    }
    result
}

/// 从目录安装（不剥壳，`path` 须直接含 manifest.json）。
pub fn install_from_dir(root: &Path, path: &Path) -> Result<String, PluginError> {
    let outcome = install_from_dir_with_backup(root, path)?;
    if let Err(error) = commit_install(&outcome) {
        if let Err(rollback_error) = rollback_install(&outcome) {
            crate::logger::report_error("plugin", "rollback_install", &rollback_error);
        }
        return Err(error);
    }
    Ok(outcome.identity().to_string())
}

/// 安装目录插件但保留旧版本，供 manager 在源码校验后提交或回滚。
pub(crate) fn install_from_dir_with_backup(
    root: &Path,
    path: &Path,
) -> Result<InstallOutcome, PluginError> {
    let manifest = read_manifest(path)?;
    validate_archive_rules_seed(&manifest, path)?;
    let identity = manifest.identity.clone();
    std::fs::create_dir_all(root)
        .map_err(|e| PluginError::ManifestInvalid(format!("创建插件根目录失败: {e}")))?;
    install_staged_dir(root, path, identity)
}

/// 一次安装的结果。旧目录存在时已被改名为 backup，待源码校验通过后再删除。
pub(crate) struct InstallOutcome {
    identity: String,
    dest: PathBuf,
    backup: Option<PathBuf>,
}

impl InstallOutcome {
    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }
}

fn install_staged_dir(
    root: &Path,
    source: &Path,
    identity: String,
) -> Result<InstallOutcome, PluginError> {
    let dest = root.join(&identity);
    let backup = dest.exists().then(|| {
        root.join(format!(
            ".backup_install_{}_{}",
            identity,
            uuid::Uuid::new_v4()
        ))
    });
    if let Some(backup) = &backup {
        std::fs::rename(&dest, backup)
            .map_err(|e| PluginError::ManifestInvalid(format!("备份旧插件失败: {e}")))?;
    }
    if let Err(error) = copy_dir(source, &dest) {
        if let Err(cleanup_error) = std::fs::remove_dir_all(&dest)
            && cleanup_error.kind() != std::io::ErrorKind::NotFound
        {
            crate::logger::report_warning("plugin", "remove_failed_install", &cleanup_error);
        }
        if let Some(backup) = &backup
            && let Err(rollback_error) = std::fs::rename(backup, &dest)
        {
            crate::logger::report_error("plugin", "restore_install_backup", &rollback_error);
        }
        return Err(error);
    }
    Ok(InstallOutcome {
        identity,
        dest,
        backup,
    })
}

/// 提交安装：删除被替换的旧版本。
pub(crate) fn commit_install(outcome: &InstallOutcome) -> Result<(), PluginError> {
    if let Some(backup) = &outcome.backup {
        std::fs::remove_dir_all(backup)
            .map_err(|e| PluginError::ManifestInvalid(format!("清理旧插件备份失败: {e}")))?;
    }
    Ok(())
}

/// 回滚安装：删除新版本并恢复旧目录；新安装则只删除新目录。
pub(crate) fn rollback_install(outcome: &InstallOutcome) -> Result<(), PluginError> {
    if outcome.dest.exists() {
        std::fs::remove_dir_all(&outcome.dest)
            .map_err(|e| PluginError::ManifestInvalid(format!("清理失败插件目录失败: {e}")))?;
    }
    if let Some(backup) = &outcome.backup {
        std::fs::rename(backup, &outcome.dest)
            .map_err(|e| PluginError::ManifestInvalid(format!("恢复旧插件失败: {e}")))?;
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<PluginManifest, PluginError> {
    let bytes = std::fs::read(dir.join("manifest.json"))
        .map_err(|e| PluginError::ManifestInvalid(format!("读取 manifest.json 失败: {e}")))?;
    let manifest = PluginManifest::parse(&bytes)?;
    manifest.validate()?;
    Ok(manifest)
}

fn validate_archive_rules_seed(manifest: &PluginManifest, dir: &Path) -> Result<(), PluginError> {
    let Some(rules) = manifest
        .contributes
        .as_ref()
        .and_then(|contributes| contributes.archive_rules.as_ref())
    else {
        return Ok(());
    };
    let path = dir.join(&rules.entry);
    let file = std::fs::File::open(&path).map_err(|error| {
        PluginError::ManifestInvalid(format!(
            "archiveRules seed '{}' 读取失败: {error}",
            rules.entry
        ))
    })?;
    let read_limit = u64::try_from(MAX_ARCHIVE_RULE_SEED_BYTES + 1)
        .map_err(|error| PluginError::ManifestInvalid(error.to_string()))?;
    let mut bytes = Vec::with_capacity(MAX_ARCHIVE_RULE_SEED_BYTES);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            PluginError::ManifestInvalid(format!(
                "archiveRules seed '{}' 读取失败: {error}",
                rules.entry
            ))
        })?;
    if bytes.len() > MAX_ARCHIVE_RULE_SEED_BYTES {
        return Err(PluginError::ManifestInvalid(format!(
            "archiveRules seed '{}' 超过 {} 字节",
            rules.entry, MAX_ARCHIVE_RULE_SEED_BYTES
        )));
    }
    manifest.validate_archive_rules_content(|entry| (entry == rules.entry).then(|| bytes.clone()))
}

/// 解压 zip 到 `dest`，逐条防穿越 + 累计体积/条目上限。
fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), PluginError> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader)
        .map_err(|e| PluginError::ManifestInvalid(format!("zip 打开失败: {e}")))?;
    if zip.len() > MAX_ENTRIES {
        return Err(PluginError::ManifestInvalid(format!(
            "zip 条目数 {} 超过上限 {MAX_ENTRIES}",
            zip.len()
        )));
    }
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .map_err(|e| PluginError::ManifestInvalid(format!("读取 zip 条目失败: {e}")))?;
        let Some(enclosed) = file.enclosed_name() else {
            return Err(PluginError::ManifestInvalid(
                "zip 含非法路径（zip-slip）".to_string(),
            ));
        };
        // 二次防护：规整后必须落在 dest 内。
        let out_path = dest.join(&enclosed);
        if !is_within(dest, &out_path) {
            return Err(PluginError::ManifestInvalid(
                "zip 成员越界（zip-slip）".to_string(),
            ));
        }
        if file.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| PluginError::ManifestInvalid(format!("创建目录失败: {e}")))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| PluginError::ManifestInvalid(format!("创建父目录失败: {e}")))?;
        }
        // 累计**实际**解压字节上限：以剩余预算 +1 限流本条目读取，读完累加实际字节
        // 再判上限。杜绝「每条目声明极小、实际膨胀」的压缩炸弹（reviewer finding 6）。
        let remaining = MAX_TOTAL_UNCOMPRESSED.saturating_sub(total);
        let mut limited = (&mut file).take(remaining + 1);
        let mut buf = Vec::new();
        limited
            .read_to_end(&mut buf)
            .map_err(|e| PluginError::ManifestInvalid(format!("解压读取失败: {e}")))?;
        total = total.saturating_add(buf.len() as u64);
        if total > MAX_TOTAL_UNCOMPRESSED {
            return Err(PluginError::ManifestInvalid(format!(
                "解压总量超过 {MAX_TOTAL_UNCOMPRESSED} 字节上限（疑压缩炸弹）"
            )));
        }
        std::fs::write(&out_path, &buf)
            .map_err(|e| PluginError::ManifestInvalid(format!("写入解压文件失败: {e}")))?;
    }
    Ok(())
}

/// `child` 规整后是否落在 `base` 内。
fn is_within(base: &Path, child: &Path) -> bool {
    // 逐组件校验：不允许任何 ParentDir/RootDir/Prefix 逃逸。
    let rel = match child.strip_prefix(base) {
        Ok(r) => r,
        Err(_) => return false,
    };
    for comp in rel.components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            _ => {}
        }
    }
    true
}

/// 剥壳：根含 manifest.json 直接返回；否则若唯一子目录含 manifest.json 则返回它。
fn resolve_pkg_root(tmp: &Path) -> Result<PathBuf, PluginError> {
    if tmp.join("manifest.json").is_file() {
        return Ok(tmp.to_path_buf());
    }
    let mut subdirs = Vec::new();
    let mut has_root_files = false;
    let rd = std::fs::read_dir(tmp)
        .map_err(|e| PluginError::ManifestInvalid(format!("读取暂存目录失败: {e}")))?;
    for entry in rd {
        let entry =
            entry.map_err(|e| PluginError::ManifestInvalid(format!("读取暂存条目失败: {e}")))?;
        let p = entry.path();
        if p.is_dir() {
            subdirs.push(p);
        } else {
            has_root_files = true;
        }
    }
    if !has_root_files && subdirs.len() == 1 && subdirs[0].join("manifest.json").is_file() {
        return Ok(subdirs[0].clone());
    }
    Err(PluginError::ManifestInvalid(
        "zip 内未找到根 manifest.json".to_string(),
    ))
}

/// 递归拷贝目录（安装到最终位置）。
fn copy_dir(src: &Path, dest: &Path) -> Result<(), PluginError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| PluginError::ManifestInvalid(format!("创建目标目录失败: {e}")))?;
    let rd = std::fs::read_dir(src)
        .map_err(|e| PluginError::ManifestInvalid(format!("读取源目录失败: {e}")))?;
    for entry in rd {
        let entry =
            entry.map_err(|e| PluginError::ManifestInvalid(format!("读取源条目失败: {e}")))?;
        let from = entry.path();
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = dest.join(name);
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| PluginError::ManifestInvalid(format!("拷贝文件失败: {e}")))?;
        }
    }
    Ok(())
}
