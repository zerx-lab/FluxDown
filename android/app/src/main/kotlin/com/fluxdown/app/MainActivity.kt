package com.fluxdown.app

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

/**
 * FluxDown 桌面启动入口 + 本地存储桥宿主。
 *
 * 只承担桌面 Launcher（不透明主题，见 MainTheme）与 [AppStorage] 本地存储能力；
 * 外部下载唤起由 [ExternalDownloadActivity] 承载（透明窗口弹下载框）。二者共享
 * 同一个 FlutterEngine（见 [FluxdownEngine]），保持单 Dart 会话、下载状态、
 * Rust 桥与前台服务不重复初始化。
 *
 * 引擎在 [onStart] 里经 [FluxdownEngine.cacheIfAbsent] 缓存、channel 经
 * [AppStorage.bind] 绑定——[onStart] 对 fresh 与 cached 两种引擎路径都会执行，
 * 不依赖 configureFlutterEngine 的版本差异。
 */
class MainActivity : FlutterActivity() {
    override fun getCachedEngineId(): String? =
        if (FluxdownEngine.cached != null) FluxdownEngine.ENGINE_ID else null

    override fun onStart() {
        super.onStart()
        getFlutterEngine()?.let { engine ->
            FluxdownEngine.cacheIfAbsent(engine)
            AppStorage.bind(engine, this)
        }
        FluxdownEngine.registerMain(this)
    }

    override fun onDestroy() {
        FluxdownEngine.clearMain(this)
        super.onDestroy()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        // 目录选择器结果由 AppStorage 处理；其余交给默认行为。
        if (!AppStorage.onActivityResult(this, requestCode, resultCode, data)) {
            super.onActivityResult(requestCode, resultCode, data)
        }
    }
}