// Shared by the Windows build scripts of fluxdown-desktop, fluxdown-agent,
// fluxdownd and fluxdown_nmh (`include!`d inside a `#[cfg(windows)]` module,
// never compiled into `fluxdown_protocol` itself) so every shipped `.exe`
// carries the same product version block.
//
// Explorer's hover tooltip and the "Details" tab read the numeric
// `VS_FIXEDFILEINFO` version, which winresource otherwise derives from the
// crate's `CARGO_PKG_VERSION` (`0.1.0`) instead of the release tag.

/// Product version: `FLUXDOWN_APP_VERSION` from the release pipeline, falling
/// back to the crate version for local builds.
pub fn app_version() -> String {
    std::env::var("FLUXDOWN_APP_VERSION")
        .ok()
        .map(|version| version.trim().trim_start_matches('v').to_owned())
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

/// Packs `MAJOR.MINOR.PATCH[-pre.N]` into the four 16-bit words Windows uses.
///
/// The fourth word is the trailing prerelease number (`0.6.0-rc.2` →
/// `0.6.0.2`) and `0` for stable releases; unparsable parts become `0`.
pub fn numeric_version(version: &str) -> u64 {
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let core = core.split_once('+').map_or(core, |(core, _)| core);
    let mut parts = core.split('.').map(|part| part.parse::<u16>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let build = pre
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse::<u16>().ok())
        .unwrap_or(0);
    (u64::from(major) << 48)
        | (u64::from(minor) << 32)
        | (u64::from(patch) << 16)
        | u64::from(build)
}

/// Applies the common FluxDown version/company strings to `res`.
pub fn apply_version_info(
    res: &mut winresource::WindowsResource,
    description: &str,
    original_filename: &str,
) {
    let version = app_version();
    let numeric = numeric_version(&version);
    res.set("CompanyName", "FluxDown");
    res.set("ProductName", "FluxDown");
    res.set("FileDescription", description);
    res.set("OriginalFilename", original_filename);
    res.set(
        "LegalCopyright",
        "Copyright (C) 2026 FluxDown. All rights reserved.",
    );
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, numeric);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, numeric);
}
