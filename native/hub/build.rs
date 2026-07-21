//! 门控插件（plugins）能力的编译期开关。
//!
//! 桌面（Windows/macOS/Linux）开启 `hub_plugins` cfg，源码里所有 plugin 相关代码随之编译；
//! 移动端（Android/iOS）关闭——`rquickjs-sys` 对这两个平台的 ABI 无预置 bindings，交叉编译必失败，
//! 且移动端暂不需要插件能力。`Cargo.toml` 里对 `fluxdown_engine`/`fluxdown_api` 的 `plugins`
//! feature 依赖分裂同样按 target_os 判定（rquickjs 是否被引入），与本文件保持同源。
//!
//! 本地可用 `HUB_FORCE_NO_PLUGINS=1 cargo check -p hub` 在桌面 host 上验证「关闭 plugins」分支
//! （移动端源码路径），无需 Android NDK。
//!
//! 未来移动端若要支持插件，在 `Cargo.toml` 基线补上 `plugins` feature，并让本文件对 android/ios
//! 也开启 `hub_plugins` 即可。

fn main() {
    println!("cargo::rerun-if-env-changed=HUB_FORCE_NO_PLUGINS");
    println!("cargo::rustc-check-cfg=cfg(hub_plugins)");
    println!("cargo::rustc-check-cfg=cfg(hub_link)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let is_mobile = target_os == "android" || target_os == "ios";
    let forced_off = std::env::var("HUB_FORCE_NO_PLUGINS").is_ok();

    if !is_mobile && !forced_off {
        println!("cargo::rustc-cfg=hub_plugins");
    }
    // 本地设备互联：桌面开启（mobile 关闭——mDNS 需原生权限，且引擎 link feature
    // 亦仅对桌面开启，见 hub/Cargo.toml target 依赖分裂）。
    if !is_mobile {
        println!("cargo::rustc-cfg=hub_link");
    }
}
