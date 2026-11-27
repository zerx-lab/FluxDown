package com.fluxdown.app

import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.core.capture.ExternalDownload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ExternalConfirmationQueueTest {
    @Test fun differentDirectoriesAreConfirmedInOrderAndTorrentCannotCloseTheExitingSheet() {
        val confirmations = ExternalConfirmationQueue()
        var current: ExternalConfirmation? = null
        var windowCloses = 0
        val torrents = ExternalTorrentQueue<Any>(
            closeNow = { windowCloses++ },
            closeAfterToast = {},
            cancelClose = {},
            hasOtherSheet = { confirmations.hasOtherSheet(current != null) },
        )
        fun request(dir: String, unwritableDir: String? = null) =
            ExternalConfirmation(
                SheetRoute.NewDownload(external = listOf(ExternalDownload("https://x.test/archive.zip", saveDir = dir))),
                unwritableDir,
            )
        fun arrive(confirmation: ExternalConfirmation) {
            confirmations.offer(current != null, confirmation)?.let { current = it }
        }
        fun closeSheet() {
            confirmations.beginClosing()
            current = null
        }
        fun finishExit() {
            current = confirmations.afterClosing()
            if (current == null) torrents.closeIfIdle()
        }

        arrive(request("/first"))
        arrive(request("/second", unwritableDir = "/second"))
        assertEquals("/first", current?.route?.external?.single()?.saveDir)
        closeSheet()
        arrive(request("/third"))
        torrents.closeIfIdle()
        assertNull(current)
        assertEquals(0, windowCloses)

        finishExit()
        assertEquals("/second", current?.route?.external?.single()?.saveDir)
        assertEquals("/second", current?.unwritableDir)
        closeSheet()
        finishExit()
        assertEquals("/third", current?.route?.external?.single()?.saveDir)
        assertNull(current?.unwritableDir)
        closeSheet()
        torrents.closeIfIdle()
        assertEquals(0, windowCloses)

        finishExit()
        assertNull(current)
        assertEquals(1, windowCloses)
    }
}
