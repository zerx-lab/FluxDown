//! 对话输入的图片附件：按文件头识别格式、必要时转 PNG、生成缩略图与 `data:` URL。
//!
//! 这里只做字节计算，不碰窗口与实体；解码 / 转码较重，调用方（视图）必须放到后台线程执行。

use std::{
    io::{Cursor, Read as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{ClipboardEntry, ClipboardItem};
use image::{ColorType, DynamicImage, ImageDecoder as _, ImageFormat, ImageReader};

/// 转码 / 解码前原始输入的上限：防止把超大文件整个读进内存。
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
/// 缩略图长边（px）。按 2 倍屏取 56px 缩略图的约 1.5 倍，避免把整张大图当纹理上传 GPU。
const THUMBNAIL_EDGE: u32 = 160;
/// 文件选择 / 拖入 / 剪贴板文件认作图片的扩展名（小写）。
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];

/// 云端接受的图片格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mime {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Mime {
    const fn data_url_prefix(self) -> &'static str {
        match self {
            Self::Png => "data:image/png;base64,",
            Self::Jpeg => "data:image/jpeg;base64,",
            Self::Gif => "data:image/gif;base64,",
            Self::Webp => "data:image/webp;base64,",
        }
    }

    const fn gpui_format(self) -> gpui::ImageFormat {
        match self {
            Self::Png => gpui::ImageFormat::Png,
            Self::Jpeg => gpui::ImageFormat::Jpeg,
            Self::Gif => gpui::ImageFormat::Gif,
            Self::Webp => gpui::ImageFormat::Webp,
        }
    }
}

/// 按文件头识别云端直接接受的格式；其他格式（TIFF / BMP 等）返回 `None`。
fn sniff(bytes: &[u8]) -> Option<Mime> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Mime::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Mime::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Mime::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Mime::Webp)
    } else {
        None
    }
}

/// 图片来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageSource {
    /// 已在内存里的图片字节（剪贴板）。
    Bytes(Vec<u8>),
    /// 磁盘文件（拖入、文件选择器、剪贴板里复制的文件）。
    File(PathBuf),
}

/// 加图失败的原因（决定提示文案）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageError {
    /// 不是可识别 / 可解码的图片。
    Unsupported,
    /// 文件读取失败。
    Unreadable,
    /// 超过单张大小上限（转 PNG 后的大小也计入）。
    TooLarge,
}

/// 一张本机可显示的图：缩略图用于输入区 / 气泡，原图只在大图预览时渲染（不把整张大图当缩略图纹理）。
#[derive(Clone)]
pub(crate) struct LocalImage {
    pub(crate) thumbnail: Arc<gpui::Image>,
    pub(crate) full: Arc<gpui::Image>,
}

impl LocalImage {
    /// 占用的已编码字节数（缩略图与原图是同一份时只算一次）。
    pub(crate) fn byte_len(&self) -> usize {
        if Arc::ptr_eq(&self.thumbnail, &self.full) {
            self.full.bytes.len()
        } else {
            self.thumbnail.bytes.len() + self.full.bytes.len()
        }
    }
}

/// 已处理好的一张图：待发送的 `data:` URL 与本机显示用的图。
pub(crate) struct Prepared {
    pub(crate) data_url: String,
    pub(crate) image: LocalImage,
}

/// 路径的扩展名是否是可添加的图片。
pub(crate) fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// 从粘贴的剪贴板内容里取要加成附件的图片。
///
/// - 图片条目（截图 / 复制图片）：有同时附带的文字则视为普通文字粘贴（返回空）；
/// - 复制的文件：全部是图片文件才接管，否则交还给输入框按文字粘贴。
pub(crate) fn clipboard_sources(item: &ClipboardItem) -> Vec<ImageSource> {
    let mut images = Vec::new();
    let mut files = Vec::new();
    let mut non_image_file = false;
    let mut has_text = false;
    for entry in item.entries() {
        match entry {
            ClipboardEntry::Image(image) => images.push(ImageSource::Bytes(image.bytes.clone())),
            ClipboardEntry::ExternalPaths(paths) => {
                for path in paths.paths() {
                    if is_image_path(path) {
                        files.push(ImageSource::File(path.clone()));
                    } else {
                        non_image_file = true;
                    }
                }
            }
            ClipboardEntry::String(string) => has_text |= !string.text.is_empty(),
        }
    }
    if !files.is_empty() || non_image_file {
        // 复制了文件：Finder / 资源管理器的路径文字不算「同时附带的文字」。
        return if non_image_file { Vec::new() } else { files };
    }
    if has_text { Vec::new() } else { images }
}

