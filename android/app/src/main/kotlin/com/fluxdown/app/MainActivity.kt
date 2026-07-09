package com.fluxdown.app

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.Settings
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * FluxDown 移动端存储桥。
 *
 * MethodChannel `com.fluxdown/storage`：
 * - `pickDirectory`        → 调起系统文件管理器（SAF ACTION_OPEN_DOCUMENT_TREE）
 *                            选择目录，返回可供 Rust 引擎 std::fs 直写的文件系统
 *                            路径；无法映射（如云存储 provider）返回 null。
 * - `hasAllFilesAccess`    → 是否已具备写公共目录的权限
 *                            （API 30+: 所有文件访问；API <30: WRITE_EXTERNAL_STORAGE）。
 * - `requestAllFilesAccess`→ 引导授权（API 30+ 跳系统设置页；API <30 运行时权限弹窗）。
 */
class MainActivity : FlutterActivity() {
    private var pendingResult: MethodChannel.Result? = null
    private var shareChannel: MethodChannel? = null
    /** 冷启动时暂存的分享内容，等 Dart 侧首次 getInitialShare 时取走。 */
    private var pendingShare: String? = null
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            CHANNEL,
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "pickDirectory" -> pickDirectory(result)
                "hasAllFilesAccess" -> result.success(hasAllFilesAccess())
                "requestAllFilesAccess" -> {
                    requestAllFilesAccess()
                    result.success(null)
                }
                // 应用专属外部下载目录。必须经 framework 创建
                // （Android/data 层禁止应用自建子树），Rust std::fs 才能直写。
                "getExternalDownloadDir" ->
                    result.success(getExternalFilesDir("Download")?.absolutePath)
                // 应用内更新：唤起系统安装器安装下载好的 APK
                "installApk" -> installApk(call.argument<String>("path"), result)
                else -> result.notImplemented()
            }
        }
        shareChannel = MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            SHARE_CHANNEL,
        ).apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    // Dart 侧就绪后主动拉取冷启动分享（取走即清空）
                    "getInitialShare" -> {
                        result.success(pendingShare)
                        pendingShare = null
                    }
                    else -> result.notImplemented()
                }
            }
        }
        // 冷启动：configureFlutterEngine 时 Dart 尚未注册 handler，先暂存
        pendingShare = extractShared(intent)
    }

    // ── 目录选择（SAF） ──

    private fun pickDirectory(result: MethodChannel.Result) {
        if (pendingResult != null) {
            result.error("busy", "directory picker already open", null)
            return
        }
        pendingResult = result
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION or
                    Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                    Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
            )
        }
        try {
            startActivityForResult(intent, REQUEST_PICK_DIR)
        } catch (e: Exception) {
            pendingResult = null
            result.error("unavailable", e.message, null)
        }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode != REQUEST_PICK_DIR) {
            super.onActivityResult(requestCode, resultCode, data)
            return
        }
        val result = pendingResult ?: return
        pendingResult = null
        val uri = data?.data
        if (resultCode != Activity.RESULT_OK || uri == null) {
            result.success(null) // 用户取消
            return
        }
        try {
            contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        } catch (_: Exception) {
            // 持久化授权失败不致命：路径写入依赖文件系统权限而非 SAF 授权
        }
        // null = 用户取消；"" = 无法映射为文件系统路径（Dart 侧提示重选）
        result.success(treeUriToPath(uri) ?: "")
    }

    /**
     * SAF tree URI → 文件系统路径。
     *
     * 仅外部存储 provider 可映射：
     * - `primary:<rel>` → `/storage/emulated/0/<rel>`
     * - `home:<rel>`    → 公共 Documents 目录
     * - `<volId>:<rel>` → `/storage/<volId>/<rel>`（SD 卡等）
     * 其他 provider（下载 provider / 云存储）返回 null，由 Dart 侧提示重选。
     */
    private fun treeUriToPath(uri: Uri): String? {
        if (uri.authority != "com.android.externalstorage.documents") return null
        val docId = DocumentsContract.getTreeDocumentId(uri)
        val split = docId.split(":", limit = 2)
        val volume = split[0]
        val rel = split.getOrElse(1) { "" }
        val base = when (volume) {
            "primary" -> Environment.getExternalStorageDirectory().absolutePath
            "home" ->
                Environment
                    .getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)
                    .absolutePath
            else -> "/storage/$volume"
        }
        return if (rel.isEmpty()) base else "$base/$rel"
    }

    // ── 公共目录写权限 ──

    private fun hasAllFilesAccess(): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) ==
                PackageManager.PERMISSION_GRANTED
        }

    private fun requestAllFilesAccess() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            try {
                startActivity(
                    Intent(
                        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:$packageName"),
                    ),
                )
            } catch (_: Exception) {
                // 个别 ROM 不支持带包名的入口，退回总开关页
                try {
                    startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                } catch (_: Exception) {
                }
            }
        } else {
            requestPermissions(
                arrayOf(Manifest.permission.WRITE_EXTERNAL_STORAGE),
                REQUEST_WRITE_PERM,
            )
        }
    }

    // ── 应用内更新：APK 安装唤起 ──

    /**
     * 经 FileProvider 把 cache 目录下的 APK 交给系统安装器。
     * Android 8+ 首次会引导用户开启"允许安装未知应用"，随后重入安装流程。
     * 返回 true=已发出安装 intent；错误经 result.error 报回 Dart。
     */
    private fun installApk(path: String?, result: MethodChannel.Result) {
        if (path.isNullOrEmpty()) {
            result.error("bad_args", "path is required", null)
            return
        }
        val file = java.io.File(path)
        if (!file.exists()) {
            result.error("not_found", "APK not found: $path", null)
            return
        }
        try {
            val uri = androidx.core.content.FileProvider.getUriForFile(
                this,
                "$packageName.fileprovider",
                file,
            )
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            startActivity(intent)
            result.success(true)
        } catch (e: Exception) {
            result.error("install_failed", e.message, null)
        }
    }

    /** 热启动（singleTop）：应用已在前台/后台，新分享 intent 到达。 */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        val shared = extractShared(intent) ?: return
        // Dart 侧已就绪，直接推送；channel 未建成则暂存兜底
        shareChannel?.invokeMethod("onShare", shared) ?: run { pendingShare = shared }
    }

    /**
     * 从 intent 提取可下载的 URL / magnet。
     * - ACTION_SEND：取 EXTRA_TEXT（浏览器"分享链接"）
     * - ACTION_VIEW：取 data（magnet: 直链等）
     * 返回 null 表示无可用内容（如首页 LAUNCHER 启动）。
     */
    private fun extractShared(intent: Intent?): String? {
        if (intent == null) return null
        return when (intent.action) {
            Intent.ACTION_SEND ->
                intent.getStringExtra(Intent.EXTRA_TEXT)?.trim()?.ifEmpty { null }
            Intent.ACTION_VIEW ->
                intent.dataString?.trim()?.ifEmpty { null }
            else -> null
        }
    }

    companion object {
        private const val CHANNEL = "com.fluxdown/storage"
        private const val SHARE_CHANNEL = "com.fluxdown/share"
        private const val REQUEST_PICK_DIR = 0x4D01
        private const val REQUEST_WRITE_PERM = 0x4D02
    }
}
