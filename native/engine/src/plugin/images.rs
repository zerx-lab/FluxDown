//! Plugin image loading, thumbnailing, and session-scoped opaque image references.

use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_WIDTH: u32 = 2048;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_PIXELS: u64 = 16_000_000;
const MAX_DIMENSION: u32 = 8192;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const SESSION_IMAGE_COUNT: usize = 128;
const GLOBAL_IMAGE_COUNT: usize = 4096;
const SESSION_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const GLOBAL_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Arguments accepted by `ctx.images.load` / `flux.images.load`.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageLoadOptions {
    /// Image URL.
    pub url: String,
    /// Additional request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional plugin-owned authentication profile reference.
    #[serde(default)]
    pub auth_ref: Option<String>,
    /// Maximum thumbnail width; defaults to 512.
    #[serde(default = "default_dimension")]
    pub max_width: u32,
    /// Maximum thumbnail height; defaults to 512.
    #[serde(default = "default_dimension")]
    pub max_height: u32,
    /// Whether to memoize the generated image reference; defaults to true.
    #[serde(default = "default_cache")]
    pub cache: bool,
}

fn default_dimension() -> u32 {
    512
}
fn default_cache() -> bool {
    true
}

impl ImageLoadOptions {
    /// Validate the URL, thumbnail dimensions, and bounded HTTP headers before network access.
    pub fn validate(&self) -> Result<(), crate::plugin::runtime::PluginError> {
        if !(1..=MAX_WIDTH).contains(&self.max_width) || !(1..=MAX_WIDTH).contains(&self.max_height)
        {
            return Err(crate::plugin::runtime::PluginError::InvalidOutput(
                "图片尺寸必须在 1..=2048 范围内".to_string(),
            ));
        }
        if self.url.is_empty() || self.url.len() > 16 * 1024 {
            return Err(crate::plugin::runtime::PluginError::InvalidOutput(
                "图片 URL 非法".to_string(),
            ));
        }
        if self.headers.len() > 64 {
            return Err(crate::plugin::runtime::PluginError::InvalidOutput(
                "图片请求头最多 64 项".to_string(),
            ));
        }
        let mut total = 0usize;
        for (name, value) in &self.headers {
            total = total.saturating_add(name.len()).saturating_add(value.len());
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                crate::plugin::runtime::PluginError::InvalidOutput(format!(
                    "图片请求头名称非法: {e}"
                ))
            })?;
            reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                crate::plugin::runtime::PluginError::InvalidOutput(format!("图片请求头值非法: {e}"))
            })?;
        }
        if total > 64 * 1024 {
            return Err(crate::plugin::runtime::PluginError::InvalidOutput(
                "图片请求头总量超过 64 KiB".to_string(),
            ));
        }
        Ok(())
    }
}

struct ImageEntry {
    session: String,
    bytes: Arc<[u8]>,
}

struct CachedRef {
    id: String,
    expires: Instant,
}

#[derive(Default)]
struct Registry {
    sessions: HashMap<String, String>,
    entries: HashMap<String, ImageEntry>,
    cache: HashMap<(String, String, [u8; 32]), CachedRef>,
    total_bytes: usize,
}

/// Synchronous in-memory image registry; references belong to exactly one open plugin session.
///
/// Call [`open_session`](Self::open_session) before exposing image load to a plugin and close it
/// when that host session ends. Registered image bytes never survive session closure.
pub struct ImageResources {
    state: Mutex<Registry>,
}

