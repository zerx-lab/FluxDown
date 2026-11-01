---
description: 根 Cargo.toml 的 GPUI / gpui-kit / quickjs-jit 版本对齐、[patch.crates-io]（librqbit 系）与 release profile 的编译期陷阱
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/Cargo.toml'
  - '**/Cargo.lock'
  - '**/third_party/**'
interruptMode: never
---

你正在改 Cargo 清单 / 锁文件 / 第三方补丁。以下补丁与配置牵一发动全身，升级或删改前逐条核对：

- **QuickJS = Longbridge `quickjs-jit`**：gpui-shell 依赖 crates.io `quickjs-jit`（rquickjs API，`=` 精确版本）；engine 以 `rquickjs = { package = "quickjs-jit", … }` 引用同一版本，全工作区一份 VM。升级 gpui-kit 时 engine 的 `quickjs-jit` 跟到 gpui-shell 的同一版本，并重跑 `cargo nextest run -p fluxdown_engine --features plugins,components plugin`。engine 侧禁止叠加 `rust-alloc`/`allocator`（会让 `set_memory_limit` 静默失效）；必带 `parallel`（`AsyncRuntime`/`AsyncContext` 的 Send/Sync 依赖它）。gpui-shell 的 LLRT crypto 对 `rsa` 预发布版敏感，`Cargo.lock` 中 `rsa` / `pkcs1` 须与 gpui-kit 该发布版锁文件同版本（`cargo update -p rsa --precise …`）。
- **GPUI 核心 = gpui-fast（crates.io 发布版）**：`gpui` / `gpui_platform` 以 `package = "gpui-fast{,-platform}"` 引用；gpui-kit 组件（`gpui-base` / `gpui-component` / `gpui-kit-assets`）与 `gpui-shell` 取 crates.io **同一发布版**并全部开 `gpui-fast` feature（漏开任一包会让依赖图里出现两套不兼容的 GPUI 类型）。升级时 gpui-fast 照抄该 gpui-kit 发布 tag 的 `Cargo.toml` 中 `gpui-fast` 版本；gpui-kit 非可选的 `gpui-pre` 依赖仍会编进依赖图但不被使用（官方支持配置）。只认已发布的 release，不钉 main commit。gpui-shell 版本变化（含补丁版）会改变插件桌面类型目录：同回合重跑 `cargo run -p fluxdown_ui_plugin_ui --example gen_plugin_typings -- packages/plugin-sdk/types/desktop.d.ts`，否则 CI「plugin desktop typings drift」失败。渲染语义见 `.omp/knowledge/clients.md`「GPUI 核心 = gpui-fast」。
- **iOS 依赖 `third_party/librqbit-dualstack-sockets`**：上游 0.7.0 在 iOS 上不编译（Apple 平台只放行 macOS 的按索引绑定）。删除补丁前先确认上游已修，并重跑 `mobile/FluxDown/scripts/build-core.sh`。
- **`libc` 钉 `=0.2.189`（`native/engine/Cargo.toml`，全工作区经此统一）**：引擎 `disk_space.rs` 直接用 `libc::statfs`；0.2.190 起 Apple 路由 / in6 符号收窄到仅 macOS，iOS 目标上依赖这些符号的传递依赖会编译失败。不要 `cargo update` 掉它；放开前过 CI `ios-core`（`cargo check -p fluxdown_mobile --lib --target aarch64-apple-ios`，含 `--no-default-features`）。
- **`profile.release` 不设 `panic="abort"`**：`download_manager` 靠 `catch_unwind` 恢复 task panic。
- 新增 crate / path 依赖 / `[patch]` 路径后跑 `python3 .github/scripts/check_ci_paths.py`。
