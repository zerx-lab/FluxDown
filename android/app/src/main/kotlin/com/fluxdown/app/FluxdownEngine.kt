package com.fluxdown.app

import android.app.Activity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.FlutterEngineCache
import java.lang.ref.WeakReference

/**
 * FluxDown 单 FlutterEngine 保持者。
 *
 * MainActivity 与 ExternalDownloadActivity 共享同一个引擎，保证单 Dart 会话、
 * 下载状态、Rust 桥与前台服务不重复初始化。首个启动的 Activity 走 FlutterActivity
 * 默认的 createFlutterEngine 路径（插件在此注册一次、Dart entrypoint 只运行一次），
 * 并在其 onStart 里经 [cacheIfAbsent] 把引擎缓存；后续 Activity 通过 override
 * [io.flutter.embedding.android.FlutterActivity.getCachedEngineId] 复用引擎。
 * 新 embedding 中引擎为进程级持有，不随 Activity 销毁自动销毁，故复用方始终
 * 拿到同一个仍存活的引擎。缓存/绑定统一放在 onStart，避免受 configureFlutterEngine
 * 对 cached 引擎是否触发的版本差异影响。
 *
 * 渲染约束：一个 FlutterEngine 的渲染面同一时刻只能挂载一个 FlutterView。因此当
 * ExternalDownloadActivity 接管渲染（前台显示透明弹窗）时，必须结束掉 Main 的
 * 视图（见 [releaseMainHost]），否则 Main 这个仍存活的 Activity 会持有一个已解绑、
 * 且不会再重新挂载引擎的 FlutterView，重新打开时黑屏。Main 之后由桌面/多任务
 * 触发时是全新创建、重新挂载缓存引擎的，可正常渲染。
 */
object FluxdownEngine {
    const val ENGINE_ID = "com.fluxdown.app/engine"

    /** 当前持有视图的 MainActivity 弱引用（仅用于 [releaseMainHost] 结束其 stale 视图）。 */
    private var mainRef: WeakReference<Activity>? = null

    val cached: FlutterEngine?
        get() = FlutterEngineCache.getInstance().get(ENGINE_ID)

    fun cacheIfAbsent(engine: FlutterEngine) {
        if (cached == null) {
            FlutterEngineCache.getInstance().put(ENGINE_ID, engine)
        }
    }

    fun registerMain(activity: Activity) {
        mainRef = WeakReference(activity)
    }

    fun clearMain(activity: Activity) {
        if (mainRef?.get() === activity) {
            mainRef = null
        }
    }

    /**
     * External 前台接管渲染后调用：结束仍活着的 MainActivity，回收其已解绑、无法
     * 再重新挂载引擎的 FlutterView，只保留共享引擎。Main 下次由桌面/多任务启动时
     * 全新创建并重新挂载缓存引擎，恢复正常渲染。
     */
    fun releaseMainHost() {
        mainRef?.get()?.finish()
        mainRef = null
    }
}