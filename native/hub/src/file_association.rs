//! Windows `.torrent` file association via HKCU registry.
//!
//! Registry structure (matches the Inno Setup installer):
//! ```text
//! HKCU\Software\Classes\.torrent                               → "FluxDown.TorrentFile"
//! HKCU\Software\Classes\FluxDown.TorrentFile                   → "BitTorrent File"
//! HKCU\Software\Classes\FluxDown.TorrentFile\DefaultIcon       → "<exe>,0"
//! HKCU\Software\Classes\FluxDown.TorrentFile\shell\open\command → "\"<exe>\" \"%1\""
//! ```
//!
//! All operations target `HKEY_CURRENT_USER` — no admin elevation required.

#[cfg(target_os = "windows")]
mod inner {
    use crate::logger::log_info;
    use std::io;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    const PROG_ID: &str = "FluxDown.TorrentFile";
    const PROG_DESC: &str = "BitTorrent File";
    const EXT: &str = ".torrent";

    /// Get the canonical path of the current running executable.
    ///
    /// Uses `std::fs::canonicalize` to resolve symlinks and `\\?\` prefixes,
    /// then strips the `\\?\` prefix (if any) for clean comparison with
    /// registry values written by `associate()`.
    fn exe_path() -> Result<String, io::Error> {
        let path = std::env::current_exe()?;
        // canonicalize resolves symlinks and normalizes the path, but on
        // Windows it may add a `\\?\` extended-length prefix.
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        let s = canonical.to_string_lossy().into_owned();
        // Strip the extended-length prefix for clean registry comparison.
        Ok(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
    }

    /// Check whether `.torrent` files are currently associated with FluxDown.
    ///
    /// Returns `true` if `HKCU\Software\Classes\.torrent` default value
    /// equals `"FluxDown.TorrentFile"`. We intentionally do NOT compare the
    /// exe path in the command, because path representations can differ
    /// between the installer and the running process (UNC prefix, casing,
    /// short names, etc.). Checking the ProgID alone is sufficient to
    /// confirm FluxDown owns the association.
    pub fn is_associated() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Check .torrent → FluxDown.TorrentFile
        let ext_key =
            match hkcu.open_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_READ) {
                Ok(k) => k,
                Err(_) => return false,
            };
        let prog_id: String = match ext_key.get_value("") {
            Ok(v) => v,
            Err(_) => return false,
        };
        prog_id == PROG_ID
    }

    /// Register `.torrent` file association with FluxDown.
    pub fn associate() -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = exe_path()?;

        // 1. .torrent → FluxDown.TorrentFile
        let (ext_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_WRITE)?;
        ext_key.set_value("", &PROG_ID)?;

        // 2. FluxDown.TorrentFile description
        let (prog_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{PROG_ID}"), KEY_WRITE)?;
        prog_key.set_value("", &PROG_DESC)?;

        // 3. DefaultIcon
        let (icon_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\DefaultIcon"),
            KEY_WRITE,
        )?;
        icon_key.set_value("", &format!("\"{exe}\",0"))?;

        // 4. shell\open\command
        let (cmd_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\shell\\open\\command"),
            KEY_WRITE,
        )?;
        cmd_key.set_value("", &format!("\"{exe}\" \"%1\""))?;

        // Notify the shell about the change
        notify_shell();

        log_info!("[file_assoc] associated .torrent with FluxDown (exe={exe})");
        Ok(())
    }

    /// Remove `.torrent` file association for FluxDown.
    pub fn disassociate() -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Only remove if currently associated to us (don't break other app's association)
        if !is_associated() {
            log_info!("[file_assoc] not associated to FluxDown, skipping removal");
            return Ok(());
        }

        // Remove .torrent key
        let classes = hkcu.open_subkey_with_flags("Software\\Classes", KEY_WRITE)?;
        let _ = classes.delete_subkey_all(EXT);

        // Remove FluxDown.TorrentFile tree
        let _ = classes.delete_subkey_all(PROG_ID);

        // Notify the shell about the change
        notify_shell();

        log_info!("[file_assoc] removed .torrent association");
        Ok(())
    }

    /// Call SHChangeNotify to inform Explorer about file association changes.
    ///
    /// Uses raw FFI to avoid pulling in the `Win32_UI_Shell` feature gate
    /// of `windows-sys`.
    fn notify_shell() {
        // SHCNE_ASSOCCHANGED = 0x08000000, SHCNF_IDLIST = 0x0000
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHChangeNotify(
                wEventId: i32,
                uFlags: u32,
                dwItem1: *const std::ffi::c_void,
                dwItem2: *const std::ffi::c_void,
            );
        }
        unsafe {
            SHChangeNotify(0x08000000, 0, std::ptr::null(), std::ptr::null());
        }
    }
}

// Linux implementation — uses xdg-mime to query / set the default handler.
#[cfg(target_os = "linux")]
mod inner {
    use std::io;

