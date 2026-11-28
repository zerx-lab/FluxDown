//! `<agent 数据目录>/icons/` 下的图标文件：自定义母版 `custom.png`（导入时原子写入），以及
//! Windows 专用的派生 `.ico`（`custom-<revision>.ico` / 内置图标 `<key>.ico`）。
//!
//! `.ico` 文件名带修订号：Windows 外壳按路径缓存图标，同名覆盖不会刷新任务栏 / 开始菜单。

use std::io::Write;
use std::path::{Path, PathBuf};

use fluxdown_protocol::app_icon::AppIconChoice;
use image::RgbaImage;

use super::image::{self as pipeline, ImageError};

const DIR_NAME: &str = "icons";
const MASTER_FILE: &str = "custom.png";
#[cfg(any(windows, test))]
const CUSTOM_ICO_PREFIX: &str = "custom-";
#[cfg(any(windows, test))]
const ICO_EXTENSION: &str = "ico";

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// 还没有导入过自定义图片（或母版已被删除）。
    #[error("no custom app icon has been imported")]
    NoCustomIcon,
    #[error("app icon file error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] ImageError),
}

/// 图标文件目录；所有写入先比较、再原子替换。
#[derive(Debug, Clone)]
pub(crate) struct IconStore {
    dir: PathBuf,
}

impl IconStore {
    pub(crate) fn new(agent_data_dir: &Path) -> Self {
        Self {
            dir: agent_data_dir.join(DIR_NAME),
        }
    }

    /// 图标目录（`<agent 数据目录>/icons`）。
    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn master_path(&self) -> PathBuf {
        self.dir.join(MASTER_FILE)
    }

    /// 导入用户图片：规范化后原子写入母版，返回新修订号。内容未变时不重写文件。
    pub(crate) fn import(&self, source: &Path) -> Result<String, StoreError> {
        let bytes = pipeline::read_source(source)?;
        let master = pipeline::build_master(&bytes)?;
        let path = self.master_path();
        if !std::fs::read(&path).is_ok_and(|existing| existing == master.png) {
            write_atomic(&path, &master.png)?;
        }
        Ok(master.revision)
    }

    /// 当前母版：修订号（按磁盘内容重新计算）与解码后的图像。
    pub(crate) fn master(&self) -> Result<(String, RgbaImage), StoreError> {
        let png = self.read_master_png()?;
        let image = pipeline::decode_master(&png)?;
        Ok((pipeline::revision_of(&png), image))
    }

    /// 当前母版的修订号；缺失或不可读返回 `None`。
    pub(crate) fn master_revision(&self) -> Option<String> {
        self.read_master_png()
            .ok()
            .map(|png| pipeline::revision_of(&png))
    }

    fn read_master_png(&self) -> Result<Vec<u8>, StoreError> {
        match std::fs::read(self.master_path()) {
            Ok(png) => Ok(png),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NoCustomIcon)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// 非默认选择对应的位图（内置 = 随包资源，自定义 = 母版）；默认返回 `None`。
    pub(crate) fn image_for(
        &self,
        choice: &AppIconChoice,
    ) -> Result<Option<RgbaImage>, StoreError> {
        match choice {
            AppIconChoice::Default => Ok(None),
            AppIconChoice::Builtin(icon) => {
                Ok(Some(pipeline::decode_limited(super::builtin_png(*icon))?))
            }
            AppIconChoice::Custom { .. } => Ok(Some(self.master()?.1)),
        }
    }

    /// Windows 派生 `.ico` 的路径；默认没有。
    #[cfg(any(windows, test))]
    pub(crate) fn ico_path(&self, choice: &AppIconChoice) -> Option<PathBuf> {
        match choice {
            AppIconChoice::Default => None,
            AppIconChoice::Builtin(icon) => {
                Some(self.dir.join(format!("{}.{ICO_EXTENSION}", icon.key())))
            }
            AppIconChoice::Custom { revision } => Some(
                self.dir
                    .join(format!("{CUSTOM_ICO_PREFIX}{revision}.{ICO_EXTENSION}")),
            ),
        }
    }

    /// 保证选择对应的 `.ico` 已落盘并返回路径；默认返回 `None`。
    ///
    /// 自定义的文件名含修订号，存在即视为有效；内置图标的固定文件名逐字节比较后才重写。
    /// 写入自定义 `.ico` 后清理其它修订号的旧文件。
    #[cfg(any(windows, test))]
    pub(crate) fn ensure_ico(&self, choice: &AppIconChoice) -> Result<Option<PathBuf>, StoreError> {
        let Some(path) = self.ico_path(choice) else {
            return Ok(None);
        };
        match choice {
            AppIconChoice::Default => Ok(None),
            AppIconChoice::Builtin(icon) => {
                let ico =
                    pipeline::build_ico(&pipeline::decode_limited(super::builtin_png(*icon))?)?;
                if !std::fs::read(&path).is_ok_and(|existing| existing == ico) {
                    write_atomic(&path, &ico)?;
                }
                Ok(Some(path))
            }
            AppIconChoice::Custom { .. } => {
                if !path.is_file() {
                    let (_, image) = self.master()?;
                    write_atomic(&path, &pipeline::build_ico(&image)?)?;
                }
                self.remove_stale_custom_icos(&path);
                Ok(Some(path))
            }
        }
    }

    #[cfg(any(windows, test))]
    fn remove_stale_custom_icos(&self, keep: &Path) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stale = path != keep
                && path.extension().is_some_and(|ext| ext == ICO_EXTENSION)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(CUSTOM_ICO_PREFIX));
            if stale && let Err(error) = std::fs::remove_file(&path) {
                tracing::debug!(path = %path.display(), %error, "could not remove stale app icon");
            }
        }
    }
}

