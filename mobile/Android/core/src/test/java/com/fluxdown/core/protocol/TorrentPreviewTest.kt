package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TorrentPreviewTest {
    @Test fun previewRetainsOriginalIndicesUnicodeAndLargeSizes() {
        val preview = TorrentPreviewDto.fromJson(Json.parse("""{"torrentBlobId":"torrent:test","name":"资料","totalBytes":5000000000,"files":[{"index":2,"path":"目录/a.mkv","size":5000000000}]}"""))!!
        assertEquals(2, preview.files.single().index)
        assertEquals("目录/a.mkv", preview.files.single().path)
        assertEquals(5000000000L, preview.totalBytes)
    }

    @Test fun confirmationKeepsSourceAndDraftAndOmitsMissingOptions() {
        val params = TorrentConfirmParams("torrent:test", "/downloads", listOf(2), queueId = "queue", startPaused = true,
            customName = "资料", sourceUrl = "magnet:?xt=test", options = jsonObject("url" to "magnet:?xt=test", "userAgent" to "draft"))
        val wire = Json.parse(params.toJson().toJson())
        assertEquals("magnet:?xt=test", wire.str("sourceUrl"))
        assertEquals("draft", wire["options"].str("userAgent"))
        assertEquals(true, wire.bool("startPaused"))
        assertNull(TorrentConfirmParams("torrent:test", "/downloads", listOf(2)).toJson()["options"])
    }
}
