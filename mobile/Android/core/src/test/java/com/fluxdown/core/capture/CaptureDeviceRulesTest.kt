package com.fluxdown.core.capture

import com.fluxdown.core.capture.CaptureDeviceRules.Rule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureDeviceRulesTest {
    private val rules = CaptureDeviceRules.parse("apk=cloud:phone\nexe=link:pc")

    @Test fun malformedLinesAreSkippedAndTheFirstValidRuleWins() {
        val parsed = CaptureDeviceRules.parse(
            "bad line\r\n..apk=cloud:wrong\n.apk=cloud:\nApK=local:phone\n .APK = cloud:PHONE \r\napk=link:late\nEXE=link:pc",
        )
        assertEquals(listOf(Rule("apk", "cloud:PHONE"), Rule("apk", "link:late"), Rule("exe", "link:pc")), parsed)
        assertEquals("cloud:PHONE", CaptureDeviceRules.targetId(parsed, ExternalDownload("https://x.test/app.apk")))
        assertEquals(parsed, CaptureDeviceRules.parse(CaptureDeviceRules.encode(parsed)))
        assertTrue(CaptureDeviceRules.parse("").isEmpty())
        assertNull(CaptureDeviceRules.normalizeExtension("..APK"))
        assertNull(CaptureDeviceRules.normalizeExtension("éxe"))
    }

    @Test fun suggestedFilenameOverridesTheDecodedHttpPath() {
        val link = "HTTPS://例え.test/下载%2eAPK?save_as=other.exe#wrong.stem"
        assertEquals("cloud:phone", CaptureDeviceRules.targetId(rules, ExternalDownload(link)))
        assertEquals("link:pc", CaptureDeviceRules.targetId(rules, ExternalDownload(link, " 安装包.EXE ")))
        assertNull(CaptureDeviceRules.targetId(rules, ExternalDownload(link, "no-extension")))
        assertNull(CaptureDeviceRules.targetId(rules, ExternalDownload(link, "directory/file.apk")))
        for (suffix in listOf(".apk", "file.apk.", "file.%20apk", "dir%2Fapp.apk", "file.apk/", "?name=file.apk")) {
            assertNull("unexpected match for $suffix", CaptureDeviceRules.targetId(rules, ExternalDownload("https://x.test/$suffix")))
        }
        assertNull(CaptureDeviceRules.targetId(rules, ExternalDownload("ftp://x.test/app.apk", "app.exe")))
    }

    @Test fun mixedSharesUseEachLinksOwnTarget() {
        val shares = ExternalIntake.fromSharedText("https://x.test/a.apk\nhttps://x.test/b.exe https://x.test/readme.txt\nhttps://x.test/c.apk")
        assertEquals(
            listOf("cloud:phone", "link:pc", null, "cloud:phone"),
            shares.map { CaptureDeviceRules.targetId(rules, it) },
        )
    }

    @Test fun automaticRemoteDispatchNeverDropsCredentialsOrCustomHeaders() {
        val portable = ExternalDownload("https://x.test/app.apk", "app.apk")
        assertTrue(CaptureDeviceRules.canDispatch(portable))
        assertTrue(CaptureDeviceRules.canDispatch(portable.copy(referrer = "https://x.test/", headers = mapOf("user-agent" to "UA"))))
        val unsafe = listOf(
            portable.copy(cookies = "sid=private"),
            portable.copy(headers = mapOf("Authorization" to "Bearer secret")),
            portable.copy(headers = mapOf("Referer" to "https://x.test/")),
            portable.copy(headers = mapOf("Origin" to "https://x.test")),
            portable.copy(url = "https://user:pass@x.test/app.apk"),
            portable.copy(url = "ftp://x.test/app.apk"),
            portable.copy(hasUnusableRequestContext = true),
        )
        for (capture in unsafe) assertFalse(CaptureDeviceRules.canDispatch(capture))
    }
}
