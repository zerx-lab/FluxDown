//! 历史提问附图的缓存：按附件 ID 取回并解码一次，气泡里按 ID 直接读。
//!
//! 云端历史里的 `images` 是附件 ID（不是图片本身），取图经 `agent.assistant.attachment`。
//! 缓存只存状态，不做 IO；取回 / 解码由视图在后台任务里完成后写入。每张存缩略图与原图（大图预览用），
//! 按两者已编码字节总量设上限（与 Web 同口径，约 64 MB），超出时淘汰最早写入、且不在当前时间线里的那些。

use std::collections::{HashMap, HashSet, VecDeque};

use crate::attachment::LocalImage;

/// 缓存里已解码图片的字节总量上限。
pub(crate) const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// 一张历史附图的加载状态。
#[derive(Clone)]
pub(crate) enum ImageSlot {
    Loading,
    Ready(LocalImage),
    Failed,
}

pub(crate) struct ImageCache {
    slots: HashMap<String, ImageSlot>,
    /// 已就绪图片的写入顺序（最早的在前）：淘汰按它走。
    order: VecDeque<String>,
    /// 已就绪图片的字节总量。
    bytes: usize,
    limit: usize,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::with_limit(MAX_CACHE_BYTES)
    }
}

impl ImageCache {
    fn with_limit(limit: usize) -> Self {
        Self {
            slots: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<&ImageSlot> {
        self.slots.get(id)
    }

    /// 登记开始加载。已在加载或已完成返回 `false`；失败过的允许重试。
    pub(crate) fn begin(&mut self, id: &str) -> bool {
        if matches!(
            self.slots.get(id),
            Some(ImageSlot::Loading | ImageSlot::Ready(_))
        ) {
            return false;
        }
        self.slots.insert(id.to_owned(), ImageSlot::Loading);
        true
    }

    /// 写入加载结果。超出字节上限时淘汰最早写入的就绪图片，`keep`（当前时间线仍要显示的附件 ID）
    /// 与刚写入的这张不淘汰。
    pub(crate) fn finish(&mut self, id: String, image: Option<LocalImage>, keep: &HashSet<String>) {
        self.forget_ready(&id);
        match image {
            Some(image) => {
                self.bytes += image.byte_len();
                self.order.push_back(id.clone());
                self.slots.insert(id.clone(), ImageSlot::Ready(image));
            }
            None => {
                self.slots.insert(id.clone(), ImageSlot::Failed);
            }
        }
        while self.bytes > self.limit {
            let Some(position) = self
                .order
                .iter()
                .position(|candidate| *candidate != id && !keep.contains(candidate))
            else {
                break;
            };
            if let Some(evicted) = self.order.remove(position) {
                self.forget_ready(&evicted);
                self.slots.remove(&evicted);
            }
        }
    }

    /// 账号变化：旧账号的附图与在途加载一并作废。
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// 从字节统计与顺序里去掉一张已就绪的图（槽位本身由调用方处理）。
    fn forget_ready(&mut self, id: &str) {
        if let Some(ImageSlot::Ready(old)) = self.slots.get(id) {
            self.bytes = self.bytes.saturating_sub(old.byte_len());
            self.order.retain(|queued| queued != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Image, ImageFormat};

    use super::*;

    fn image() -> LocalImage {
        sized(0)
    }

    fn sized(len: usize) -> LocalImage {
        let full = Arc::new(Image::from_bytes(ImageFormat::Png, vec![0; len]));
        LocalImage {
            thumbnail: Arc::clone(&full),
            full,
        }
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn loading_and_ready_slots_are_not_requested_again() {
        let mut cache = ImageCache::default();
        assert!(cache.begin("a"));
        assert!(
            !cache.begin("a"),
            "loading slot must not start a second fetch"
        );
        cache.finish("a".to_owned(), Some(image()), &none());
        assert!(!cache.begin("a"), "ready slot must not be fetched again");
        assert!(matches!(cache.get("a"), Some(ImageSlot::Ready(_))));
    }

    #[test]
    fn failed_slots_can_be_retried() {
        let mut cache = ImageCache::default();
        assert!(cache.begin("a"));
        cache.finish("a".to_owned(), None, &none());
        assert!(matches!(cache.get("a"), Some(ImageSlot::Failed)));
        assert!(cache.begin("a"));
        assert!(matches!(cache.get("a"), Some(ImageSlot::Loading)));
    }

    #[test]
    fn clear_drops_every_slot() {
        let mut cache = ImageCache::default();
        cache.begin("a");
        cache.finish("b".to_owned(), Some(sized(10)), &none());
        cache.clear();
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_none());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn oldest_ready_images_are_evicted_over_the_byte_limit() {
        let mut cache = ImageCache::with_limit(100);
        cache.finish("a".to_owned(), Some(sized(40)), &none());
        cache.finish("b".to_owned(), Some(sized(40)), &none());
        cache.finish("c".to_owned(), Some(sized(40)), &none());
        assert!(cache.get("a").is_none(), "oldest must be evicted first");
        assert!(matches!(cache.get("b"), Some(ImageSlot::Ready(_))));
        assert!(matches!(cache.get("c"), Some(ImageSlot::Ready(_))));
        assert_eq!(cache.bytes, 80);
        // 被淘汰的允许重新取回。
        assert!(cache.begin("a"));
    }

    #[test]
    fn images_in_the_timeline_and_the_new_one_are_never_evicted() {
        let mut cache = ImageCache::with_limit(100);
        let keep: HashSet<String> = ["a".to_owned()].into();
        cache.finish("a".to_owned(), Some(sized(60)), &keep);
        cache.finish("b".to_owned(), Some(sized(60)), &keep);
        assert!(matches!(cache.get("a"), Some(ImageSlot::Ready(_))));
        assert!(matches!(cache.get("b"), Some(ImageSlot::Ready(_))));
        // 单张就超限：保留它自己，不死循环。
        let mut cache = ImageCache::with_limit(10);
        cache.finish("big".to_owned(), Some(sized(50)), &none());
        assert!(matches!(cache.get("big"), Some(ImageSlot::Ready(_))));
    }

    #[test]
    fn rewriting_a_slot_does_not_double_count_bytes() {
        let mut cache = ImageCache::with_limit(100);
        cache.finish("a".to_owned(), Some(sized(30)), &none());
        cache.finish("a".to_owned(), Some(sized(20)), &none());
        assert_eq!(cache.bytes, 20);
        cache.finish("a".to_owned(), None, &none());
        assert_eq!(cache.bytes, 0);
        assert!(matches!(cache.get("a"), Some(ImageSlot::Failed)));
    }
}
