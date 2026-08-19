package com.fluxdown.app

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.Settings
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * FluxDown 移动端本地存储能力（SAF 目录选择 / 公共目录写权限 / 应用内更新 /
 * 打开已下载文件）。由 MainActivity 与 ExternalDownloadActivity 通过
 * [bind] 在各自承载的 FlutterEngine 上登记同一个 channel。
 *
 * 由于同一时刻只有一个 Activity 承载引擎，[pendingResult]（目录选择器结果）以
 * 单例持有即可，不会并发冲突。
 */
object AppStorage {
    const val CHANNEL = "com.fluxdown/storage"
    private const val REQUEST_PICK_DIR = 0x4D01
    private const val REQUEST_WRITE_PERM = 0x4D02

    private var pendingResult: MethodChannel.Result? = null

    /** 在给定引擎上登记 `com.fluxdown/storage` channel。 */
    fun bind(engine: FlutterEngine, activity: Activity) {
        MethodChannel(
            engine.dartExecutor.binaryMessenger,
            CHANNEL,
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "pickDirectory" -> pickDirectory(activity, result)
                "hasAllFilesAccess" -> result.success(hasAllFilesAccess(activity))
                "requestAllFilesAccess" -> {
                    requestAllFilesAccess(activity)
                    result.success(null)
                }
                // 应用专属外部下载目录。必须经 framework 创建
                // （Android/data 层禁止应用自建子树），Rust std::fs 才能直写。
                "getExternalDownloadDir" ->
                    result.success(activity.getExternalFilesDir("Download")?.absolutePath)
                // 应用内更新：唤起系统安装器安装下载好的 APK
                "installApk" -> installApk(activity, call.argument<String>("path"), result)
                // 用系统默认应用打开已下载文件（无关联时回退系统选择器）
                "openFile" -> openFile(activity, call.argument<String>("path"), result)
                else -> result.notImplemented()
            }
        }
    }

    /** 目录选择器结果统一入口。返回 true=已消费；false=交由 Activity 默认处理。 */
    fun onActivityResult(
        activity: Activity,
        requestCode: Int,
        resultCode: Int,
        data: Intent?,
    ): Boolean {
        if (requestCode != REQUEST_PICK_DIR) return false
        val result = pendingResult ?: return true
        pendingResult = null
        val uri = data?.data
        if (resultCode != Activity.RESULT_OK || uri == null) {
            result.success(null) // 用户取消
            return true
        }
        try {
            activity.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        } catch (_: Exception) {
            // 持久化授权失败不致命：路径写入依赖文件系统权限而非 SAF 授权
        }
        // null = 用户取消；"" = 无法映射为文件系统路径（Dart 侧提示重选）
        result.success(treeUriToPath(uri) ?: "")
        return true
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
    fun treeUriToPath(uri: Uri): String? {
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

    // ── 目录选择（SAF） ──

    private fun pickDirectory(activity: Activity, result: MethodChannel.Result) {
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
            activity.startActivityForResult(intent, REQUEST_PICK_DIR)
        } catch (e: Exception) {
            pendingResult = null
            result.error("unavailable", e.message, null)
        }
    }

    // ── 公共目录写权限 ──

    private fun hasAllFilesAccess(activity: Activity): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            activity.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) ==
                PackageManager.PERMISSION_GRANTED
        }

    private fun requestAllFilesAccess(activity: Activity) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            try {
                activity.startActivity(
                    Intent(
                        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:${activity.packageName}"),
                    ),
                )
            } catch (_: Exception) {
                // 个别 ROM 不支持带包名的入口，退回总开关页
                try {
                    activity.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                } catch (_: Exception) {
                }
            }
        } else {
            activity.requestPermissions(
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
    private fun installApk(activity: Activity, path: String?, result: MethodChannel.Result) {
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
                activity,
                "${activity.packageName}.fileprovider",
                file,
            )
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            activity.startActivity(intent)
            result.success(true)
        } catch (e: Exception) {
            result.error("install_failed", e.message, null)
        }
    }

    // ── 打开已下载文件 ──

    /**
     * 用系统默认应用打开文件：经 FileProvider 暴露 content:// URI + ACTION_VIEW。
     * targetSdk ≥ 24 禁止把 file:// URI 递出应用，必须走 content://。
     * 无应用声明处理该 MIME（或系统拒绝）时，放宽为 星/星 并经系统选择器
     * （Intent.createChooser）让用户自选应用。
     * 错误码：bad_args / not_found / open_failed / no_handler，Dart 侧据此提示。
     */
    private fun openFile(activity: Activity, path: String?, result: MethodChannel.Result) {
        if (path.isNullOrEmpty()) {
            result.error("bad_args", "path is required", null)
            return
        }
        val file = java.io.File(path)
        if (!file.exists()) {
            result.error("not_found", "file not found: $path", null)
            return
        }
        val uri = try {
            androidx.core.content.FileProvider.getUriForFile(
                activity,
                "${activity.packageName}.fileprovider",
                file,
            )
        } catch (e: IllegalArgumentException) {
            // 路径不在 file_provider_paths 声明的根之内（如部分 SD 卡挂载点）
            result.error("open_failed", e.message, null)
            return
        }
        val mime = android.webkit.MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(file.extension.lowercase()) ?: "*/*"
        val view = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, mime)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            activity.startActivity(view)
            result.success(true)
        } catch (_: ActivityNotFoundException) {
            // 没有应用能直接处理该 MIME：放宽类型并让用户从选择器挑应用
            val fallback = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "*/*")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            try {
                val chooser = Intent.createChooser(fallback, null).apply {
                    addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                }
                activity.startActivity(chooser)
                result.success(true)
            } catch (e: Exception) {
                result.error("no_handler", e.message, null)
            }
        } catch (e: Exception) {
            result.error("open_failed", e.message, null)
        }
    }
}