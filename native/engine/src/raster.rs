//! Bounded raster thumbnails for untrusted image bytes.

use std::io::{self, Cursor, Write};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, codecs::png::PngEncoder};
use tokio::sync::Semaphore;

/// Largest encoded source image accepted by [`thumbnail`] and [`rss_thumbnail`], in bytes.
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_THUMBNAIL_DIMENSION: u32 = 2048;
const MAX_SOURCE_PIXELS: u64 = 16_000_000;
const MAX_SOURCE_DIMENSION: u32 = 8192;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const RSS_MAX_OUTPUT_BYTES: usize = 512 * 1024;
static DECODE_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Clone, Copy, Default)]
pub(crate) struct ThumbnailMask {
    pub circle: bool,
    pub corner_radius: u32,
}

/// Why raster thumbnail generation failed.
#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    /// Requested output dimensions are outside the allowed range.
    #[error("图片尺寸必须在 1..=2048 范围内")]
    InvalidThumbnailDimensions,
    /// The encoded input exceeds [`MAX_SOURCE_BYTES`].
    #[error("图片超过 8 MiB 上限")]
    SourceTooLarge,
    /// The image format could not be determined from the source bytes.
    #[error("不支持或无法识别的图片格式")]
    UnrecognizedFormat,
    /// The recognized format is not PNG, JPEG or WebP.
    #[error("仅支持 PNG、JPEG、WebP 图片")]
    UnsupportedFormat,
    /// The decoder could not read the image header.
    #[error("图片尺寸读取失败: {0}")]
    ReadDimensions(#[source] image::ImageError),
    /// The source image exceeds the dimension or pixel budget.
    #[error("图片尺寸或解码内存超过限制")]
    SourceDimensions,
    /// The decoder could not read the image pixels within its memory budget.
    #[error("图片解码失败: {0}")]
    Decode(#[source] image::ImageError),
    /// The PNG encoder failed.
    #[error("PNG 编码失败: {0}")]
    Encode(#[source] image::ImageError),
    /// The encoded thumbnail exceeds the caller's byte limit.
    #[error("缩略图超过 {max_bytes} 字节上限")]
    OutputTooLarge {
        /// The caller's maximum allowed PNG size.
        max_bytes: usize,
    },
    /// A decoder slot could not be acquired.
    #[error("图片解码器已关闭")]
    DecoderClosed(#[source] tokio::sync::AcquireError),
    /// The blocking worker did not finish normally.
    #[error("图片处理任务失败: {0}")]
    Worker(#[source] tokio::task::JoinError),
}

/// Decode PNG, JPEG or WebP and return a metadata-free PNG thumbnail.
///
/// The thumbnail fits within `max_width` and `max_height` (each in `1..=2048`)
/// without upscaling. Decoding and encoding run on a shared pool of at most two
/// blocking workers; source dimensions, pixels and decoder memory are bounded.
///
/// # Errors
///
/// Returns an error for unsupported or malformed input, exceeded limits, or
/// when producing the PNG would exceed `max_output_bytes`.
///
/// ```
/// use fluxdown_engine::raster::{ThumbnailError, thumbnail};
///
/// # #[tokio::main]
/// # async fn main() {
/// let result = thumbnail(b"not an image".to_vec(), 512, 512, 512 * 1024).await;
/// assert!(matches!(result, Err(ThumbnailError::UnrecognizedFormat)));
/// # }
/// ```
pub async fn thumbnail(
    bytes: Vec<u8>,
    max_width: u32,
    max_height: u32,
    max_output_bytes: usize,
) -> Result<Vec<u8>, ThumbnailError> {
    thumbnail_with_mask(
        bytes,
        max_width,
        max_height,
        max_output_bytes,
        ThumbnailMask::default(),
    )
    .await
}

pub(crate) async fn thumbnail_with_mask(
    bytes: Vec<u8>,
    max_width: u32,
    max_height: u32,
    max_output_bytes: usize,
    mask: ThumbnailMask,
) -> Result<Vec<u8>, ThumbnailError> {
    let dimensions = 1..=MAX_THUMBNAIL_DIMENSION;
    if !dimensions.contains(&max_width) || !dimensions.contains(&max_height) {
        return Err(ThumbnailError::InvalidThumbnailDimensions);
    }
    make_thumbnail(bytes, max_width, max_height, max_output_bytes, &[], mask).await
}

/// Decode PNG, JPEG or WebP into a metadata-free RSS preview.
///
/// The preview fits in 640×640 without upscaling. If the PNG exceeds 512 KiB,
/// the already decoded preview is resized further until it fits.
///
/// # Errors
///
/// Returns the same errors as [`thumbnail`] for malformed input, unsupported
/// formats, or exceeded limits.
///
/// ```
/// # async fn example() {
/// use fluxdown_engine::raster::{ThumbnailError, rss_thumbnail};
///
/// let result = rss_thumbnail(b"not an image".to_vec()).await;
/// assert!(matches!(result, Err(ThumbnailError::UnrecognizedFormat)));
/// # }
/// ```
pub async fn rss_thumbnail(bytes: Vec<u8>) -> Result<Vec<u8>, ThumbnailError> {
    // At 224px even an uncompressed RGBA16 image leaves room for PNG overhead.
    make_thumbnail(
        bytes,
        640,
        640,
        RSS_MAX_OUTPUT_BYTES,
        &[384, 224],
        ThumbnailMask::default(),
    )
    .await
}

async fn make_thumbnail(
    bytes: Vec<u8>,
    max_width: u32,
    max_height: u32,
    max_output_bytes: usize,
    fallback_sides: &'static [u32],
    mask: ThumbnailMask,
) -> Result<Vec<u8>, ThumbnailError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(ThumbnailError::SourceTooLarge);
    }
    let permit = DECODE_SLOTS
        .acquire()
        .await
        .map_err(ThumbnailError::DecoderClosed)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let decoded = decode_raster(bytes)?;
        let mut thumbnail = if mask.circle {
            let side = decoded
                .width()
                .min(decoded.height())
                .min(max_width)
                .min(max_height);
            if decoded.width() == side && decoded.height() == side {
                decoded
            } else {
                decoded.resize_to_fill(side, side, image::imageops::FilterType::Lanczos3)
            }
        } else {
            resize_thumbnail(decoded, max_width, max_height)
        };
        if mask.circle || mask.corner_radius != 0 {
            let mut rgba = thumbnail.into_rgba8();
            let half = f64::from(rgba.width().min(rgba.height())) / 2.0;
            let radius = if mask.circle {
                half
            } else {
                f64::from(mask.corner_radius).min(half)
            };
            mask_corners(&mut rgba, radius);
            thumbnail = DynamicImage::ImageRgba8(rgba);
        }
        for &side in fallback_sides {
            if thumbnail.width() <= side && thumbnail.height() <= side {
                continue;
            }
            match encode_png(&thumbnail, max_output_bytes) {
                Err(ThumbnailError::OutputTooLarge { .. }) => {
                    thumbnail = resize_thumbnail(thumbnail, side, side);
                }
                result => return result,
            }
        }
        encode_png(&thumbnail, max_output_bytes)
    })
    .await
    .map_err(ThumbnailError::Worker)?
}

