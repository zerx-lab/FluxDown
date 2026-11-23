package com.fluxdown.app.diag

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.IOException
import java.io.File

class SharedDiagnosticLogsTest {
    @get:Rule val folder = TemporaryFolder()

    @Test
    fun cleanupDeletesOnlyRecognizedLegacyLogFiles() {
        val noBackup = folder.newFolder()
        val files = folder.newFolder()
        fun log(root: File, name: String) = File(root, name).apply {
            parentFile?.mkdirs()
            writeText("diagnostic data")
        }

        val legacy = listOf(
            log(noBackup, "diagnostics/app-events.log.1"),
            log(files, "fluxdown/logs/fluxdown_2025-01-02.1.log"),
            log(files, "fluxdown/agent/logs/agent.log"),
        )
        val retained = listOf(
            log(noBackup, "diagnostics/redacted/app-events.log"),
            log(files, "fluxdown/diagnostic-logs/engine/fluxdown_2025-01-02.log"),
            log(files, "fluxdown/logs/notes.txt"),
            log(files, "fluxdown/agent/logs/other/agent.log"),
        )
        repeat(2) { removeLegacyDiagnosticLogs(noBackup, files) { throw it } }

        legacy.forEach { assertFalse(it.path, it.exists()) }
        retained.forEach { assertTrue(it.path, it.isFile) }
    }

    @Test
    fun snapshotKeepsCompleteLinesAcrossRotations() {
        val rotated = folder.newFile().apply { writeText("discard\nold\n") }
        val current = folder.newFile().apply { writeText("前缀\nnew\npartial") }
        val result = snapshotDiagnosticLogs(listOf(rotated, current), maxTailBytes = 13)
        assertEquals("discard\nold\nnew\n", result?.decodeToString())
        assertEquals("old\n", snapshotDiagnosticLogs(listOf(rotated), maxTailBytes = 4)?.decodeToString())
    }

    @Test
    fun failedPromotionPreservesLastCompleteCopyThroughRetry() {
        val files = mutableMapOf("current" to "last complete", "previous" to "older", "pending" to "new")
        var failPromotion = true
        fun promote() = promoteDiagnosticLog(
            "pending", "current".takeIf(files::containsKey), "previous", "current", "previous",
            delete = { files.remove(it) },
            rename = { source, target ->
                if (source == "pending" && failPromotion) throw IOException("provider stopped")
                files[target] = checkNotNull(files.remove(source))
            },
        )

        try {
            promote()
            fail("Expected promotion to fail")
        } catch (_: IOException) {
            assertEquals(mapOf("previous" to "last complete", "pending" to "new"), files)
        }
        failPromotion = false
        promote()
        assertEquals(mapOf("previous" to "last complete", "current" to "new"), files)
    }
}
