//! 历史提问附图的缓存：按附件 ID 取回并解码一次，气泡里按 ID 直接读。
//!
//! 云端历史里的 `images` 是附件 ID（不是图片本身），取图经 `agent.assistant.attachment`。
//! 缓存只存状态，不做 IO；取回 / 解码由视图在后台任务里完成后写入。

use std::{collections::HashMap, sync::Arc};

use gpui::Image;

/// 一张历史附图的加载状态。
#[derive(Clone)]
pub(crate) enum ImageSlot {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

#[derive(Default)]
pub(crate) struct ImageCache {
    slots: HashMap<String, ImageSlot>,
}

impl ImageCache {
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

    pub(crate) fn finish(&mut self, id: String, image: Option<Arc<Image>>) {
        let slot = image.map_or(ImageSlot::Failed, ImageSlot::Ready);
        self.slots.insert(id, slot);
    }

    /// 账号变化：旧账号的附图与在途加载一并作废。
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests {
    use gpui::ImageFormat;

    use super::*;

    fn image() -> Arc<Image> {
        Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new()))
    }

    #[test]
    fn loading_and_ready_slots_are_not_requested_again() {
        let mut cache = ImageCache::default();
        assert!(cache.begin("a"));
        assert!(
            !cache.begin("a"),
            "loading slot must not start a second fetch"
        );
        cache.finish("a".to_owned(), Some(image()));
        assert!(!cache.begin("a"), "ready slot must not be fetched again");
        assert!(matches!(cache.get("a"), Some(ImageSlot::Ready(_))));
    }

    #[test]
    fn failed_slots_can_be_retried() {
        let mut cache = ImageCache::default();
        assert!(cache.begin("a"));
        cache.finish("a".to_owned(), None);
        assert!(matches!(cache.get("a"), Some(ImageSlot::Failed)));
        assert!(cache.begin("a"));
        assert!(matches!(cache.get("a"), Some(ImageSlot::Loading)));
    }

    #[test]
    fn clear_drops_every_slot() {
        let mut cache = ImageCache::default();
        cache.begin("a");
        cache.clear();
        assert!(cache.get("a").is_none());
    }
}
