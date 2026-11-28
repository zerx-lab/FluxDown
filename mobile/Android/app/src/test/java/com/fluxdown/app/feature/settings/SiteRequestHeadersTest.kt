package com.fluxdown.app.feature.settings

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SiteRequestHeadersTest {
    @Test
    fun siteAndSourceAreSeparateAndOtherHeaderValuesRoundTrip() {
        val origin = "https://例え.example:8443"
        val referer = "https://source.example/日本語?q=${"x".repeat(1024)}"
        val extra = "X-Example:  a:  b\nX-Locale: עברית"
        val draft = requireNotNull(draftRule(origin, referer, extra))

        assertEquals(origin, draft.origin)
        assertEquals(referer, draft.referer)
        assertEquals(" a:  b", draft.headers["X-Example"])
        assertEquals(extra, draft.extra)
        assertEquals(listOf(draft), parseSiteHeaderRules("[${draft.toJson().toJson()}]"))
    }

    @Test
    fun rejectsMalformedExtrasAndDoesNotTreatCorruptConfigAsEmpty() {
        assertNull(draftRule("https://files.example", "", "X-Example"))
        assertNull(draftRule("https://files.example", "", "X-Example: a\nx-example: b"))
        assertNull(draftRule("https://files.example", "", "referer: https://source.example"))
        assertNull(parseSiteHeaderRules("[{\"origin\":\"https://files.example\"}]"))
        assertEquals(emptyList<SiteHeaderRule>(), parseSiteHeaderRules(""))
    }
}
