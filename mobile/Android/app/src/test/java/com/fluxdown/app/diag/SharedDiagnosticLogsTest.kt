package com.fluxdown.app.diag

import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.IOException

class SharedDiagnosticLogsTest {
    @get:Rule val folder = TemporaryFolder()

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
