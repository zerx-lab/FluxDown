package com.fluxdown.app

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.android.FlutterActivityLaunchConfigs.BackgroundMode
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/**
 * FluxDown 外部下载唤起入口（透明窗口弹下载框）。
 *
 * 通过 `singleInstance` + `taskAffinity=""` 运行在独立任务栈，窗口背景透明
 * （[getBackgroundMode] transparent + ExternalTheme），Dart 侧在外部流程下用
 * `SizedBox.expand()` 隐藏主页、只留新建下载弹窗，于是弹窗下方透出来源应用
 * （浏览器 / 文件管理器）—— 与 InstallerX 的"透明卡片弹窗"一致。
 *
 * 与 [MainActivity] 共享同一个 FlutterEngine（见 [FluxdownEngine]）。share 与
 * storage channel 都在 [onStart] 绑定到当前承载引擎的 Activity 上，因此
 * `moveTaskToBack`（关闭后返回来源应用）作用于本 external 独立任务。
 */
class ExternalDownloadActivity : FlutterActivity() {
    override fun getBackgroundMode(): BackgroundMode = BackgroundMode.transparent

    override fun getCachedEngineId(): String? =
        if (FluxdownEngine.cached != null) FluxdownEngine.ENGINE_ID else null

    private var shareChannel: MethodChannel? = null
    /** 冷启动时后台解析暂存的分享内容，Dart 首次 getInitialShare 时取走。 */
    private val pendingShare = CompletableFuture<HashMap<String, String>?>()
    /** 一个 Activity 实例只派发一次冷分享（避免回到前台/重绑事件重复触发）。 */
    private var shareDelivered = false
    /** 本 Activity 是否创建了引擎（false = 应用已在运行，Dart 已 init 过，须走 onShare）。 */
    private var createdEngine = false

    override fun onCreate(savedInstanceState: Bundle?) {
        createdEngine = FluxdownEngine.cached == null
        super.onCreate(savedInstanceState)
    }

    override fun onStart() {
        super.onStart()
        getFlutterEngine()?.let(::bindAndDeliver)
    }

    private fun bindAndDeliver(engine: FlutterEngine) {
        FluxdownEngine.cacheIfAbsent(engine)
        // 透明弹窗接管渲染：结束仍存活、持有已解绑 FlutterView 的 MainActivity，
        // 避免其之后被重新打开时黑屏（共享引擎渲染面同一时刻只能挂一个 view）。
        FluxdownEngine.releaseMainHost()
        // 外部弹窗内同样能触发存储相关操作（换目录 / 装 APK / 打开文件）。
        AppStorage.bind(engine, this)
        if (shareChannel == null) {
            shareChannel = MethodChannel(
                engine.dartExecutor.binaryMessenger,
                SHARE_CHANNEL,
            ).apply {
                setMethodCallHandler { call, result ->
                    when (call.method) {
                        // Dart 侧就绪后主动拉取冷启动分享（等待后台解析完成即取走）
                        "getInitialShare" -> {
                            result.success(
                                try {
                                    pendingShare.get(2000, TimeUnit.MILLISECONDS)
                                } catch (_: Exception) {
                                    null
                                },
                            )
                        }
                        // 关闭弹窗后把本 external 任务移到后台，露出来源应用
                        "moveTaskToBack" -> {
                            result.success(moveTaskToBack(true))
                        }
                        else -> result.notImplemented()
                    }
                }
            }
        }
        if (shareDelivered) return
        shareDelivered = true
        // 后台线程解析 intent（超大输入不阻塞主线程）。冷启动（引擎由本 Activity 新建）
        // 交给 Dart getInitialShare 取走；应用已在运行（复用引擎）则直接 onShare 推送。
        Thread {
            val shared = extractShared(intent) ?: return@Thread
            if (createdEngine) {
                pendingShare.complete(shared)
            } else {
                runOnUiThread { shareChannel?.invokeMethod("onShare", shared) }
            }
        }.start()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (!AppStorage.onActivityResult(this, requestCode, resultCode, data)) {
            super.onActivityResult(requestCode, resultCode, data)
        }
    }