/// 读出来源字节（文件在这里读取，调用方在后台线程）。
fn load(source: ImageSource) -> Result<Vec<u8>, ImageError> {
    match source {
        ImageSource::Bytes(bytes) => Ok(bytes),
        ImageSource::File(path) => {
            let file = std::fs::File::open(&path).map_err(|error| {
                log::warn!("assistant image open failed: {error}");
                ImageError::Unreadable
            })?;
            let length = file
                .metadata()
                .map_err(|error| {
                    log::warn!("assistant image stat failed: {error}");
                    ImageError::Unreadable
                })?
                .len();
            if length > MAX_SOURCE_BYTES {
                return Err(ImageError::TooLarge);
            }
            let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
            // `take` 兜住读取期间文件继续增长的情况。
            file.take(MAX_SOURCE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    log::warn!("assistant image read failed: {error}");
                    ImageError::Unreadable
                })?;
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                return Err(ImageError::TooLarge);
            }
            Ok(bytes)
        }
    }
}

/// 解码并按 EXIF 方向转正（与 gpui 渲染一致）。
fn decode(bytes: &[u8]) -> Result<DynamicImage, ImageError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ImageError::Unsupported)?;
    let mut decoder = reader.into_decoder().map_err(|error| {
        log::debug!("assistant image decoder rejected: {error}");
        ImageError::Unsupported
    })?;
    let orientation = decoder.orientation().map_err(|error| {
        log::debug!("assistant image orientation failed: {error}");
        ImageError::Unsupported
    })?;
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(|error| {
        log::debug!("assistant image decode failed: {error}");
        ImageError::Unsupported
    })?;
    decoded.apply_orientation(orientation);
    Ok(decoded)
}

/// 编码为 PNG；PNG 不支持的色彩类型（浮点等）先转 RGBA8。
fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, ImageError> {
    let mut out = Cursor::new(Vec::new());
    let result = match image.color() {
        ColorType::L8
        | ColorType::La8
        | ColorType::Rgb8
        | ColorType::Rgba8
        | ColorType::L16
        | ColorType::La16
        | ColorType::Rgb16
        | ColorType::Rgba16 => image.write_to(&mut out, ImageFormat::Png),
        _ => DynamicImage::ImageRgba8(image.to_rgba8()).write_to(&mut out, ImageFormat::Png),
    };
    result.map_err(|error| {
        log::warn!("assistant image png encode failed: {error}");
        ImageError::Unsupported
    })?;
    Ok(out.into_inner())
}

fn data_url(mime: Mime, payload: &[u8]) -> String {
    let prefix = mime.data_url_prefix();
    let mut url = String::with_capacity(prefix.len() + payload.len().div_ceil(3) * 4);
    url.push_str(prefix);
    STANDARD.encode_string(payload, &mut url);
    url
}

/// 把一张图处理成可发送的附件：云端接受的格式原样使用，其他格式转 PNG；
/// 单张（转码后）不得超过 `max_bytes`。
pub(crate) fn prepare(source: ImageSource, max_bytes: u64) -> Result<Prepared, ImageError> {
    let bytes = load(source)?;
    let direct = sniff(&bytes);
    // 直接使用的格式字节数就是最终大小：先于解码拒绝，省掉无谓的计算。
    if direct.is_some() && bytes.len() as u64 > max_bytes {
        return Err(ImageError::TooLarge);
    }
    let decoded = decode(&bytes)?;
    let (mime, payload) = match direct {
        Some(mime) => (mime, bytes),
        None => {
            let png = encode_png(&decoded)?;
            if png.len() as u64 > max_bytes {
                return Err(ImageError::TooLarge);
            }
            (Mime::Png, png)
        }
    };
    let url = data_url(mime, &payload);
    Ok(Prepared {
        data_url: url,
        image: local_image(&decoded, mime, payload)?,
    })
}

/// 历史提问里取回的图片（`data:` URL）→ 缩略图 + 原图；解码较重，调用方在后台线程执行。
pub(crate) fn image_from_data_url(url: &str) -> Result<LocalImage, ImageError> {
    let (header, payload) = url.split_once(',').ok_or(ImageError::Unsupported)?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") {
        return Err(ImageError::Unsupported);
    }
    let bytes = STANDARD
        .decode(payload.trim())
        .map_err(|_| ImageError::Unsupported)?;
    let decoded = decode(&bytes)?;
    match sniff(&bytes) {
        Some(mime) => local_image(&decoded, mime, bytes),
        // 云端只收四种格式；万一不是，原图也转成 PNG 以便 gpui 渲染。
        None => {
            let png = encode_png(&decoded)?;
            local_image(&decoded, Mime::Png, png)
        }
    }
}