impl Default for ImageResources {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageResources {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(Registry::default()),
        }
    }

    /// Register a fresh plugin identity/session pair without resetting an already-open session.
    pub fn open_session(&self, identity: &str, session: &str) {
        let mut state = self.lock();
        if state.sessions.contains_key(session) {
            return;
        }
        state
            .sessions
            .insert(session.to_string(), identity.to_string());
    }

    /// Close a session and synchronously remove its image bytes and cache records.
    pub fn close_session(&self, session: &str) {
        let mut state = self.lock();
        state.sessions.remove(session);
        remove_where(&mut state, |entry| entry.session == session);
        state.cache.retain(|(_, s, _), _| s != session);
    }

    /// Close every session and release all image bytes owned by a plugin identity.
    pub fn close_plugin(&self, identity: &str) {
        let mut state = self.lock();
        let sessions: Vec<String> = state
            .sessions
            .iter()
            .filter(|(_, owner)| *owner == identity)
            .map(|(session, _)| session.clone())
            .collect();
        for session in sessions {
            state.sessions.remove(&session);
            remove_where(&mut state, |entry| entry.session == session);
            state
                .cache
                .retain(|(i, s, _), _| i != identity || s != &session);
        }
    }

    /// Return a cached `flux-image:<uuid>` reference if it is still live and session-owned.
    pub fn cached(
        &self,
        identity: &str,
        session: &str,
        opts: &ImageLoadOptions,
    ) -> Result<Option<String>, crate::plugin::runtime::PluginError> {
        let mut state = self.lock();
        if state.sessions.get(session).map(String::as_str) != Some(identity) {
            return Err(session_error());
        }
        if !opts.cache {
            return Ok(None);
        }
        let key = options_key(opts)?;
        let cache_id = (identity.to_string(), session.to_string(), key);
        let Some(cached) = state.cache.get(&cache_id) else {
            return Ok(None);
        };
        if cached.expires <= Instant::now() {
            state.cache.remove(&cache_id);
            return Ok(None);
        }
        Ok(Some(format!("flux-image:{}", cached.id)))
    }

    /// Store sanitized image bytes and return a `flux-image:<uuid>` reference. Visible references are never evicted.
    pub fn insert(
        &self,
        identity: &str,
        session: &str,
        opts: &ImageLoadOptions,
        bytes: Vec<u8>,
    ) -> Result<String, crate::plugin::runtime::PluginError> {
        if bytes.is_empty() || bytes.len() > MAX_OUTPUT_BYTES {
            return Err(image_error("图片资源必须为 1..=4 MiB"));
        }
        let key = opts.cache.then(|| options_key(opts)).transpose()?;
        let size = bytes.len();
        let mut state = self.lock();
        if state.sessions.get(session).map(String::as_str) != Some(identity) {
            return Err(session_error());
        }
        if let Some(key) = key {
            let cache_id = (identity.to_string(), session.to_string(), key);
            if let Some(cached) = state.cache.get(&cache_id)
                && cached.expires > Instant::now()
                && state.entries.contains_key(&cached.id)
            {
                return Ok(format!("flux-image:{}", cached.id));
            }
            state.cache.remove(&cache_id);
        }
        let session_entries = state
            .entries
            .values()
            .filter(|entry| entry.session == session)
            .count();
        let session_bytes: usize = state
            .entries
            .values()
            .filter(|entry| entry.session == session)
            .map(|entry| entry.bytes.len())
            .sum();
        if state.entries.len() >= GLOBAL_IMAGE_COUNT
            || session_entries >= SESSION_IMAGE_COUNT
            || session_bytes.saturating_add(size) > SESSION_IMAGE_BYTES
            || state.total_bytes.saturating_add(size) > GLOBAL_IMAGE_BYTES
        {
            return Err(crate::plugin::runtime::PluginError::Runtime(
                "插件图片资源配额已满".to_string(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        state.total_bytes += size;
        state.entries.insert(
            id.clone(),
            ImageEntry {
                session: session.to_string(),
                bytes: Arc::from(bytes),
            },
        );
        if let Some(key) = key {
            state.cache.insert(
                (identity.to_string(), session.to_string(), key),
                CachedRef {
                    id: id.clone(),
                    expires: Instant::now() + CACHE_TTL,
                },
            );
        }
        Ok(format!("flux-image:{id}"))
    }

    /// Read image bytes by the bare UUID portion (without the `flux-image:` prefix), only for its open session.
    #[must_use]
    pub fn read(&self, session: &str, id: &str) -> Option<Arc<[u8]>> {
        let state = self.lock();
        state.sessions.contains_key(session).then_some(())?;
        let entry = state.entries.get(id)?;
        (entry.session == session).then(|| Arc::clone(&entry.bytes))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn remove_where(state: &mut Registry, mut predicate: impl FnMut(&ImageEntry) -> bool) {
    let ids: Vec<String> = state
        .entries
        .iter()
        .filter(|(_, entry)| predicate(entry))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some(entry) = state.entries.remove(&id) {
            state.total_bytes = state.total_bytes.saturating_sub(entry.bytes.len());
        }
    }
    let live: HashSet<&str> = state.entries.keys().map(String::as_str).collect();
    state
        .cache
        .retain(|_, cached| live.contains(cached.id.as_str()));
}

fn options_key(opts: &ImageLoadOptions) -> Result<[u8; 32], crate::plugin::runtime::PluginError> {
    use sha2::Digest;
    let mut headers: Vec<_> = opts.headers.iter().collect();
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let encoded = serde_json::to_vec(&(
        opts.url.as_str(),
        headers,
        opts.auth_ref.as_deref(),
        opts.max_width,
        opts.max_height,
    ))
    .map_err(|e| {
        crate::plugin::runtime::PluginError::Runtime(format!("图片缓存键生成失败: {e}"))
    })?;
    Ok(sha2::Sha256::digest(encoded).into())
}

fn session_error() -> crate::plugin::runtime::PluginError {
    crate::plugin::runtime::PluginError::Runtime("插件图片会话已关闭或所有权不匹配".to_string())
}

/// Decode untrusted supported image bytes and produce a bounded, metadata-free PNG thumbnail.
pub async fn thumbnail(
    bytes: Vec<u8>,
    opts: &ImageLoadOptions,
) -> Result<Vec<u8>, crate::plugin::runtime::PluginError> {
    opts.validate()?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(image_error("图片超过 8 MiB 上限"));
    }
    let permit: OwnedSemaphorePermit = image_semaphore()
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| image_error("图片解码器已关闭"))?;
    let width = opts.max_width;
    let height = opts.max_height;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode_thumbnail(bytes, width, height)
    })
    .await
    .map_err(|e| image_error(&format!("图片处理任务失败: {e}")))?
}