    /** 热启动（singleInstance）：external 任务已在/被置顶，新分享 intent 到达。 */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        // 后台解析避免阻塞主线程；完成后回主线程推送 Dart。
        Thread {
            val shared = extractShared(intent) ?: return@Thread
            runOnUiThread {
                if (shareChannel != null) {
                    shareChannel?.invokeMethod("onShare", shared)
                } else {
                    // channel 尚未建立（极端时序）：暂存等待 getInitialShare
                    pendingShare.complete(shared)
                }
            }
        }.start()
    }

    // ── 分享载荷解析 ──

    /**
     * 从 intent 提取可下载链接及协议携带的请求上下文。
     * `fluxdown://download` 可携带 filename/cookies/referrer/headers；普通
     * ACTION_VIEW/ACTION_SEND 只有 URL。所有此入口 intent 均标记 external，
     * 供 Dart 在弹层关闭后返回来源应用。
     */
    private fun extractShared(intent: Intent?): HashMap<String, String>? {
        if (intent == null) return null
        return when (intent.action) {
            Intent.ACTION_SEND, Intent.ACTION_SEND_MULTIPLE -> {
                val text = intent.getStringExtra(Intent.EXTRA_TEXT)?.trim()?.ifEmpty { null }
                    ?: extractClipText(intent)
                text?.let { sharePayload(it, external = true) }
            }
            Intent.ACTION_VIEW -> {
                val data = intent.dataString?.trim()?.ifEmpty { null } ?: return null
                if (data.startsWith("fluxdown://", ignoreCase = true)) {
                    try {
                        val uri = Uri.parse(data)
                        if (!uri.host.equals("download", ignoreCase = true)) return null
                        val url = uri.getQueryParameter("url")?.trim()?.ifEmpty { null }
                            ?: return null
                        sharePayload(
                            url = url,
                            filename = uri.getQueryParameter("filename")?.trim() ?: "",
                            cookies = uri.getQueryParameter("cookies") ?: "",
                            referrer = uri.getQueryParameter("referrer")?.trim() ?: "",
                            headers = uri.getQueryParameter("headers") ?: "",
                            external = true,
                        )
                    } catch (_: Exception) {
                        null
                    }
                } else {
                    sharePayload(data, external = true)
                }
            }
            else -> null
        }
    }

    private fun sharePayload(
        url: String,
        filename: String = "",
        cookies: String = "",
        referrer: String = "",
        headers: String = "",
        external: Boolean = false,
    ): HashMap<String, String> = hashMapOf(
        "url" to url.take(MAX_URL_LEN),
        "filename" to filename.take(MAX_NAME_LEN),
        "cookies" to cookies.take(MAX_COOKIES_LEN),
        "referrer" to referrer.take(MAX_REFERRER_LEN),
        // headers 是 JSON，截断会破坏结构，超限直接丢弃
        "headers" to if (headers.length <= MAX_HEADERS_JSON_LEN) headers else "",
        "external" to external.toString(),
    )

    /** 遍历 ClipData 取首个非空文本项（分享 intent 缺 EXTRA_TEXT 时的兜底）。 */
    private fun extractClipText(intent: Intent): String? {
        val clip = intent.clipData ?: return null
        for (i in 0 until clip.itemCount) {
            val text = clip.getItemAt(i).text?.toString()?.trim()
            if (!text.isNullOrEmpty()) return text
        }
        return null
    }

    companion object {
        private const val SHARE_CHANNEL = "com.fluxdown/share"

        // 外部 intent 字段截断上限，防止超大输入卡死 UI
        private const val MAX_URL_LEN = 8192
        private const val MAX_NAME_LEN = 512
        private const val MAX_COOKIES_LEN = 65536
        private const val MAX_REFERRER_LEN = 8192
        private const val MAX_HEADERS_JSON_LEN = 131072
    }
}