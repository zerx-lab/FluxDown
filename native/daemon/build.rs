//! 为 `fluxdownd.exe` 写入 Windows PE 版本信息（Explorer「文件版本 / 产品版本」）。
//! 非 Windows 目标为空操作。

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=FLUXDOWN_APP_VERSION");
    println!("cargo::rerun-if-changed=../protocol/build/windows_version_info.rs");
    #[cfg(windows)]
    embed_windows_version_info();
}

#[cfg(windows)]
mod version_info {
    include!("../protocol/build/windows_version_info.rs");
}

#[cfg(windows)]
fn embed_windows_version_info() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/logo/windows/app_icon.ico");
    version_info::apply_version_info(&mut res, "FluxDown Download Service", "fluxdownd.exe");
    res.set("InternalName", "fluxdownd");
    if let Err(e) = res.compile() {
        println!("cargo::warning=fluxdownd version resource failed: {e}");
    }
}
