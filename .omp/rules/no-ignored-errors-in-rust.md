---
description: Rust 禁止 todo!/unimplemented! 与静默忽略错误（lint deny 级）
globs: "{native,crates}/**/*.rs"
---

根 `Cargo.toml` 的 `[workspace.lints]` 把以下 lint 设为 deny（所有 crate 经 `[lints] workspace = true` 继承）。`unused_must_use` 在普通编译时生效，其余由 Clippy 门禁拦截；不得只跑 `cargo check` 就宣称全部规则通过：

|lint|拦截的写法|
|---|---|
|`clippy::todo` / `clippy::unimplemented`|`todo!()` / `unimplemented!()` 占位|
|`clippy::let_underscore_must_use`|`let _ = fallible();` 丢弃 `Result` 等 `#[must_use]` 值|
|`clippy::let_underscore_future`|`let _ = async_fn();` 丢弃未 await 的 Future|
|`clippy::unused_result_ok`|`fallible().ok();` 把错误转成 `Option` 再丢弃|
|`unused_must_use`（rustc）|`fallible();` 裸语句丢弃 `Result`|

- 关键操作（文件写入 / 刷盘 / 断点与任务状态持久化 / 权限设置 / 回滚）必须传播错误或终止对应操作，不能记录后继续返回虚假成功；异步任务异常退出必须保留失败回执或状态通知，不能仅 `return` 而留下运行中状态；完成状态成功落库后才发完成帧，检查点不得超过成功刷盘的字节。
- 非关键清理保留首个实际错误；删除仅容忍明确预期的缺失（`NotFound`，以及名字不可能指向已有文件的 `InvalidFilename`），其余失败显式记录。日志初始化前或日志自身故障使用 stderr，避免递归写日志。
- 接收端关闭、GPUI 实体/窗口释放属于正常生命周期：明确停止后续操作或按已确认的错误类型处理；不将正常退出升级成错误、不输出敏感载荷、不在热路径反复记录同一关闭。
- 后台任务的 `JoinError` 区分预期取消与 panic；嵌套 `Result<Result<T, E>, JoinError>` 的两层错误都必须消费，不能只处理外层 panic 而丢弃任务自身错误。测试准备和后台 panic 必须失败，测试末尾尽力清理不能在已有 panic 的析构路径再 panic，需显式诊断非预期清理失败。
- 禁止忽错 lint 的 `allow`，也不得换成 `drop(Result)`、改名 `_ignored`、空错误分支或仅 `.is_err()` 判定后无实际处理来绕过门禁；禁止用 `-A` / `--allow` 或 `--cap-lints` 降低这些门禁。测试代码同样适用；`?`、有意义的控制流和显式诊断才是错误处理。