    /// Check whether `.torrent` files are currently associated with FluxDown.
    ///
    /// Queries `xdg-mime query default application/x-bittorrent` and checks
    /// whether the returned .desktop name contains "fluxdown".
    pub fn is_associated() -> bool {
        let Ok(output) = std::process::Command::new("xdg-mime")
            .args(["query", "default", "application/x-bittorrent"])
            .output()
        else {
            return false;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.to_lowercase().contains("fluxdown")
    }

    /// Register FluxDown as the default handler for `.torrent` files.
    ///
    /// Requires that `com.fluxdown.app.desktop` is already installed in an
    /// XDG applications directory (handled by the package installer).
    pub fn associate() -> Result<(), io::Error> {
        std::process::Command::new("xdg-mime")
            .args([
                "default",
                "com.fluxdown.app.desktop",
                "application/x-bittorrent",
            ])
            .status()
            .map(|_| ())
    }

    /// Remove FluxDown as the default handler for `.torrent` files by
    /// delegating back to the system default (empty the user override).
    ///
    /// xdg-mime has no "unset" command, so we edit `mimeapps.list` directly:
    /// remove the `application/x-bittorrent=com.fluxdown.app.desktop` line
    /// from the `[Default Applications]` section.
    pub fn disassociate() -> Result<(), io::Error> {
        use std::io::{BufRead, Write};

        // Locate ~/.config/mimeapps.list (XDG spec default).
        let config_dir = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.config")
        });
        let path = std::path::PathBuf::from(&config_dir).join("mimeapps.list");

        if !path.exists() {
            return Ok(());
        }

        let file = std::fs::File::open(&path)?;
        let lines: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .collect::<Result<_, _>>()?;

        // Remove lines that set application/x-bittorrent to us.
        let filtered: Vec<&str> = lines
            .iter()
            .filter(|l| {
                let lower = l.to_lowercase();
                !(lower.starts_with("application/x-bittorrent=") && lower.contains("fluxdown"))
            })
            .map(|l| l.as_str())
            .collect();

        let mut out = std::fs::File::create(&path)?;
        for line in filtered {
            writeln!(out, "{line}")?;
        }
        Ok(())
    }
}

// macOS implementation — uses Launch Services to query / set the default
// handler for the `.torrent` UTI (`org.bittorrent.torrent`, declared in
// `macos/Runner/Info.plist` via CFBundleDocumentTypes + UTImportedTypeDeclarations).
#[cfg(target_os = "macos")]
mod inner {
    use crate::logger::log_info;
    use std::ffi::{CString, c_char, c_void};
    use std::io;

    /// The `.torrent` uniform type identifier declared in Info.plist.
    const TORRENT_UTI: &str = "org.bittorrent.torrent";
    /// `kLSRolesAll` — match any role (viewer/editor/shell).
    const LS_ROLES_ALL: u32 = 0xFFFF_FFFF;
    /// `kCFStringEncodingUTF8`.
    const CF_ENCODING_UTF8: u32 = 0x0800_0100;

