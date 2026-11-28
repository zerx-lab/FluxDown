//! 应用级动作：窗口 / 设置 / 退出 / 更新 / 日志 / 关于。

use gpui::actions;

actions!(
    app,
    [
        OpenSettings,
        Quit,
        ShowMainWindow,
        CheckUpdate,
        OpenLogsFolder,
        OpenWebsite,
        About,
        /// 关闭当前活动窗口（走该窗口的关闭策略，与原生关闭按钮一致）。
        CloseWindow,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullScreen,
        /// macOS：隐藏本应用 / 隐藏其他 / 全部显示 / 前置全部窗口。
        Hide,
        HideOthers,
        ShowAll,
        BringAllToFront,
        /// 打开 / 关闭全局命令面板（⌘K / Ctrl+K）。
        ToggleCommandPalette,
        /// 开关主窗口右侧 AI 助手面板（云端未开放助手入口时无效）。
        ToggleAssistant
    ]
);
