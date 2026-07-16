//! 校验 `examples/plugins/` 下随仓库分发的示例插件：manifest 必须通过
//! 引擎真实校验器，声明的入口脚本必须存在。防示例与校验规则漂移。
#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use fluxdown_engine::plugin::manifest::PluginManifest;

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins")
}

#[test]
fn all_example_plugin_manifests_are_valid() {
    let dir = examples_dir();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("读取 {dir:?} 失败: {e}"));
    let mut checked = 0usize;
    for entry in entries {
        let plugin_dir = entry.expect("dir entry").path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let manifest_path = plugin_dir.join("manifest.json");
        let bytes = std::fs::read(&manifest_path)
            .unwrap_or_else(|e| panic!("读取 {manifest_path:?} 失败: {e}"));
        let manifest = PluginManifest::parse(&bytes)
            .unwrap_or_else(|e| panic!("{manifest_path:?} 解析失败: {e}"));
        manifest
            .validate()
            .unwrap_or_else(|e| panic!("{manifest_path:?} 校验失败: {e}"));

        for resolver in &manifest.resolvers {
            let entry_path = plugin_dir.join(&resolver.entry);
            assert!(
                entry_path.is_file(),
                "{:?} 声明的 resolver 入口不存在: {:?}",
                manifest_path,
                entry_path
            );
        }
        if let Some(hooks) = &manifest.hooks {
            let entry_path = plugin_dir.join(&hooks.entry);
            assert!(
                entry_path.is_file(),
                "{:?} 声明的 hooks 入口不存在: {:?}",
                manifest_path,
                entry_path
            );
        }
        checked += 1;
    }
    assert!(checked >= 2, "示例插件目录应至少含 echo-rewriter 与 ytdlp");
}