static IMAGE_SEMAPHORE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(2)));

fn image_semaphore() -> &'static Arc<Semaphore> {
    &IMAGE_SEMAPHORE
}

fn decode_thumbnail(
    bytes: Vec<u8>,
    max_width: u32,
    max_height: u32,
) -> Result<Vec<u8>, crate::plugin::runtime::PluginError> {
    use image::{ImageFormat, ImageReader, imageops::FilterType};
    let format =
        image::guess_format(&bytes).map_err(|_| image_error("不支持或无法识别的图片格式"))?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(image_error("仅支持 PNG、JPEG、WebP 图片"));
    }
    let mut header_reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
    header_reader.limits(decoder_limits());
    let (width, height) = header_reader
        .into_dimensions()
        .map_err(|e| image_error(&format!("图片尺寸读取失败: {e}")))?;
    check_dimensions(width, height)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(decoder_limits());
    let image = reader
        .decode()
        .map_err(|e| image_error(&format!("图片解码失败: {e}")))?;
    let thumbnail = if width <= max_width && height <= max_height {
        image
    } else {
        let scale = (f64::from(max_width) / f64::from(width))
            .min(f64::from(max_height) / f64::from(height))
            .min(1.0);
        let out_width = (f64::from(width) * scale).floor().max(1.0) as u32;
        let out_height = (f64::from(height) * scale).floor().max(1.0) as u32;
        image.resize_exact(out_width, out_height, FilterType::Lanczos3)
    };
    let mut output = Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|e| image_error(&format!("PNG 编码失败: {e}")))?;
    let output = output.into_inner();
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(image_error("缩略图超过 4 MiB 上限"));
    }
    Ok(output)
}

fn decoder_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn check_dimensions(width: u32, height: u32) -> Result<(), crate::plugin::runtime::PluginError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || pixels > MAX_SOURCE_PIXELS
        || pixels.saturating_mul(4) > MAX_DECODE_BYTES
    {
        return Err(image_error("图片尺寸或解码内存超过限制"));
    }
    Ok(())
}

