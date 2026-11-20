//! Embed Windows PE version information into `fluxdown_nmh.exe`.
//!
//! A binary with a fully populated `VS_VERSION_INFO` resource (company,
//! product, description, version) looks far less suspicious to antivirus
//! heuristic engines than one with a blank version block. Everything here is
//! a no-op on non-Windows targets.

fn main() {
    println!("cargo:rerun-if-env-changed=FLUXDOWN_APP_VERSION");
    println!("cargo:rerun-if-changed=../protocol/build/windows_version_info.rs");
    #[cfg(windows)]
    embed_version_info();
}

#[cfg(windows)]
mod version_info {
    include!("../protocol/build/windows_version_info.rs");
}

/// Populate the standard `StringFileInfo` fields consumed by Explorer's
/// "Details" tab and by AV reputation heuristics.
#[cfg(windows)]
fn embed_version_info() {
    let mut res = winresource::WindowsResource::new();
    // Icon shared with the main application (path relative to this crate).
    res.set_icon("../../assets/logo/windows/app_icon.ico");
    version_info::apply_version_info(
        &mut res,
        "FluxDown Native Messaging Host (browser bridge)",
        "fluxdown_nmh.exe",
    );
    res.set("InternalName", "fluxdown_nmh");
    if let Err(e) = res.compile() {
        // Don't fail the build on resource-compiler issues; just warn so the
        // binary still links (only the version block is missing).
        println!("cargo:warning=fluxdown_nmh version resource failed: {e}");
    }
}