fn decode_raster(bytes: Vec<u8>) -> Result<DynamicImage, ThumbnailError> {
    let format = image::guess_format(&bytes).map_err(|_| ThumbnailError::UnrecognizedFormat)?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(ThumbnailError::UnsupportedFormat);
    }
    let mut limits = decoder_limits();
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    let mut decoder = reader
        .into_decoder()
        .map_err(ThumbnailError::ReadDimensions)?;
    let (width, height) = decoder.dimensions();
    check_dimensions(width, height)?;
    let decoded_bytes = decoder.total_bytes();
    if format == ImageFormat::Jpeg {
        check_jpeg_budget(width, height, decoded_bytes)?;
    }
    // As in ImageReader::decode, reserve the pixel buffer from the decoder budget.
    limits
        .reserve(decoded_bytes)
        .and_then(|()| decoder.set_limits(limits))
        .map_err(ThumbnailError::Decode)?;
    DynamicImage::from_decoder(decoder).map_err(ThumbnailError::Decode)
}

fn resize_thumbnail(image: DynamicImage, max_width: u32, max_height: u32) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width <= max_width && height <= max_height {
        return image;
    }
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    let out_width = (f64::from(width) * scale).floor().max(1.0) as u32;
    let out_height = (f64::from(height) * scale).floor().max(1.0) as u32;
    image.resize_exact(out_width, out_height, image::imageops::FilterType::Lanczos3)
}

