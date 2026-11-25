package com.fluxdown.app.feature.newtask

import com.fluxdown.core.capture.ExternalDownload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NewDownloadStateRangeTest {
    private fun form(prefill: String = "") = NewDownloadState(
        prefill = prefill,
        defaultSaveDir = "/dl",
        offeredSaveDir = "/dl",
        defaultQueueId = "",
        defaultSegments = 0,
    )

    @Test fun previewAndRequestsUseExpandedUrlsWithoutCloningSingleItemOverrides() {
        val form = form("https://example.com/[001-215].zip\n  out=fixed.zip\n  checksum=sha-256:fixed")
        form.rename = "override.zip"
        form.advanced.checksumHex = "abcd"
        assertTrue(form.hasUrlRanges)
        assertFalse(form.single)
        assertEquals(listOf("https://example.com/001.zip", "https://example.com/215.zip"), form.rangePreview.map { it.url })
        assertEquals(listOf("001.zip", "215.zip"), form.rangePreview.map { it.batchExportName() })
        val requests = form.buildRequests(false, "", "")
        assertEquals(215, requests.size)
        assertTrue(requests.all { it.fileName.isEmpty() && it.checksum.isEmpty() })
        val dispatch = form.dispatchItems()
        assertEquals(requests.map { it.url }, dispatch.map { it.entry.url })
        assertTrue(dispatch.all { it.fileName == null })
    }

    @Test fun singletonRangeKeepsFormOverrides() {
        val form = form("https://example.com/[001-001].zip")
        form.rename = "single.zip"
        val digest = "ab".repeat(32)
        form.advanced.checksumHex = digest
        val request = form.buildRequests(false, "", "").single()
        assertEquals("https://example.com/001.zip", request.url)
        assertEquals("single.zip", request.fileName)
        assertEquals("sha-256=$digest", request.checksum)
        assertEquals("single.zip", form.dispatchItems().single().fileName)
    }

    @Test fun overflowBlocksBothCurrentHostAndRemoteRequestsAndRecoversAfterEditing() {
        val form = form("https://example.com/plain.zip\nhttps://example.com/[001-1001].zip")
        assertTrue(form.rangeLimitExceeded)
        assertTrue(form.entries.isEmpty())
        assertTrue(form.buildRequests(false, "", "").isEmpty())
        assertTrue(form.dispatchItems().isEmpty())
        form.urlText = "https://example.com/[001-002].zip"
        assertFalse(form.rangeLimitExceeded)
        assertEquals(2, form.buildRequests(false, "", "").size)
    }

    @Test fun importedAndCapturedRangesRemainLiteralAlongsideManualTemplates() {
        val form = form()
        val imported = "https://example.com/imported[0001-9999].zip"
        val captured = "https://example.com/captured[0001-9999].zip"
        val digest = "sha-256:imported"
        val found = parseEntries("source: $imported\n  out=imported.zip\n  checksum=$digest", loose = true)
        assertEquals(1, form.addImported(found))
        form.addExternal(ExternalDownload(captured, fileName = "capture.zip", cookies = "session=1"))
        form.appendText("https://example.com/manual[01-02].zip")
        assertFalse(form.rangeLimitExceeded)
        assertEquals(4, form.entries.size)
        assertEquals(listOf("https://example.com/manual01.zip", "https://example.com/manual02.zip"), form.rangePreview.map { it.url })
        val requests = form.buildRequests(false, "", "").associateBy { it.url }
        assertEquals("imported.zip", requests[imported]?.fileName)
        assertEquals(digest, requests[imported]?.checksum)
        assertEquals("capture.zip", requests[captured]?.fileName)
        assertEquals("session=1", requests[captured]?.cookies)
        val items = form.dispatchItems().associateBy { it.entry.url }
        assertEquals("imported.zip", items[imported]?.fileName)
        assertEquals("capture.zip", items[captured]?.fileName)
        form.keepOnly(setOf(imported, captured))
        assertFalse(form.hasUrlRanges)
        assertEquals(listOf(imported, captured), form.entries.map { it.url })
        assertEquals(digest, form.buildRequests(false, "", "").first().checksum)
    }

    @Test fun importedDuplicatesDoNotChangeAManualTemplateAndDeletedImportsDoNotMarkNewInput() {
        val form = form()
        val manual = "https://example.com/[1-2].zip"
        val imported = "https://example.com/imported[1-2].zip"
        form.appendText(manual)
        assertEquals(0, form.addImported(listOf(UrlEntry(manual))))
        assertEquals(2, form.entries.size)
        assertEquals(1, form.addImported(listOf(UrlEntry(imported))))
        assertEquals(0, form.addImported(listOf(UrlEntry(imported))))
        assertEquals(3, form.entries.size)
        form.urlText = manual
        form.appendText(imported)
        assertEquals(4, form.entries.size)
    }

    @Test fun failedCommandEntryStaysLiteralWithItsOwnMetadataOnRemoteRetry() {
        val commandUrl = "https://example.com/command[0001-9999].zip"
        val form = form(
            "aria2c \\\n$commandUrl\n  out=command.zip\n  checksum=sha-256:command\nhttps://example.com/manual[01-02].zip",
        )
        assertFalse(form.rangeLimitExceeded)
        val failed = form.dispatchItems().single { it.entry.url == commandUrl }
        form.retain(listOf(failed.entry))
        assertFalse(form.rangeLimitExceeded)
        assertFalse(form.hasUrlRanges)
        assertEquals(failed, form.dispatchItems().single())
        assertEquals("sha-256:command", form.buildRequests(false, "", "").single().checksum)
    }

    @Test fun failedGeneratedLinkDoesNotInheritUnusedBatchOverrides() {
        val form = form("https://example.com/[1-2].zip\n  out=also-ignored.zip\n  checksum=sha-256:ignored")
        form.rename = "ignored.zip"
        form.advanced.checksumHex = "ab".repeat(32)
        form.keepOnly(setOf("https://example.com/2.zip"))
        assertFalse(form.hasUrlRanges)
        val retried = form.buildRequests(false, "", "").single()
        assertEquals("https://example.com/2.zip", retried.url)
        assertTrue(retried.fileName.isEmpty())
        assertTrue(retried.checksum.isEmpty())
    }

    @Test fun expandedRequestsKeepConfirmedDirectoryAndManualRoutingAuthorization() {
        val form = form("https://example.com/[1-2].zip")
        assertTrue(form.buildRequests(false, "", "").all { it.offeredSaveDir == "/dl" })
        form.setSaveDirManually("/dl/raw")
        val requests = form.buildRequests(false, "", "", resolvedSaveDir = "/dl/confirmed")
        assertEquals(2, requests.size)
        assertTrue(requests.all { it.saveDir == "/dl/confirmed" })
        requests.forEach { assertNull(it.offeredSaveDir) }
    }
}
