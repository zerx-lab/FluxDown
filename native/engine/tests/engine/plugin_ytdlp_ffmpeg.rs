//! 端到端：yt-dlp 示例插件 `hooks.js` 的 `onDone` 经 `flux.ffmpeg` 把非 mp4 产物
//! 转为 mp4。真实执行依赖 ffmpeg，经 `FLUXDOWN_TEST_FFMPEG=<绝对路径>` 注入；
//! 未设置则跳过（保持 CI 无 ffmpeg 时确定性）。
//!
//! 仅 `plugins` feature 下编译运行。
#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::db::Db;
use fluxdown_engine::plugin::bridge::EngineBridge;
use fluxdown_engine::plugin::quickjs::QuickJsScriptRuntime;
use fluxdown_engine::plugin::runtime::{
    ExecutionBudget, FfmpegSpec, HostContext, PluginBridge, PluginEntryKind, PluginEvent,
    PluginScript, ResolveRequest, ScriptRuntime,
};
use fluxdown_engine::proxy_config::ProxyConfig;

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut d = std::env::temp_dir();
    d.push(format!(
        "fluxdown_ythook_{}_{}_{}",
        tag,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).expect("mkdir temp");
    d
}

async fn make_bridge(data_dir: &Path, ffmpeg: &str) -> Arc<EngineBridge> {
    let db = Db::open(data_dir).await.expect("open db");
    db.set_config(fluxdown_engine::components::CONFIG_FFMPEG_PATH, ffmpeg)
        .await
        .expect("seed ffmpeg path");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    Arc::new(
        EngineBridge::new(db, &ProxyConfig::default(), tx, data_dir.to_path_buf()).expect("bridge"),
    )
}

fn hooks_source() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/ytdlp/hooks.js");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读取 {p:?} 失败: {e}"))
}

fn resolve_source() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/ytdlp/resolve.js");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读取 {p:?} 失败: {e}"))
}