/// 同目录临时文件 + rename：读者永远看不到半截文件。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp = parent.join(temp_name);
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(cleanup) = std::fs::remove_file(&temp)
                && cleanup.kind() != std::io::ErrorKind::NotFound
            {
                tracing::debug!(path = %temp.display(), error = %cleanup, "could not remove temp icon file");
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-icon-store-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_source(dir: &Path, name: &str, color: [u8; 4]) -> PathBuf {
        let image = RgbaImage::from_pixel(32, 16, Rgba(color));
        let path = dir.join(name);
        std::fs::write(&path, pipeline::encode_png(&image).expect("encode")).expect("write");
        path
    }

    #[test]
    fn import_writes_master_atomically_and_reports_its_revision() {
        let dir = temp_dir("import");
        let store = IconStore::new(&dir);
        let source = write_source(&dir, "a.png", [200, 10, 10, 255]);

        let revision = store.import(&source).expect("import");

        assert_eq!(store.master_revision().as_deref(), Some(revision.as_str()));
        let (read_back, image) = store.master().expect("master");
        assert_eq!(read_back, revision);
        assert_eq!(
            image.dimensions(),
            (pipeline::MASTER_SIZE, pipeline::MASTER_SIZE)
        );
        let leftovers = std::fs::read_dir(dir.join(DIR_NAME))
            .expect("icons dir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn missing_master_is_reported_as_no_custom_icon() {
        let dir = temp_dir("missing");
        let store = IconStore::new(&dir);

        assert_eq!(store.master_revision(), None);
        assert!(matches!(store.master(), Err(StoreError::NoCustomIcon)));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn failed_import_keeps_the_previous_master() {
        let dir = temp_dir("failed");
        let store = IconStore::new(&dir);
        let good = write_source(&dir, "good.png", [1, 2, 3, 255]);
        let revision = store.import(&good).expect("import");
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not an image").expect("write bad");

        assert!(store.import(&bad).is_err());

        assert_eq!(store.master_revision().as_deref(), Some(revision.as_str()));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn custom_ico_is_revisioned_and_stale_ones_are_removed() {
        let dir = temp_dir("ico");
        let store = IconStore::new(&dir);
        let first = write_source(&dir, "first.png", [255, 0, 0, 255]);
        let first_rev = store.import(&first).expect("import first");
        let first_ico = store
            .ensure_ico(&AppIconChoice::Custom {
                revision: first_rev.clone(),
            })
            .expect("ico")
            .expect("path");
        assert!(first_ico.is_file());

        let second = write_source(&dir, "second.png", [0, 255, 0, 255]);
        let second_rev = store.import(&second).expect("import second");
        let second_ico = store
            .ensure_ico(&AppIconChoice::Custom {
                revision: second_rev.clone(),
            })
            .expect("ico")
            .expect("path");

        assert_ne!(first_ico, second_ico);
        assert!(second_ico.is_file());
        assert!(!first_ico.exists(), "stale custom ico must be deleted");
        assert_eq!(
            store.ensure_ico(&AppIconChoice::Default).expect("default"),
            None
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn builtin_ico_is_written_once_and_left_alone_when_identical() {
        let dir = temp_dir("bolt");
        let store = IconStore::new(&dir);
        let bolt = AppIconChoice::Builtin(fluxdown_protocol::app_icon::BuiltinAppIcon::Bolt);

        let path = store.ensure_ico(&bolt).expect("ico").expect("path");
        let first = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.ensure_ico(&bolt).expect("ico again");
        let second = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");

        assert_eq!(first, second, "identical content must not be rewritten");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
