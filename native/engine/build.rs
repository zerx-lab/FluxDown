//! 构建脚本：从仓库根 pubspec.yaml 读取应用版本号，注入 `FLUXDOWN_APP_VERSION`
//! 编译期环境变量，供 downloader.rs 拼出 aria2 风格的默认 UA（`FluxDown/<版本>`）。
//!
//! 读取失败（独立打包 engine crate 等场景）时回退 "1.0"。

use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let pubspec = Path::new(&manifest_dir).join("../../pubspec.yaml");
    println!("cargo:rerun-if-changed={}", pubspec.display());

    // pubspec 版本形如 `version: 0.1.44+1`，取 `+` 前的语义版本。
    let version = fs::read_to_string(&pubspec)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let rest = line.strip_prefix("version:")?;
                let v = rest.trim().split('+').next().unwrap_or("").trim();
                if v.is_empty() { None } else { Some(v.to_string()) }
            })
        })
        .unwrap_or_else(|| "1.0".to_string());
    println!("cargo:rustc-env=FLUXDOWN_APP_VERSION={version}");
}