/// 在 jail 内用 ffmpeg 造一个非 mp4（VP9/opus webm）小样本作为 onDone 的输入。
async fn make_sample_webm(bridge: &EngineBridge, jail: &Path, name: &str) {
    let out = bridge
        .run_ffmpeg(
            "test@ff",
            jail.to_path_buf(),
            FfmpegSpec {
                args: vec![
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=32x32:rate=5",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440",
                    "-t",
                    "0.3",
                    "-c:v",
                    "libvpx-vp9",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                    "-c:a",
                    "libopus",
                    "-y",
                    name,
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                subdir: None,
                timeout_ms: Some(60_000),
            },
        )
        .await
        .expect("gen webm");
    assert_eq!(out.code, 0, "webm 生成失败: {}", out.stderr);
    assert!(jail.join(name).exists(), "webm 样本应存在");
}

async fn make_sample(
    bridge: &EngineBridge,
    jail: &Path,
    name: &str,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
) {
    let mut args = Vec::new();
    if video_codec.is_some() {
        args.extend(["-f", "lavfi", "-i", "testsrc2=size=32x32:rate=5"]);
    }
    if audio_codec.is_some() {
        args.extend(["-f", "lavfi", "-i", "sine=frequency=440"]);
    }
    args.extend(["-t", "0.3"]);
    if let Some(codec) = video_codec {
        args.extend(["-c:v", codec]);
    }
    if let Some(codec) = audio_codec {
        args.extend(["-c:a", codec]);
    }
    args.extend(["-y", name]);

    let out = bridge
        .run_ffmpeg(
            "test@ff",
            jail.to_path_buf(),
            FfmpegSpec {
                args: args.into_iter().map(String::from).collect(),
                subdir: None,
                timeout_ms: Some(60_000),
            },
        )
        .await
        .expect("gen media sample");
    assert_eq!(out.code, 0, "样本生成失败: {}", out.stderr);
    assert!(jail.join(name).exists(), "样本应存在: {name}");
}

async fn probe_stream_codecs(
    bridge: &EngineBridge,
    jail: &Path,
    name: &str,
) -> Vec<(String, String)> {
    let out = bridge
        .run_ffprobe(
            "test@ff",
            jail.to_path_buf(),
            FfmpegSpec {
                args: [
                    "-v",
                    "error",
                    "-print_format",
                    "json",
                    "-show_entries",
                    "stream=codec_type,codec_name",
                    name,
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                subdir: None,
                timeout_ms: Some(60_000),
            },
        )
        .await
        .expect("probe media sample");
    assert_eq!(out.code, 0, "样本探测失败: {}", out.stderr);
    let probe: serde_json::Value = serde_json::from_str(&out.stdout).expect("parse ffprobe json");
    probe["streams"]
        .as_array()
        .expect("ffprobe streams array")
        .iter()
        .map(|stream| {
            (
                stream["codec_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                stream["codec_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

async fn assert_stream_codecs(
    bridge: &EngineBridge,
    jail: &Path,
    name: &str,
    expected: &[(&str, &str)],
) {
    let codecs = probe_stream_codecs(bridge, jail, name).await;
    for &(stream_type, codec_name) in expected {
        assert!(
            codecs
                .iter()
                .any(|codec| codec == &(stream_type.to_string(), codec_name.to_string())),
            "{name} 缺少 {stream_type}/{codec_name} 轨，实际: {codecs:?}"
        );
    }
}

async fn run_on_done(
    rt: &QuickJsScriptRuntime,
    bridge: Arc<dyn PluginBridge>,
    jail: &Path,
    file_path: &Path,
    audio_path: Option<&Path>,
    settings_json: &str,
) {
    let script = PluginScript {
        identity: "fluxdown@ytdlp".to_string(),
        source: hooks_source(),
        entry_fn_hint: PluginEntryKind::Hook,
        version: "1.2.0".to_string(),
        app_version: "0.1.60".to_string(),
    };
    let event = PluginEvent::Done {
        task_id: "t1".to_string(),
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
        file_path: file_path.to_string_lossy().into_owned(),
        audio_path: audio_path.map(|path| path.to_string_lossy().into_owned()),
        muxed: audio_path.is_none(),
    };
    let budget = ExecutionBudget {
        timeout: Duration::from_secs(120),
        memory_limit_bytes: 32 * 1024 * 1024,
    };
    let host = HostContext {
        ffmpeg_permitted: true,
        ffmpeg_root: Some(jail.to_path_buf()),
        ..Default::default()
    };
    // invoke_hook 为 fire-and-forget（吞错仅记日志）：await 完成后由产物断言判定成败。
    rt.invoke_hook(
        &script,
        event,
        settings_json.to_string(),
        bridge,
        budget,
        host,
    )
    .await;
}

/// preferMp4=true + 非 mp4 产物 → onDone 应产出同名 .mp4。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_converts_webm_to_mp4() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过真实转码");
        return;
    };
    let data_dir = unique_dir("data_conv");
    let jail = unique_dir("jail_conv");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample_webm(&bridge, &jail, "out.webm").await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("out.webm"),
        None,
        r#"{"preferMp4":true,"verbose":true,"quality":"best"}"#,
    )
    .await;

    let mp4 = jail.join("out.mp4");
    let meta = std::fs::metadata(&mp4).expect("onDone 应产出 out.mp4");
    assert!(meta.len() > 0, "产出的 mp4 应非空");
}

/// preferMp4=false → 门控短路，不产出 mp4（源 webm 原样保留）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_skips_when_prefer_mp4_is_false() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过门控断言");
        return;
    };
    let data_dir = unique_dir("data_skip");
    let jail = unique_dir("jail_skip");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample_webm(&bridge, &jail, "keep.webm").await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("keep.webm"),
        None,
        r#"{"preferMp4":false,"verbose":true,"quality":"best"}"#,
    )
    .await;

    assert!(
        !jail.join("keep.mp4").exists(),
        "preferMp4=false 时不应产出 mp4"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_remuxes_h264_container_to_mp4() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过 remux 断言");
        return;
    };
    let data_dir = unique_dir("data_remux");
    let jail = unique_dir("jail_remux");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(&bridge, &jail, "remux.mkv", Some("libx264"), Some("aac")).await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("remux.mkv"),
        None,
        r#"{"preferMp4":true,"verbose":true}"#,
    )
    .await;

    assert!(jail.join("remux.mp4").exists(), "h264 mkv 应 remux 为 mp4");
    assert_stream_codecs(
        &bridge,
        &jail,
        "remux.mp4",
        &[("video", "h264"), ("audio", "aac")],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_remuxes_video_without_audio_track() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过无音轨 remux 断言");
        return;
    };
    let data_dir = unique_dir("data_no_audio");
    let jail = unique_dir("jail_no_audio");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(&bridge, &jail, "no_audio.mkv", Some("libx264"), None).await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("no_audio.mkv"),
        None,
        r#"{"preferMp4":true,"verbose":true}"#,
    )
    .await;

    assert!(
        jail.join("no_audio.mp4").exists(),
        "无音轨 h264 mkv 应成功 remux 为 mp4"
    );
    assert_stream_codecs(&bridge, &jail, "no_audio.mp4", &[("video", "h264")]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_selectors_prioritize_resolution_and_prefer_avc1() {
    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let resolve_src = resolve_source();
    let test_js = format!(
        r#"
        {resolve_src}
        globalThis.resolve = async (ctx) => {{
            var fmtTrue = buildFormat(true);
            if (!fmtTrue.includes("bestvideo[ext=mp4]+bestaudio[ext=m4a]") || !fmtTrue.includes("bestvideo+bestaudio")) {{
                throw new Error("buildFormat(true) 格式链不合预期: " + fmtTrue);
            }}
            var fmtFalse = buildFormat(false);
            if (fmtFalse !== "bestvideo+bestaudio/best") {{
                throw new Error("buildFormat(false) 格式链不合预期: " + fmtFalse);
            }}
            var formats = [
                {{ format_id: "2160_vp9", url: "http://example.com/2160_vp9", height: 2160, vcodec: "vp09", ext: "webm", tbr: 20000 }},
                {{ format_id: "1080_avc", url: "http://example.com/1080_avc", height: 1080, vcodec: "avc1.640028", ext: "mp4", tbr: 5000 }},
                {{ format_id: "1080_vp9", url: "http://example.com/1080_vp9", height: 1080, vcodec: "vp09", ext: "webm", tbr: 4000 }},
                {{ format_id: "720_avc", url: "http://example.com/720_avc", height: 720, vcodec: "avc1.4d401f", ext: "mp4", tbr: 2500 }}
            ];
            // 1. 2160p 目标下，2160p VP9 决不能被 1080p AVC1 顶替（分辨率绝对优先）
            var pick2160 = pickVideoAtOrBelow(formats, 2160, true);
            if (!pick2160 || pick2160.height !== 2160) {{
                throw new Error("pickVideoAtOrBelow(2160) 未保留 2160p: " + JSON.stringify(pick2160));
            }}
            // 2. 1080p 目标下，同分辨率 1080p AVC1 优先于 1080p VP9
            var pick1080 = pickVideoAtOrBelow(formats, 1080, true);
            if (!pick1080 || pick1080.format_id !== "1080_avc") {{
                throw new Error("pickVideoAtOrBelow(1080) 未优先选择 avc1: " + JSON.stringify(pick1080));
            }}
            // 3. 平台白名单判定：主流支持平台与扩容的社交平台都必须命中
            for (var u of [
                "https://www.youtube.com/watch?v=1",
                "https://www.bilibili.com/video/BV1",
                "https://www.nicovideo.jp/watch/sm1",
                "https://www.twitch.tv/videos/1",
                "https://vimeo.com/1",
                "https://www.dailymotion.com/video/x1",
                "https://soundcloud.com/artist/track",
                "https://www.acfun.cn/v/ac1",
                "https://www.instagram.com/p/1",
                "https://x.com/user/status/1",
                "https://twitter.com/user/status/1",
                "https://www.tiktok.com/@u/video/1",
                "https://www.reddit.com/r/v/comments/1",
                "https://v.redd.it/abc",
                "https://www.facebook.com/watch?v=1",
                "https://fb.watch/abc"
            ]) {{
                if (!detectPlatform(u)) throw new Error("detectPlatform 未能识别: " + u);
            }}
            return {{
                url: "http://example.com/ok",
                fileName: "ok.mp4",
                ephemeral: true,
                rangeSupported: true
            }};
        }};
        "#
    );
    let data_dir = unique_dir("data_selectors");
    let bridge = make_bridge(&data_dir, "ffmpeg").await;
    let script = PluginScript {
        identity: "fluxdown@ytdlp".to_string(),
        source: test_js,
        entry_fn_hint: PluginEntryKind::Resolve,
        version: "1.2.0".to_string(),
        app_version: "0.1.60".to_string(),
    };
    let req = ResolveRequest {
        task_id: "t_sel".to_string(),
        url: "https://www.youtube.com/watch?v=test".to_string(),
        browser_original_url: None,
        auth_ref: String::new(),
        cookies: String::new(),
        referrer: String::new(),
        user_agent: String::new(),
        extra_headers: std::collections::HashMap::new(),
        resolver_item: String::new(),
    };
    let budget = ExecutionBudget {
        timeout: Duration::from_secs(30),
        memory_limit_bytes: 32 * 1024 * 1024,
    };
    let host = HostContext::default();
    let res = rt
        .invoke_resolve(&script, req, "{}".to_string(), bridge, budget, host)
        .await
        .expect("invoke_resolve must succeed");
    let res = res.expect("resolve result must be Some");
    assert_eq!(res.url, "http://example.com/ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_merges_audio_sidecar_for_compatible_video() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过 sidecar 合并断言");
        return;
    };
    let data_dir = unique_dir("data_sidecar");
    let jail = unique_dir("jail_sidecar");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(&bridge, &jail, "video.mp4", Some("libx264"), None).await;
    make_sample(&bridge, &jail, "video.audio.m4a", None, Some("aac")).await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("video.mp4"),
        Some(&jail.join("video.audio.m4a")),
        r#"{"preferMp4":true,"verbose":true}"#,
    )
    .await;

    assert!(
        jail.join("video.compatible.mp4").exists(),
        "已有 h264 mp4 仍须合并独立音频"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_skips_compatible_output_pure_audio_and_probe_failure() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过短路分支断言");
        return;
    };
    let data_dir = unique_dir("data_skips");
    let jail = unique_dir("jail_skips");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(
        &bridge,
        &jail,
        "compatible.mp4",
        Some("libx264"),
        Some("aac"),
    )
    .await;
    make_sample(&bridge, &jail, "audio.m4a", None, Some("aac")).await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    for name in ["compatible.mp4", "audio.m4a", "missing.webm"] {
        let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
        run_on_done(
            &rt,
            dyn_bridge,
            &jail,
            &jail.join(name),
            None,
            r#"{"preferMp4":true,"verbose":true}"#,
        )
        .await;
    }

    assert!(!jail.join("compatible.compatible.mp4").exists());
    assert!(!jail.join("audio.mp4").exists());
    assert!(!jail.join("missing.mp4").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_transcodes_non_h264_video_inside_mp4() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过 MP4 视频转码断言");
        return;
    };
    let data_dir = unique_dir("data_non_h264_mp4");
    let jail = unique_dir("jail_non_h264_mp4");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(&bridge, &jail, "mpeg4.mp4", Some("mpeg4"), Some("aac")).await;
    assert_stream_codecs(
        &bridge,
        &jail,
        "mpeg4.mp4",
        &[("video", "mpeg4"), ("audio", "aac")],
    )
    .await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("mpeg4.mp4"),
        None,
        r#"{"preferMp4":true,"verbose":true}"#,
    )
    .await;

    assert_stream_codecs(
        &bridge,
        &jail,
        "mpeg4.compatible.mp4",
        &[("video", "h264"), ("audio", "aac")],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_done_transcodes_non_aac_audio_inside_h264_mp4() {
    let Ok(ffmpeg) = std::env::var("FLUXDOWN_TEST_FFMPEG") else {
        eprintln!("[skip] 未设置 FLUXDOWN_TEST_FFMPEG，跳过 MP4 音频转码断言");
        return;
    };
    let data_dir = unique_dir("data_non_aac_mp4");
    let jail = unique_dir("jail_non_aac_mp4");
    let bridge = make_bridge(&data_dir, &ffmpeg).await;
    make_sample(&bridge, &jail, "alac.mp4", Some("libx264"), Some("alac")).await;
    assert_stream_codecs(
        &bridge,
        &jail,
        "alac.mp4",
        &[("video", "h264"), ("audio", "alac")],
    )
    .await;

    let rt = QuickJsScriptRuntime::new(2).expect("runtime");
    let dyn_bridge: Arc<dyn PluginBridge> = bridge.clone();
    run_on_done(
        &rt,
        dyn_bridge,
        &jail,
        &jail.join("alac.mp4"),
        None,
        r#"{"preferMp4":true,"verbose":true}"#,
    )
    .await;

    assert_stream_codecs(
        &bridge,
        &jail,
        "alac.compatible.mp4",
        &[("video", "h264"), ("audio", "aac")],
    )
    .await;
}