    type CFStringRef = *const c_void;
    type CFBundleRef = *const c_void;
    type CFAllocatorRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetCStringPtr(the_string: CFStringRef, encoding: u32) -> *const c_char;
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(cf: *const c_void);
        fn CFBundleGetMainBundle() -> CFBundleRef;
        fn CFBundleGetIdentifier(bundle: CFBundleRef) -> CFStringRef;
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler_bundle_id: CFStringRef,
        ) -> i32;
    }

    /// RAII guard that releases a Core Foundation reference on drop.
    ///
    /// Only wraps references we own (returned from `Create`/`Copy` functions).
    /// A null pointer is treated as "nothing to release".
    struct CfOwned(CFStringRef);

    impl Drop for CfOwned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is a non-null CF reference we own (obtained
                // from a Create/Copy call), released exactly once here.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// Create an owned `CFString` from a Rust `&str`.
    fn cf_string(s: &str) -> Result<CfOwned, io::Error> {
        let c = CString::new(s).map_err(|_| io::Error::other("string contains interior NUL"))?;
        // SAFETY: `c` is a valid NUL-terminated C string that outlives the call;
        // the default allocator (null) copies the bytes into the new CFString.
        let cf =
            unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_ENCODING_UTF8) };
        if cf.is_null() {
            return Err(io::Error::other("CFStringCreateWithCString failed"));
        }
        Ok(CfOwned(cf))
    }

    /// Convert a borrowed (non-owned) `CFStringRef` to a Rust `String`.
    fn cf_to_string(cf: CFStringRef) -> Option<String> {
        if cf.is_null() {
            return None;
        }
        // Fast path: some CFStrings expose their UTF-8 buffer directly.
        // SAFETY: `cf` is a valid CFStringRef; the returned pointer, if
        // non-null, is owned by `cf` and valid for the lifetime of `cf`.
        let ptr = unsafe { CFStringGetCStringPtr(cf, CF_ENCODING_UTF8) };
        if !ptr.is_null() {
            // SAFETY: `ptr` is a valid NUL-terminated UTF-8 buffer owned by `cf`.
            return unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_str()
                .ok()
                .map(str::to_owned);
        }
        // Slow path: copy into a local buffer (bundle ids are short).
        let mut buf = [0_i8; 512];
        // SAFETY: `buf` is a valid writable buffer of `buf.len()` bytes; the
        // function NUL-terminates within that bound on success. Returns a CF
        // `Boolean` (u8): non-zero means the whole string was written.
        let ok = unsafe {
            CFStringGetCString(
                cf,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len() as isize,
                CF_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: on success the buffer holds a NUL-terminated C string.
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr().cast::<c_char>()) }
            .to_str()
            .ok()
            .map(str::to_owned)
    }

    /// Return this app's bundle identifier (e.g. `dev.zerx.fluxdown`).
    fn main_bundle_id() -> Option<String> {
        // SAFETY: `CFBundleGetMainBundle` returns a borrowed (non-owned) ref or
        // null; `CFBundleGetIdentifier` likewise returns a borrowed ref — neither
        // is released here.
        let id = unsafe {
            let bundle = CFBundleGetMainBundle();
            if bundle.is_null() {
                return None;
            }
            CFBundleGetIdentifier(bundle)
        };
        cf_to_string(id)
    }

    /// Check whether `.torrent` files are currently associated with FluxDown.
    ///
    /// Queries the default role handler for the torrent UTI and compares its
    /// bundle id (case-insensitively) with this app's bundle id.
    pub fn is_associated() -> bool {
        let Ok(uti) = cf_string(TORRENT_UTI) else {
            return false;
        };
        // SAFETY: `uti.0` is a valid CFStringRef; the returned handler ref is
        // owned by us and released via the `CfOwned` guard below.
        let handler =
            CfOwned(unsafe { LSCopyDefaultRoleHandlerForContentType(uti.0, LS_ROLES_ALL) });
        let Some(handler_id) = cf_to_string(handler.0) else {
            return false;
        };
        match main_bundle_id() {
            Some(mine) => handler_id.eq_ignore_ascii_case(&mine),
            None => false,
        }
    }

    /// Register FluxDown as the default handler for `.torrent` files.
    ///
    /// The app must already be registered with Launch Services (which happens
    /// automatically the first time the bundle — declaring the UTI in
    /// Info.plist — is scanned or launched by the system).
    pub fn associate() -> Result<(), io::Error> {
        let bundle_id =
            main_bundle_id().ok_or_else(|| io::Error::other("main bundle id unavailable"))?;
        let uti = cf_string(TORRENT_UTI)?;
        let id = cf_string(&bundle_id)?;
        // SAFETY: both `uti.0` and `id.0` are valid CFStringRefs alive for the
        // duration of the call; the function does not take ownership of them.
        let status = unsafe { LSSetDefaultRoleHandlerForContentType(uti.0, LS_ROLES_ALL, id.0) };
        if status != 0 {
            return Err(io::Error::other(format!(
                "LSSetDefaultRoleHandlerForContentType failed (OSStatus={status})"
            )));
        }
        log_info!("[file_assoc] associated .torrent with FluxDown (bundle={bundle_id})");
        Ok(())
    }

    /// Remove FluxDown as the default handler for `.torrent` files.
    ///
    /// Launch Services has no "unset" primitive; setting the handler to an empty
    /// bundle id hands the type back to the system default. Only acts if we
    /// currently own the association (don't clobber another app's choice).
    pub fn disassociate() -> Result<(), io::Error> {
        if !is_associated() {
            log_info!("[file_assoc] not associated to FluxDown, skipping removal");
            return Ok(());
        }
        let uti = cf_string(TORRENT_UTI)?;
        let empty = cf_string("")?;
        // SAFETY: `uti.0` and `empty.0` are valid CFStringRefs alive for the call.
        let status = unsafe { LSSetDefaultRoleHandlerForContentType(uti.0, LS_ROLES_ALL, empty.0) };
        if status != 0 {
            return Err(io::Error::other(format!(
                "LSSetDefaultRoleHandlerForContentType (clear) failed (OSStatus={status})"
            )));
        }
        log_info!("[file_assoc] removed .torrent association");
        Ok(())
    }
}

// Fallback stubs for platforms without a native implementation.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod inner {
    use std::io;

    pub fn is_associated() -> bool {
        false
    }

    pub fn associate() -> Result<(), io::Error> {
        Ok(())
    }

    pub fn disassociate() -> Result<(), io::Error> {
        Ok(())
    }
}

pub use inner::{associate, disassociate, is_associated};