fn image_error(message: &str) -> crate::plugin::runtime::PluginError {
    crate::plugin::runtime::PluginError::InvalidOutput(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, ImageReader, Rgba, RgbaImage};

    fn options(cache: bool) -> ImageLoadOptions {
        ImageLoadOptions {
            url: "https://images.example/image.png".to_string(),
            headers: HashMap::new(),
            auth_ref: None,
            max_width: 512,
            max_height: 512,
            cache,
        }
    }

    fn raw_id(reference: &str) -> &str {
        reference.strip_prefix("flux-image:").unwrap()
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            width,
            height,
            Rgba([20, 40, 60, 255]),
        ));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Png).unwrap();
        output.into_inner()
    }

    #[tokio::test]
    async fn decodes_and_bounds_thumbnail_output() {
        let opts = options(false);
        let output = thumbnail(png(1200, 600), &opts).await.unwrap();
        assert_eq!(image::guess_format(&output).unwrap(), ImageFormat::Png);
        let dimensions = ImageReader::new(Cursor::new(output))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(dimensions, (512, 256));
    }

    #[test]
    fn rejects_pixel_and_dimension_overflow() {
        assert!(check_dimensions(8193, 1).is_err());
        assert!(check_dimensions(4096, 4096).is_err());
        assert!(check_dimensions(4000, 4000).is_ok());
    }

    #[test]
    fn cache_and_reference_ownership_follow_session() {
        let registry = ImageResources::new();
        registry.open_session("plugin-a", "session-a");
        registry.open_session("plugin-b", "session-b");
        let opts = options(true);
        let id = registry
            .insert("plugin-a", "session-a", &opts, vec![1, 2, 3])
            .unwrap();
        assert_eq!(
            registry
                .insert("plugin-a", "session-a", &opts, vec![4])
                .unwrap(),
            id
        );
        assert_eq!(
            registry.cached("plugin-a", "session-a", &opts).unwrap(),
            Some(id.clone())
        );
        let other = registry
            .insert("plugin-b", "session-b", &opts, vec![8])
            .unwrap();
        assert!(registry.read("session-b", raw_id(&id)).is_none());
        assert!(id.starts_with("flux-image:"));
        assert!(uuid::Uuid::parse_str(raw_id(&id)).is_ok());
        assert_eq!(
            registry.read("session-a", raw_id(&id)).unwrap().as_ref(),
            &[1, 2, 3]
        );
        registry.close_plugin("plugin-b");
        assert!(registry.read("session-b", raw_id(&other)).is_none());
        registry.close_session("session-a");
        assert!(registry.read("session-a", raw_id(&id)).is_none());
        assert!(
            registry
                .insert("plugin-a", "session-a", &opts, vec![1])
                .is_err()
        );
    }

    #[test]
    fn no_cache_skips_memoization_and_quota_never_evicts() {
        let registry = ImageResources::new();
        registry.open_session("plugin-a", "session-a");
        let opts = options(false);
        let first = registry
            .insert("plugin-a", "session-a", &opts, vec![1])
            .unwrap();
        let second = registry
            .insert("plugin-a", "session-a", &opts, vec![2])
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(
            registry.cached("plugin-a", "session-a", &opts).unwrap(),
            None
        );
        let cached_opts = options(true);
        for n in 0..SESSION_IMAGE_COUNT - 3 {
            let mut distinct = cached_opts.clone();
            distinct.url.push_str(&n.to_string());
            registry
                .insert("plugin-a", "session-a", &distinct, vec![n as u8])
                .unwrap();
        }
        let last_visible = registry
            .insert("plugin-a", "session-a", &cached_opts, vec![9])
            .unwrap();
        let mut extra = cached_opts.clone();
        extra.url.push_str("overflow");
        assert!(
            registry
                .insert("plugin-a", "session-a", &extra, vec![10])
                .is_err()
        );
        assert!(registry.read("session-a", raw_id(&last_visible)).is_some());
    }

    #[test]
    fn enforces_byte_quota_without_evicting_visible_images() {
        let registry = ImageResources::new();
        registry.open_session("plugin-a", "session-a");
        let opts = options(false);
        let mut ids = Vec::new();
        for index in 0..4 {
            let mut distinct = opts.clone();
            distinct.url.push_str(&index.to_string());
            ids.push(
                registry
                    .insert(
                        "plugin-a",
                        "session-a",
                        &distinct,
                        vec![index as u8; MAX_OUTPUT_BYTES],
                    )
                    .unwrap(),
            );
        }
        assert!(
            registry
                .insert("plugin-a", "session-a", &opts, vec![1])
                .is_err()
        );
        assert!(
            ids.iter()
                .all(|id| registry.read("session-a", raw_id(id)).is_some())
        );
    }
}
