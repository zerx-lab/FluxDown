//! GPUI 设置能力页面与设置分区。
//!
//! 设置通过共享的翻译 Entity 和主题全局状态更新 UI，不依赖其他业务能力；
//! 全部读写经 [`SettingsPort`] 注入的单一 agent 会话；导入主题也存为（随云同步的）agent 偏好。

mod model;
mod port;
mod push_view;
mod search;
mod sections;
mod shortcut_display;
mod shortcut_record;
mod store;
mod theme_library;
mod ui;
pub mod update_button;
pub mod update_prompt;
pub mod update_view;
mod view;

pub use port::{PortFuture, SettingsPort};
pub use push_view::{PushHost, PushView, WindowAction};
pub use search::{SettingsSearchEntry, SettingsTarget, search_index};
pub use shortcut_display::{shortcut_label, shortcut_recording};
pub use store::{SettingsError, SettingsErrorKind, SettingsStore};
pub use theme_library::sync_theme_library;
pub use update_button::{OpenUpdatePrompt, UpdateButton};
pub use update_prompt::{
    UPDATE_PROMPT_WINDOW_HEIGHT, UPDATE_PROMPT_WINDOW_WIDTH, UpdatePromptEvent, UpdatePromptView,
};
pub use view::{
    ActivityBarToggle, SettingsContentSlots, SettingsView, SlotVisibility, VisibleSlot,
};

/// 打开分类编辑对话框（`id = None` 新建；未知 id 视为新建）。供 app 把下载侧栏的
/// 「编辑分类」接到设置能力，而不让下载 crate 依赖设置 crate。
pub fn open_category_editor(
    store: gpui::Entity<SettingsStore>,
    translator: &fluxdown_ui_i18n::Translator,
    id: Option<String>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let existing = id.and_then(|id| {
        sections::categories::read_categories(store.read(cx))
            .into_iter()
            .find(|entry| entry.id == id)
    });
    sections::category_dialog::open(store, translator.clone(), existing, window, cx);
}

/// 将 FluxDown locale 映射为 gpui-component 支持的 locale。
pub fn component_locale(locale: &str) -> &str {
    match locale {
        "zh" => "zh-CN",
        "zh-tw" => "zh-TW",
        _ => "en",
    }
}