fn mask_corners(image: &mut image::RgbaImage, radius: f64) {
    let (width, height) = image.dimensions();
    let inner = (radius - 0.5).powi(2);
    let outer = (radius + 0.5).powi(2);
    // The four corner squares do not overlap; leave the rest of the pixels untouched.
    for y in 0..radius as u32 {
        let dy = radius - f64::from(y) - 0.5;
        for x in 0..radius as u32 {
            let dx = radius - f64::from(x) - 0.5;
            let distance_squared = dx * dx + dy * dy;
            if distance_squared <= inner {
                continue;
            }
            let coverage = if distance_squared >= outer {
                0.0
            } else {
                radius + 0.5 - distance_squared.sqrt()
            };
            for (x, y) in [
                (x, y),
                (width - 1 - x, y),
                (x, height - 1 - y),
                (width - 1 - x, height - 1 - y),
            ] {
                let alpha = &mut image.get_pixel_mut(x, y).0[3];
                *alpha = (f64::from(*alpha) * coverage).round() as u8;
            }
        }
    }
}

fn encode_png(
    thumbnail: &DynamicImage,
    max_output_bytes: usize,
) -> Result<Vec<u8>, ThumbnailError> {
    let mut output = PngOutput {
        bytes: Vec::new(),
        limit: max_output_bytes,
        limit_exceeded: false,
    };
    let encoded = thumbnail.write_with_encoder(PngEncoder::new(&mut output));
    // The PNG encoder may write the final IEND chunk from Drop, ignoring its I/O error.
    if output.limit_exceeded {
        return Err(ThumbnailError::OutputTooLarge {
            max_bytes: max_output_bytes,
        });
    }
    encoded.map_err(ThumbnailError::Encode)?;
    Ok(output.bytes)
}

fn decoder_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn check_dimensions(width: u32, height: u32) -> Result<(), ThumbnailError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SOURCE_DIMENSION
        || height > MAX_SOURCE_DIMENSION
        || pixels > MAX_SOURCE_PIXELS
        || pixels.saturating_mul(4) > MAX_DECODE_BYTES
    {
        return Err(ThumbnailError::SourceDimensions);
    }
    Ok(())
}

fn check_jpeg_budget(width: u32, height: u32, decoded_bytes: u64) -> Result<(), ThumbnailError> {
    let (width, height) = (u64::from(width), u64::from(height));
    // image's JPEG adapter doesn't enforce max_alloc inside zune-jpeg. A
    // progressive (or multi-scan baseline) JPEG can allocate four i16 planes.
    // MCU padding can be 8, 16, or 32 horizontally, and also 24 vertically.
    let padded_width = width.next_multiple_of(32);
    let padded_height = height.next_multiple_of(32).max(height.next_multiple_of(24));
    let coefficients = padded_width * padded_height * 4 * 2;
    let output = decoded_bytes.max(width * height * 3);
    // Allow 3 KiB per padded column for four sets of IDCT and upsampling
    // row buffers, shared scratch and growth headroom, plus fixed state.
    let scratch = padded_width * (3 * 1024) + 1024 * 1024;
    if coefficients + output + scratch > MAX_DECODE_BYTES {
        return Err(ThumbnailError::SourceDimensions);
    }
    Ok(())
}

