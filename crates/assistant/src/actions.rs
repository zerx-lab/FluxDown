//! 助手面板的键盘动作。键位由 app 装配；本 crate 只定义动作并在 [`crate::AssistantView`]
//! 根元素上注册处理器。

use gpui::actions;

actions!(
    assistant,
    [
        /// 开新对话（焦点在助手面板内时由「新建下载」的键位触发）。
        NewChat,
    ]
);

/// 助手面板根元素的键位上下文名。
pub const KEY_CONTEXT: &str = "AssistantView";
