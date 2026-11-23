package com.fluxdown.app.ui

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Environment
import android.os.storage.StorageManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import java.io.File
import java.io.IOException

/**
 * 本机主机的默认保存目录：应用专属外部目录（无需存储权限，卸载时随应用清理）。
 * 与首次启动播种给引擎的 `default_save_dir` 同源，“恢复默认”也回到这里。
 */
fun defaultLocalSaveDir(context: Context): File =
    context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: File(context.filesDir, "downloads")

/** 两种内部数据根都包含各自的 files、cache、no_backup；不要调用会创建子目录的 getter。 */
internal fun externalLocalDirectory(context: Context): ExternalLocalDirectory = ExternalLocalDirectory(
    listOf(context.dataDir, context.createDeviceProtectedStorageContext().dataDir),
)

/**
 * 系统公共下载根目录（`/storage/emulated/0/Download`）。
 * 本机引擎按文件路径写入：Android 11+ 需要所有文件访问，Android 8–10 需要存储运行时权限。
 */
fun publicDownloadSaveDir(): File =
    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)

/** 一个已挂载的存储卷根目录；[label] 为系统给出的卷名（主存储用共享文案）。 */
internal class StorageRoot(val label: String, val path: String, val removable: Boolean)

/** 目录浏览与授权判定共用的存储布局：主存储、已挂载卷、无需权限的应用自有目录。 */
internal class StorageLayout(val primary: File, val volumes: List<StorageRoot>, val appRoots: List<File>) {
    val volumeFiles: List<File> get() = volumes.map { File(it.path) }
}

internal fun storageLayout(context: Context): StorageLayout {
    val primary = Environment.getExternalStorageDirectory()
    val internalLabel = context.str(R.string.mobileLocalFolderInternal)
    val sdLabel = context.str(R.string.mobileLocalFolderSdCard)
    val externalFiles = context.getExternalFilesDirs(null).filterNotNull()
    val volumes = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        context.getSystemService(StorageManager::class.java).storageVolumes
            .filter { it.state == Environment.MEDIA_MOUNTED || it.state == Environment.MEDIA_MOUNTED_READ_ONLY }
            .mapNotNull { volume ->
                val dir = volume.directory ?: return@mapNotNull null
                val label = if (volume.isPrimary) internalLabel else volume.getDescription(context).ifBlank { sdLabel }
                StorageRoot(label, dir.path, !volume.isPrimary)
            }
    } else {
        // 8–10 无 StorageVolume.directory：由 /storage/<volume>/Android/data/<package>/files 反推卷根。
        externalFiles
            .filter { Environment.getExternalStorageState(it) == Environment.MEDIA_MOUNTED }
            .mapNotNull { it.parentFile?.parentFile?.parentFile?.parentFile }
            .map { root ->
                val isPrimary = root.path == primary.path
                StorageRoot(if (isPrimary) internalLabel else sdLabel, root.path, !isPrimary)
            }
    }.distinctBy { it.path }
    val appRoots = externalFiles.mapNotNull { it.parentFile } + context.filesDir + context.cacheDir
    val withPrimary = if (volumes.any { it.path == primary.path }) volumes else listOf(StorageRoot(internalLabel, primary.path, false)) + volumes
    return StorageLayout(primary, withPrimary, appRoots)
}

/** 按路径写入共享存储的授权：Android 11+ 为「所有文件访问」，更低版本为存储运行时权限（Android 10 另靠 legacy 存储）。 */
internal fun hasSharedStorageAccess(context: Context): Boolean =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        Environment.isExternalStorageManager()
    } else {
        ContextCompat.checkSelfPermission(context, Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
    }

/** 共享存储里的目录且尚未授权：浏览 / 保存前需要先授权。 */
internal fun canRequestStorageAccess(context: Context, layout: StorageLayout, path: String): Boolean =
    !hasSharedStorageAccess(context) && isSharedStorageAccessPath(path, layout.primary, layout.volumeFiles, layout.appRoots)

/**
 * 共享存储授权：Android 11+ 直接打开系统「所有文件访问」页（不可用时退回系统总列表，仍不可用则提示），
 * 返回时重新核验；更低版本申请存储运行时权限。结果经最新 [onResult] 回调。说明文案由调用方就地展示。
 */
@Composable
internal fun rememberStorageAccessRequest(onResult: (Boolean) -> Unit): () -> Unit {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val noSettingsApp = str(R.string.mobileNoSettingsApp)
    val result by rememberUpdatedState(onResult)
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok -> result(ok) }
    val allFiles = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        result(hasSharedStorageAccess(context))
    }
    return {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            try {
                permission.launch(Manifest.permission.WRITE_EXTERNAL_STORAGE)
            } catch (_: ActivityNotFoundException) {
                overlays.toast(noSettingsApp, FluxToastKind.Error)
            }
        } else {
            try {
                allFiles.launch(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, "package:${context.packageName}".toUri()))
            } catch (_: ActivityNotFoundException) {
                try {
                    allFiles.launch(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                } catch (_: ActivityNotFoundException) {
                    overlays.toast(noSettingsApp, FluxToastKind.Error)
                }
            }
        }
    }
}

/**
 * 本机主机的保存目录能否写入（不存在则尝试创建）。
 *
 * Android 分区存储下，应用无权限的目录（如拼错的 `Android/dta/...`、其他应用的私有目录）
 * 创建 / 写入会得到 EPERM，引擎只能在下载开始后报 `Operation not permitted`；提交前在这里拦住。
 * 远端主机的路径在服务器上，不能在手机侧校验。
 */
fun isLocalDirWritable(path: String): Boolean {
    if (path.isBlank()) return true
    return try {
        prepareLocalDirectory(path.trim())
        true
    } catch (_: IOException) {
        false
    }
}

/** [friendlyPath] 的上下文封装:应用专属目录优先于所在存储卷,其余按卷名显示。 */
fun friendlyLocalPath(context: Context, path: String): String {
    val layout = storageLayout(context)
    val appLabel = context.str(R.string.mobileLocalFolderAppShort)
    val roots = context.getExternalFilesDirs(null).filterNotNull().map { appLabel to it.path } +
        layout.volumes.map { it.label to it.path }
    return friendlyPath(path, roots)
}

/**
 * 启动系统文件选择器（`ACTION_OPEN_DOCUMENT`）。部分精简 ROM 去掉了 DocumentsUI，没有任何 Activity 能处理该意图，
 * `launch` 会抛 [ActivityNotFoundException]：这里接住并提示，而不是让应用崩溃。
 */
internal inline fun launchDocumentPicker(overlays: FluxOverlayState, noPickerText: String, launch: () -> Unit) {
    try {
        launch()
    } catch (_: ActivityNotFoundException) {
        overlays.toast(noPickerText, FluxToastKind.Error)
    }
}
