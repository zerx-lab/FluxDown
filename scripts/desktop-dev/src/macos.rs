use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const HELPER: &str = "Contents/Helpers/FluxDownAgent.app";
const BINARIES: &[(&str, &str)] = &[
    ("fluxdown-desktop", "Contents/MacOS/fluxdown-desktop"),
    (
        "fluxdown-agent",
        "Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown-agent",
    ),
    (
        "fluxdownd",
        "Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdownd",
    ),
    (
        "fluxdown_nmh",
        "Contents/Helpers/FluxDownAgent.app/Contents/MacOS/fluxdown_nmh",
    ),
];

pub(super) fn assemble(root: &Path, output: &Path) -> io::Result<PathBuf> {
    // Keep previous generations intact: a resident agent can outlive its UI.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let stage = output
        .join("desktop-dev")
        .join(format!("{}-{nonce}", std::process::id()));
    fs::create_dir_all(&stage)?;
    let app = stage.join("FluxDown.app");
    let helper = app.join(HELPER);
    for bundle in [&app, &helper] {
        fs::create_dir_all(bundle.join("Contents/MacOS"))?;
        fs::create_dir_all(bundle.join("Contents/Resources"))?;
    }
    for (binary, relative) in BINARIES {
        // Never symlink: NSBundle must identify the bundle, not the Cargo target.
        fs::copy(output.join(binary), app.join(relative))?;
    }
    fs::write(app.join("Contents/Info.plist"), plist(false))?;
    fs::write(helper.join("Contents/Info.plist"), plist(true))?;
    let iconset = stage.join("AppIcon.iconset");
    fs::create_dir(&iconset)?;
    for (size, name) in [
        (16, "16x16"),
        (32, "16x16@2x"),
        (32, "32x32"),
        (64, "32x32@2x"),
        (128, "128x128"),
        (256, "128x128@2x"),
        (256, "256x256"),
        (512, "256x256@2x"),
        (512, "512x512"),
        (1024, "512x512@2x"),
    ] {
        fs::copy(
            root.join(format!("assets/logo/macos/app_icon_{size}.png")),
            iconset.join(format!("icon_{name}.png")),
        )?;
    }
    checked(
        Command::new("/usr/bin/iconutil")
            .arg("-c")
            .arg("icns")
            .arg(&iconset)
            .arg("-o")
            .arg(app.join("Contents/Resources/AppIcon.icns")),
    )?;
    fs::copy(
        app.join("Contents/Resources/AppIcon.icns"),
        helper.join("Contents/Resources/AppIcon.icns"),
    )?;
    sign_bundles(&app, &helper)?;
    eprintln!("desktop-dev: signed development bundle: {}", app.display());
    Ok(app.join(BINARIES[0].1))
}

fn sign_bundles(app: &Path, helper: &Path) -> io::Result<()> {
    for bundle in [helper, app] {
        checked(
            Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(bundle.join("Contents/Info.plist")),
        )?;
    }
    for (binary, id) in [
        ("fluxdownd", "com.fluxdown.app.daemon"),
        ("fluxdown_nmh", "com.fluxdown.app.nmh"),
    ] {
        checked(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--identifier", id])
                .arg(helper.join("Contents/MacOS").join(binary)),
        )?;
    }
    // Signing a bundle signs its main executable with the plist identity.
    for bundle in [helper, app] {
        checked(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(bundle),
        )?;
    }
    checked(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app),
    )
}

pub(super) fn configure_launch(desktop: &Path, launch: &mut Command) -> io::Result<()> {
    let contents = desktop
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("development desktop has no Contents directory"))?;
    let helper = contents.join("Helpers/FluxDownAgent.app");
    // Register only this helper, without changing URL/file defaults or launching it.
    checked(Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f").arg(&helper))?;
    // LaunchServices does not reliably forward shell env (notably data-dir overrides).
    // Direct spawn still supplies a real main NSBundle and lets tao own the main thread.
    if std::env::var_os("FLUXDOWN_AGENT_BIN").is_none() {
        launch.env(
            "FLUXDOWN_AGENT_BIN",
            helper.join("Contents/MacOS/fluxdown-agent"),
        );
    }
    Ok(())
}

fn checked(command: &mut Command) -> io::Result<()> {
    super::no_console_window(command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{command:?} failed: {status}")))
    }
}

/// 本地网络隐私（macOS 15+）的授权说明：两个 bundle 都声明，daemon 由 agent 拉起、归属 helper。
/// 与 `packaging/macos/Info.plist`、`scripts/package_gpui_macos.sh` 的文案一致。
const LOCAL_NETWORK_USAGE: &str = "FluxDown uses your local network to forward BT and eD2K ports on your router (UPnP / NAT-PMP) and to connect to paired devices and download hosts.";

fn plist(helper: bool) -> String {
    let (id, executable) = if helper {
        ("com.fluxdown.app.agent", "fluxdown-agent")
    } else {
        ("com.fluxdown.app", "fluxdown-desktop")
    };
    let background = if helper {
        "<key>LSUIElement</key><true/>"
    } else {
        ""
    };
    // Deliberately omit document/URL handlers: dev staging must not claim defaults.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleExecutable</key><string>{executable}</string>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleName</key><string>FluxDown</string>
<key>CFBundleDisplayName</key><string>FluxDown</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.0.0</string>
<key>CFBundleVersion</key><string>0</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSLocalNetworkUsageDescription</key><string>{LOCAL_NETWORK_USAGE}</string>
{background}
</dict></plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::{BINARIES, HELPER, plist};
    use std::path::Path;

    #[test]
    fn assembles_real_signed_bundles_without_launching() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "fluxdown-dev-bundle-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&output).unwrap();
        for (binary, _) in BINARIES {
            std::fs::copy("/usr/bin/true", output.join(binary)).unwrap();
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let desktop = super::assemble(&root, &output).unwrap();
        let app = desktop
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        for (_, relative) in BINARIES {
            let path = app.join(relative);
            assert!(
                !std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(
                path.canonicalize()
                    .unwrap()
                    .starts_with(app.canonicalize().unwrap())
            );
        }
        for bundle in [app.to_path_buf(), app.join(HELPER)] {
            assert!(bundle.join("Contents/Resources/AppIcon.icns").is_file());
        }
        std::fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn layout_preserves_helper_identity_and_service_adjacency() {
        let helper = Path::new(HELPER).join("Contents/MacOS");
        assert_eq!(BINARIES[0].1, "Contents/MacOS/fluxdown-desktop");
        for (binary, relative) in &BINARIES[1..] {
            assert_eq!(Path::new(relative), helper.join(binary));
        }
        let agent = plist(true);
        for (key, value) in [
            ("CFBundleIdentifier", "com.fluxdown.app.agent"),
            ("CFBundleExecutable", "fluxdown-agent"),
            ("CFBundleName", "FluxDown"),
            ("CFBundleDisplayName", "FluxDown"),
            ("CFBundleIconFile", "AppIcon"),
        ] {
            assert!(agent.contains(&format!("<key>{key}</key><string>{value}</string>")));
        }
        assert!(agent.contains("<key>LSUIElement</key><true/>"));
        for contents in [&agent, &plist(false)] {
            assert!(contents.contains("<key>NSLocalNetworkUsageDescription</key>"));
        }
        assert!(!plist(false).contains("LSUIElement"));
        for contents in [agent, plist(false)] {
            assert!(!contents.contains("CFBundleURLTypes"));
            assert!(!contents.contains("CFBundleDocumentTypes"));
        }
    }
}
