//! 助手面板的键盘动作。键位由 app 装配;本 crate 只定义动作并在 [`crate::AssistantView`] 上注册处理器。

use gpui::actions;

actions!(
    assistant,
    [
        /// 开新对话(焦点在助手面板内时由「新建下载」的键位触发)。
        NewChat,
        /// Esc:依次取消待确认的删除 / 清空 / 重命名 / 编辑重发,其次停止进行中的回答,最后关闭历史视图。
        Dismiss,
        /// 历史视图:光标上移一项。
        HistoryPrev,
        /// 历史视图:光标下移一项。
        HistoryNext,
        /// 历史视图:打开光标所在的会话。
        HistoryOpen,
    ]
);

/// 助手面板内焦点所在的键位上下文。
pub const KEY_CONTEXT: &str = "AssistantView";

/// 历史视图(会话列表)获得焦点时的键位上下文:上下键移动、回车打开。
pub const HISTORY_KEY_CONTEXT: &str = "AssistantHistory";