struct PngOutput {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl Write for PngOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.limit - self.bytes.len() {
            self.limit_exceeded = true;
            return Err(io::Error::from(io::ErrorKind::FileTooLarge));
        }
        let needed = self.bytes.len() + buf.len();
        let capacity = self.bytes.capacity();
        if needed > capacity {
            let target = capacity.saturating_mul(2).max(needed).min(self.limit);
            self.bytes.reserve_exact(target - self.bytes.len());
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{
        DynamicImage, ExtendedColorType, ImageBuffer, ImageFormat, ImageReader, Rgb, RgbImage,
        Rgba, codecs::jpeg::JpegEncoder,
    };

    use super::{
        MAX_SOURCE_BYTES, RSS_MAX_OUTPUT_BYTES, ThumbnailError, check_dimensions,
        check_jpeg_budget, rss_thumbnail, thumbnail,
    };

    fn image_bytes(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([20, 40, 60])));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, format).unwrap();
        output.into_inner()
    }

    #[tokio::test]
    async fn converts_and_fits_supported_sources_without_upscaling() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            for (side, expected_size) in [(120, (96, 48)), (72, (72, 36))] {
                let output = thumbnail(image_bytes(format, 96, 48), side, side, 512 * 1024)
                    .await
                    .unwrap();
                assert_eq!(image::guess_format(&output).unwrap(), ImageFormat::Png);
                let decoded = ImageReader::with_format(Cursor::new(output), ImageFormat::Png)
                    .decode()
                    .unwrap();
                assert_eq!((decoded.width(), decoded.height()), expected_size);
            }
        }
    }

    fn noise(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }

    fn rss_png_dimensions(bytes: Vec<u8>) -> (u32, u32) {
        assert!(bytes.len() <= RSS_MAX_OUTPUT_BYTES);
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
        ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png)
            .into_dimensions()
            .unwrap()
    }

    #[tokio::test]
    async fn rss_keeps_first_pass_for_jpeg_that_fits() {
        let jpeg = image_bytes(ImageFormat::Jpeg, 800, 400);
        let png = rss_thumbnail(jpeg).await.unwrap();
        assert_eq!(rss_png_dimensions(png), (640, 320));
    }

    #[tokio::test]
    async fn rss_shrinks_jpeg_when_first_png_exceeds_wire_cap() {
        let mut state = 0x437a_37b9;
        let image = RgbImage::from_fn(640, 640, |_, _| {
            let [r, g, b, _] = noise(&mut state).to_le_bytes();
            Rgb([r, g, b])
        });
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode(image.as_raw(), 640, 640, ExtendedColorType::Rgb8)
            .unwrap();

        assert!(matches!(
            thumbnail(jpeg.clone(), 640, 640, RSS_MAX_OUTPUT_BYTES).await,
            Err(ThumbnailError::OutputTooLarge { .. })
        ));
        let png = rss_thumbnail(jpeg).await.unwrap();
        assert_eq!(rss_png_dimensions(png), (384, 384));
    }

    #[tokio::test]
    async fn rss_further_shrinks_high_entropy_rgba16_png() {
        let mut state = 0x437a_37b9;
        let image = DynamicImage::ImageRgba16(ImageBuffer::from_fn(640, 640, |_, _| {
            Rgba(std::array::from_fn(|_| noise(&mut state) as u16))
        }));
        let mut source = Cursor::new(Vec::new());
        image.write_to(&mut source, ImageFormat::Png).unwrap();
        let png = rss_thumbnail(source.into_inner()).await.unwrap();
        assert_eq!(rss_png_dimensions(png), (224, 224));
    }

    #[tokio::test]
    async fn rejects_jpeg_over_decode_budget_before_reading_pixels() {
        // Keep the small JPEG's scan data, but advertise a frame whose unpadded
        // planes would fit. The padded planes plus output and scratch do not.
        let mut jpeg = image_bytes(ImageFormat::Jpeg, 32, 32);
        let sof = jpeg
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .unwrap();
        let dims = jpeg.get_mut(sof + 5..sof + 9).unwrap();
        dims.copy_from_slice(&[0x09, 0x19, 0x08, 0xfc]); // 2329 high, 2300 wide
        assert!(check_dimensions(2300, 2329).is_ok());
        assert!(check_jpeg_budget(2300, 2300, 2300 * 2300 * 3).is_ok());
        assert!(matches!(
            rss_thumbnail(jpeg).await,
            Err(ThumbnailError::SourceDimensions)
        ));
    }

    #[tokio::test]
    async fn enforces_output_boundary_including_final_png_chunk() {
        let input = image_bytes(ImageFormat::Png, 8, 4);
        let expected = thumbnail(input.clone(), 512, 512, 512 * 1024)
            .await
            .unwrap();
        let limit = expected.len() - 1;
        assert!(matches!(
            thumbnail(input.clone(), 512, 512, limit).await,
            Err(ThumbnailError::OutputTooLarge { max_bytes }) if max_bytes == limit
        ));
        assert_eq!(
            thumbnail(input, 512, 512, expected.len()).await.unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn rejects_malformed_unsupported_and_oversized_sources() {
        assert!(matches!(
            thumbnail(b"GIF89a".to_vec(), 512, 512, 512 * 1024).await,
            Err(ThumbnailError::UnsupportedFormat)
        ));
        assert!(matches!(
            thumbnail(b"\x89PNG\r\n\x1a\n".to_vec(), 512, 512, 512 * 1024).await,
            Err(ThumbnailError::ReadDimensions(_))
        ));
        assert!(matches!(
            thumbnail(vec![0; MAX_SOURCE_BYTES + 1], 512, 512, 512 * 1024).await,
            Err(ThumbnailError::SourceTooLarge)
        ));
    }

    #[test]
    fn rejects_pixel_and_dimension_overflow() {
        assert!(check_dimensions(8193, 1).is_err());
        assert!(check_dimensions(4096, 4096).is_err());
        assert!(check_dimensions(4000, 4000).is_ok());
    }
}
