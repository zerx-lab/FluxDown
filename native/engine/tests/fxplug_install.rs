//! 验证市场分发的 `.fxplug`（hand-built store zip）能被引擎的 `zip` crate 安装管线
//! 正确解压 + manifest 校验。通过环境变量 `FLUXDOWN_TEST_FXPLUG` 指向 .fxplug 路径运行：
//!
//! ```text
//! FLUXDOWN_TEST_FXPLUG=.../echo-rewriter-1.0.0.fxplug \
//!   cargo test -p fluxdown_engine --features plugins --test fxplug_install
//! ```
//! 未设置该环境变量时跳过（不阻塞常规 CI）。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use fluxdown_engine::plugin::install;

#[test]
fn fxplug_installs_via_zip_pipeline() {
    let Ok(path) = std::env::var("FLUXDOWN_TEST_FXPLUG") else {
        eprintln!("FLUXDOWN_TEST_FXPLUG 未设置，跳过");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fxplug");
    let work = std::env::temp_dir().join(format!("fxplug-install-{}", std::process::id()));
    std::fs::create_dir_all(&work).expect("mkdir");
    let identity = install::install_from_zip(&work, &bytes).expect("install_from_zip must succeed");
    // 默认校验示例 echo-rewriter；可经 FLUXDOWN_TEST_FXPLUG_IDENTITY 覆盖以验证其它包。
    let expected = std::env::var("FLUXDOWN_TEST_FXPLUG_IDENTITY")
        .unwrap_or_else(|_| "fluxdown@echo-rewriter".to_string());
    assert_eq!(identity, expected);
    assert!(work.join(&identity).join("manifest.json").exists());
    assert!(work.join(&identity).join("resolve.js").exists());
    let _ = std::fs::remove_dir_all(&work);
    eprintln!("OK: .fxplug 经 zip crate 安装管线成功，identity={identity}");
}
