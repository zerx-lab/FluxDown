package com.fluxdown.app.feature.newtask

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import com.fluxdown.app.AppContainer
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.capture.TorrentFile
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.callUnit
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import java.io.ByteArrayOutputStream
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * `.torrent` 导入（同 iOS `TorrentImport`）：每个文件在 IO 线程读取（至多 `MAX_BYTES + 1` 字节）→ 粗校验 →
 * `daemon.task.create{request.torrentB64}`。本机与远端主机同一路径；单个失败不影响后续文件。
 */
object TorrentImport {
    /** 返回成功建出的任务数；失败与成功都已各自弹 toast。 */
    suspend fun submit(
        context: Context,
        container: AppContainer,
        overlays: FluxOverlayState,
        errorText: (HostException) -> String,
        uris: List<Uri>,
        saveDir: String,
        queueId: String,
        startPaused: Boolean,
        session: HostSession = container.session,
        beforeCreate: (suspend () -> Boolean)? = null,
    ): Int {
        if (uris.isEmpty()) return 0
        if (beforeCreate == null && container.store.state.value.isReadOnly) {
            overlays.toast(context.str(R.string.localServiceDisconnected), FluxToastKind.Error)
            return 0
        }
        val dir = saveDir.trim()
        var created = 0
        for (uri in uris) {
            val name = displayName(context, uri)
            val data = withContext(Dispatchers.IO) { readBytes(context, uri) }
            if (data == null) {
                overlays.toast(context.str(R.string.torrentImportReadFailed, "name" to name), FluxToastKind.Error)
                continue
            }
            val issue = TorrentFile.validate(data)
            if (issue != null) {
                val message = when (issue) {
                    TorrentFile.Issue.Empty -> context.str(R.string.torrentImportEmpty, "name" to name)
                    TorrentFile.Issue.TooLarge -> context.str(R.string.torrentImportTooLarge, "name" to name)
                    TorrentFile.Issue.NotTorrent -> context.str(R.string.torrentImportNotTorrent, "name" to name)
                }
                overlays.toast(message, FluxToastKind.Warn)
                continue
            }
            val params = TorrentFile.createParams(data, dir, queueId, startPaused)
            if (beforeCreate != null && !beforeCreate()) break
            try {
                session.callUnit(HostMethod.daemonTaskCreate, params)
                created++
            } catch (e: HostException) {
                overlays.toast(
                    context.str(R.string.torrentImportFailed, "name" to name, "error" to errorText(e)),
                    FluxToastKind.Error,
                )
            }
        }
        if (created > 0) {
            val text = if (created == 1) {
                context.str(R.string.torrentFileSelected)
            } else {
                context.str(R.string.torrentFileCount, "count" to created)
            }
            overlays.toast(text, FluxToastKind.Success)
        }
        return created
    }

    /** 至多读 `MAX_BYTES + 1` 字节：足以判定「过大」而不把大文件读进内存；读不到返回 null。 */
    private fun readBytes(context: Context, uri: Uri): ByteArray? = try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            val limit = TorrentFile.MAX_BYTES + 1
            val out = ByteArrayOutputStream()
            val buf = ByteArray(16 * 1024)
            while (out.size() < limit) {
                val n = input.read(buf, 0, minOf(buf.size, limit - out.size()))
                if (n < 0) break
                out.write(buf, 0, n)
            }
            out.toByteArray()
        }
    } catch (_: IOException) {
        null
    } catch (_: SecurityException) {
        null
    }

    /** 文件显示名：content URI 取 `DISPLAY_NAME`，否则取路径末段。 */
    fun displayName(context: Context, uri: Uri): String {
        if (uri.scheme == "content") {
            try {
                context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                    if (c.moveToFirst()) c.getString(0)?.takeIf { it.isNotBlank() }?.let { return it }
                }
            } catch (_: SecurityException) {
                // 无权读取元数据：退回路径末段
            } catch (_: IllegalArgumentException) {
            }
        }
        return uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() } ?: uri.toString()
    }
}
