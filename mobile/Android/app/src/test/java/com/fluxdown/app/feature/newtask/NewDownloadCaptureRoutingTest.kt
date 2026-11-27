package com.fluxdown.app.feature.newtask

import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.store.HostState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NewDownloadCaptureRoutingTest {
    @Test fun userCanOverrideAnUnavailableTargetWithoutLosingRequestContext() {
        val request = ExternalDownload(
            "https://x.test/object", fileName = "安装包.apk", cookies = "sid=private",
            referrer = "https://x.test/page", headers = mapOf("Authorization" to "Bearer secret"),
        )
        val form = NewDownloadState.create("", HostState(), external = request, targetId = "cloud:missing")
        assertEquals("cloud:missing", form.targetId)
        assertNull(form.target(emptyList()))
        assertTrue(form.hasUnsupportedCaptureForDispatch())
        assertEquals("安装包.apk", form.previewName(form.entries.single()))

        form.targetId = null
        val local = form.buildRequests(false, "", "").single()
        assertEquals(request.url, local.url)
        assertEquals(request.fileName, local.fileName)
        assertEquals(request.cookies, local.cookies)
        assertEquals(request.referrer, local.referrer)
        assertEquals(request.headers, local.headers)
    }

    @Test fun dispatchRetryRetainsOnlyFailedEntriesAndTheirSuggestedNames() {
        val target = DispatchTarget(DispatchTarget.Kind.Cloud, "phone", "Phone", false, null, null)
        val form = NewDownloadState.create("", HostState(), targetId = target.id)
        form.addExternal(ExternalDownload("https://x.test/a", "already-sent.apk"))
        form.addExternal(ExternalDownload("https://x.test/b", "try-again.apk"))
        assertEquals(target, form.target(listOf(target)))
        assertFalse(form.hasUnsupportedCaptureForDispatch())

        form.retain(listOf(form.entries.last()))
        val remaining = form.dispatchItems().single()
        assertEquals("https://x.test/b", remaining.entry.url)
        assertEquals("try-again.apk", remaining.fileName)
        assertEquals(target, form.target(listOf(target)))
    }
}