/// 由已解码的图与原图字节（`mime` 格式）生成缩略图 + 原图。本身就不大的图直接当缩略图：
/// `thumbnail` 会把小图放大，白白多出像素。
fn local_image(
    decoded: &DynamicImage,
    mime: Mime,
    payload: Vec<u8>,
) -> Result<LocalImage, ImageError> {
    let full = Arc::new(gpui::Image::from_bytes(mime.gpui_format(), payload));
    let thumbnail = if decoded.width().max(decoded.height()) <= THUMBNAIL_EDGE {
        Arc::clone(&full)
    } else {
        let png = encode_png(&decoded.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE))?;
        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png))
    };
    Ok(LocalImage { thumbnail, full })
}

#[cfg(test)]
mod tests {
    use gpui::ExternalPaths;

    use super::*;

    /// 生成一张 `width`×`height` 的纯色测试图，编码为 `format`。
    fn sample(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([200, 40, 40]),
        ));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn sniff_recognises_accepted_formats_by_header() {
        assert_eq!(sniff(&sample(ImageFormat::Png, 2, 2)), Some(Mime::Png));
        assert_eq!(sniff(&sample(ImageFormat::Jpeg, 2, 2)), Some(Mime::Jpeg));
        assert_eq!(sniff(&sample(ImageFormat::Gif, 2, 2)), Some(Mime::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Mime::Webp));
        assert_eq!(sniff(&sample(ImageFormat::Bmp, 2, 2)), None);
        assert_eq!(sniff(&sample(ImageFormat::Tiff, 2, 2)), None);
        assert_eq!(sniff(b"RIFF"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn png_is_sent_as_is_with_a_small_thumbnail() {
        let png = sample(ImageFormat::Png, 400, 300);
        let prepared = prepare(ImageSource::Bytes(png.clone()), u64::MAX).unwrap();
        let (prefix, encoded) = prepared.data_url.split_at("data:image/png;base64,".len());
        assert_eq!(prefix, "data:image/png;base64,");
        assert_eq!(STANDARD.decode(encoded).unwrap(), png);
        let thumbnail = image::load_from_memory(&prepared.image.thumbnail.bytes).unwrap();
        assert!(thumbnail.width().max(thumbnail.height()) <= THUMBNAIL_EDGE);
        // 预览用原图：原样字节，不缩小。
        assert_eq!(prepared.image.full.bytes, png);
    }

    #[test]
    fn tiff_and_bmp_are_converted_to_png() {
        for format in [ImageFormat::Tiff, ImageFormat::Bmp] {
            let prepared = prepare(ImageSource::Bytes(sample(format, 8, 6)), u64::MAX).unwrap();
            let (prefix, encoded) = prepared.data_url.split_at("data:image/png;base64,".len());
            assert_eq!(prefix, "data:image/png;base64,", "{format:?}");
            let payload = STANDARD.decode(encoded).unwrap();
            assert_eq!(sniff(&payload), Some(Mime::Png), "{format:?}");
            let decoded = image::load_from_memory(&payload).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (8, 6), "{format:?}");
        }
    }

    #[test]
    fn size_limit_applies_to_direct_and_converted_images() {
        let png = sample(ImageFormat::Png, 64, 64);
        let limit = png.len() as u64 - 1;
        assert_eq!(
            prepare(ImageSource::Bytes(png), limit).err(),
            Some(ImageError::TooLarge)
        );
        // TIFF 原图未压缩较大，但转成 PNG 后落在上限内：按转码后的大小判定。
        let tiff = sample(ImageFormat::Tiff, 64, 64);
        let converted = encode_png(&decode(&tiff).unwrap()).unwrap();
        assert!(converted.len() < tiff.len());
        assert!(prepare(ImageSource::Bytes(tiff.clone()), converted.len() as u64).is_ok());
        assert_eq!(
            prepare(ImageSource::Bytes(tiff), converted.len() as u64 - 1).err(),
            Some(ImageError::TooLarge)
        );
    }

    #[test]
    fn non_images_are_unsupported() {
        assert_eq!(
            prepare(ImageSource::Bytes(b"not an image".to_vec()), u64::MAX).err(),
            Some(ImageError::Unsupported)
        );
        // 文件头像 PNG 但内容残缺：解码失败也算不支持，不把坏数据发给云端。
        let mut truncated = sample(ImageFormat::Png, 16, 16);
        truncated.truncate(24);
        assert_eq!(
            prepare(ImageSource::Bytes(truncated), u64::MAX).err(),
            Some(ImageError::Unsupported)
        );
    }

    #[test]
    fn missing_file_is_unreadable() {
        let path = PathBuf::from("/nonexistent/fluxdown/assistant/missing.png");
        assert_eq!(
            prepare(ImageSource::File(path), u64::MAX).err(),
            Some(ImageError::Unreadable)
        );
    }

    #[test]
    fn image_paths_are_matched_by_extension() {
        assert!(is_image_path(Path::new("/a/b.PNG")));
        assert!(is_image_path(Path::new("c.tiff")));
        assert!(!is_image_path(Path::new("/a/b.txt")));
        assert!(!is_image_path(Path::new("/a/png")));
    }

    #[test]
    fn clipboard_image_becomes_a_source() {
        let png = sample(ImageFormat::Png, 2, 2);
        let item = ClipboardItem::new_image(&gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            png.clone(),
        ));
        assert_eq!(clipboard_sources(&item), vec![ImageSource::Bytes(png)]);
    }

    #[test]
    fn clipboard_text_wins_over_an_attached_image() {
        let item = ClipboardItem {
            entries: vec![
                ClipboardEntry::Image(gpui::Image::from_bytes(
                    gpui::ImageFormat::Png,
                    sample(ImageFormat::Png, 2, 2),
                )),
                ClipboardEntry::String(gpui::ClipboardString::new("hello".to_owned())),
            ],
        };
        assert!(clipboard_sources(&item).is_empty());
        assert!(clipboard_sources(&ClipboardItem::new_string("hello".to_owned())).is_empty());
    }

    #[test]
    fn clipboard_files_are_taken_only_when_all_are_images() {
        let images = ExternalPaths(
            [PathBuf::from("/a/1.png"), PathBuf::from("/a/2.JPG")]
                .into_iter()
                .collect(),
        );
        // Finder 复制文件时会同时带上路径文字：不应因此放弃接管。
        let item = ClipboardItem {
            entries: vec![
                ClipboardEntry::ExternalPaths(images),
                ClipboardEntry::String(gpui::ClipboardString::new("/a/1.png\n/a/2.JPG".to_owned())),
            ],
        };
        assert_eq!(
            clipboard_sources(&item),
            vec![
                ImageSource::File(PathBuf::from("/a/1.png")),
                ImageSource::File(PathBuf::from("/a/2.JPG")),
            ]
        );

        let mixed = ExternalPaths(
            [PathBuf::from("/a/1.png"), PathBuf::from("/a/notes.txt")]
                .into_iter()
                .collect(),
        );
        let item = ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(mixed)],
        };
        assert!(clipboard_sources(&item).is_empty());
    }

    #[test]
    fn history_data_url_gives_a_small_thumbnail_and_the_original_and_rejects_junk() {
        let png = sample(ImageFormat::Png, 400, 300);
        let url = data_url(Mime::Png, &png);
        let image = image_from_data_url(&url).unwrap();
        let decoded = image::load_from_memory(&image.thumbnail.bytes).unwrap();
        assert!(decoded.width().max(decoded.height()) <= THUMBNAIL_EDGE);
        assert_eq!(image.full.bytes, png);
        assert_eq!(image.byte_len(), image.thumbnail.bytes.len() + png.len());
        // 小图缩略图与原图是同一份，只算一次。
        let small = sample(ImageFormat::Png, 40, 30);
        let image = image_from_data_url(&data_url(Mime::Png, &small)).unwrap();
        assert!(Arc::ptr_eq(&image.thumbnail, &image.full));
        assert_eq!(image.byte_len(), small.len());
        // 头部合法但负载不是图片 / 不是 base64：不支持，不 panic。
        assert_eq!(
            image_from_data_url("data:image/png;base64,aGk=").err(),
            Some(ImageError::Unsupported)
        );
        assert_eq!(
            image_from_data_url("data:image/png,aGk=").err(),
            Some(ImageError::Unsupported)
        );
        assert_eq!(
            image_from_data_url("data:text/plain;base64,aGk=").err(),
            Some(ImageError::Unsupported)
        );
        assert_eq!(
            image_from_data_url("not a data url").err(),
            Some(ImageError::Unsupported)
        );
        assert_eq!(
            image_from_data_url("data:image/png;base64,@@@").err(),
            Some(ImageError::Unsupported)
        );
    }
}
