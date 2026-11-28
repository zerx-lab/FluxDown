package com.fluxdown.app.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import java.io.IOException
import java.io.OutputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal data class TaskInfoRow(val name: String, val url: String, val size: Long? = null)

internal data class TaskInfoLabels(val name: String, val url: String, val size: String)

internal fun taskInfoText(rows: List<TaskInfoRow>, labels: TaskInfoLabels): String = buildString {
    fun cell(value: String) {
        for (ch in value) {
            when (ch) {
                '\\' -> append("\\\\")
                '\t' -> append("\\t")
                '\r' -> append("\\r")
                '\n' -> append("\\n")
                else -> append(ch)
            }
        }
    }
    cell(labels.name)
    append('\t')
    cell(labels.url)
    append('\t')
    cell(labels.size)
    for (row in rows) {
        append('\n')
        cell(row.name)
        append('\t')
        if (!row.url.startsWith("torrent-file://", ignoreCase = true)) cell(row.url)
        append('\t')
        row.size?.let { append(it) }
    }
}

internal fun writeTaskInfo(stream: OutputStream, text: String) {
    stream.bufferedWriter(Charsets.UTF_8).use { it.write(text) }
}

@Composable
internal fun rememberTaskInfoExport(): (List<TaskInfoRow>) -> Unit {
    val context = LocalContext.current
    val scope = LocalAppContainer.current.appScope
    val overlays = LocalFluxOverlays.current
    val failed = str(R.string.taskInfoExportFailed)
    val labels = TaskInfoLabels(
        str(R.string.taskInfoColumnName),
        str(R.string.taskInfoColumnUrl),
        str(R.string.taskInfoColumnSize),
    )
    var pending by remember { mutableStateOf<String?>(null) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        val text = pending
        pending = null
        if (uri != null && text != null) {
            scope.launch {
                try {
                    withContext(Dispatchers.IO) {
                        val stream = context.contentResolver.openOutputStream(uri, "wt")
                            ?: throw IOException("No output stream")
                        writeTaskInfo(stream, text)
                    }
                } catch (_: IOException) {
                    overlays.toast(failed, FluxToastKind.Error)
                } catch (_: SecurityException) {
                    overlays.toast(failed, FluxToastKind.Error)
                }
            }
        }
    }
    return { rows ->
        if (pending == null && rows.isNotEmpty()) {
            pending = taskInfoText(rows, labels)
            try {
                launcher.launch("fluxdown-tasks.txt")
            } catch (_: android.content.ActivityNotFoundException) {
                pending = null
                overlays.toast(failed, FluxToastKind.Error)
            }
        }
    }
}
