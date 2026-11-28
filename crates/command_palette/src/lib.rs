//! 命令面板（⌘K / Ctrl+K）：模糊、拼音、英文别名匹配全局命令与设置项，按使用频次加权排序。
//!
//! 本 crate 只提供条目模型、匹配排序与面板视图；具体命令、设置索引与执行路由由 app 装配后以
//! [`PaletteItem`] 注入，使用记录经 [`PaletteConfig::on_usage`] 交还 app 持久化。

mod matcher;
mod strings;
mod usage;
mod view;

pub use usage::UsageStats;
pub use view::{
    PaletteConfig, PaletteItem, PaletteItemKind, PaletteQueryAction, UsageSink, bind_keys, close,
    is_open, open,
};
