//! 为 GPUI 桌面客户端嵌入 Windows 程序资源。

const WINDOWS_APP_ICON: &str = "../../assets/logo/windows/app_icon.ico";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={WINDOWS_APP_ICON}");
    println!("cargo:rerun-if-changed=../../native/protocol/build/windows_version_info.rs");
    println!("cargo:rerun-if-env-changed=FLUXDOWN_APP_VERSION");

    #[cfg(windows)]
    embed_windows_resources()?;

    Ok(())
}

#[cfg(windows)]
mod version_info {
    include!("../../native/protocol/build/windows_version_info.rs");
}

/// 资源 ID 1 是 Windows Explorer 与 GPUI Windows 后端共同读取的默认图标。
///
/// 后续运行时图标切换应覆盖窗口、任务栏和快捷方式引用；这里的默认资源始终
/// 保留，供未选择动态图标、重置图标及进程未运行时回退使用。
#[cfg(windows)]
fn embed_windows_resources() -> std::io::Result<()> {
    let mut resources = winresource::WindowsResource::new();
    resources.set_icon(WINDOWS_APP_ICON);
    version_info::apply_version_info(&mut resources, "FluxDown", "fluxdown-desktop.exe");
    resources.set("InternalName", "com.fluxdown.app");
    resources.compile()
}
