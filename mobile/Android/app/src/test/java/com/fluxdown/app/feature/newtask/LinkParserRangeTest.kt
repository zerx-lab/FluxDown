package com.fluxdown.app.feature.newtask

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LinkParserRangeTest {
    @Test fun expandsAscendingFilenameAndPreservesStartingWidthAndSuffix() {
        val parsed = parseManualEntries("https://example.com/章/[001-215].tar.gz?token=a%2Fb#part")
        assertEquals(215, parsed.generatedCount)
        assertEquals(215, parsed.entries.size)
        assertEquals("https://example.com/章/001.tar.gz?token=a%2Fb#part", parsed.entries.first().url)
        assertEquals("https://example.com/章/215.tar.gz?token=a%2Fb#part", parsed.entries.last().url)
        assertFalse(parsed.rangeLimitExceeded)
    }

    @Test fun usesStartWidthRatherThanEndWidthAndAllowsDigitGrowth() {
        assertEquals(
            listOf("http://example.com/9.zip", "http://example.com/10.zip"),
            parseManualEntries("http://example.com/[9-010].zip").entries.map { it.url },
        )
    }

    @Test fun supportsLargeIntegersAndWideZeroPadding() {
        val start = "9".repeat(40)
        val end = "1" + "0".repeat(40)
        val large = parseManualEntries("https://example.com/[$start-$end].zip")
        assertEquals(listOf("https://example.com/$start.zip", "https://example.com/$end.zip"), large.entries.map { it.url })
        val padded = "0".repeat(80)
        val wide = parseManualEntries("https://example.com/[${padded}1-${padded}2].zip")
        assertEquals("https://example.com/${padded}1.zip", wide.entries.first().url)
        assertEquals("https://example.com/${padded}2.zip", wide.entries.last().url)
    }

    @Test fun leavesMalformedDescendingAndMultipleRangesLiteral() {
        val filenames = listOf(
            "[3-1].zip", "[-1-3].zip", "[1-].zip", "[1–3].zip", "[1-2-3].zip",
            "[１-３].zip", "[1-2][3-4].zip", "[[1-3]].zip", "[1-3]x[broken].zip",
            "%5B1-3%5D.zip", "[1-3.zip", "1-3].zip",
        )
        for (filename in filenames) {
            val url = "https://example.com/$filename"
            val parsed = parseManualEntries(url)
            assertEquals(filename, listOf(UrlEntry(url)), parsed.entries)
            assertEquals(filename, 0, parsed.generatedCount)
        }
    }

    @Test fun expandsOnlyHttpFilenameNotDirectoryAuthorityQueryFragmentOrOtherProtocols() {
        val urls = listOf(
            "https://[1-3].example.com/file.zip", "https://example.com/[1-3]/file.zip",
            "https://example.com/[1-3]/", "https://example.com/file.zip?q=[1-3]",
            "https://example.com/file.zip#[1-3]", "ftp://example.com/[1-3].zip",
            "magnet:?xt=urn:btih:abc&dn=[1-3].zip", "ed2k://|file|[1-3].zip|1|abc|/",
        )
        assertEquals(urls, parseManualEntries(urls.joinToString("\n")).entries.map { it.url })
        assertEquals(2, parseManualEntries("HTTPS://[::1]/file[0-1].zip").entries.size)
        assertEquals(2, parseManualEntries("https://example.com/[1-2]?literal=[3-4]#[5-6]").entries.size)
    }

    @Test fun preservesUnicodeAndVeryLongQueriesWithoutDecoding() {
        val suffix = "?token=" + "x".repeat(4096) + "&encoded=%5B1-9%5D#片段"
        val parsed = parseManualEntries("https://例子.test/深层/下载😀%20%20[01-02].zip$suffix")
        assertEquals("https://例子.test/深层/下载😀%20%2001.zip$suffix", parsed.entries.first().url)
        assertEquals("https://例子.test/深层/下载😀%20%2002.zip$suffix", parsed.entries.last().url)
    }

    @Test fun enforcesCumulativeGeneratedLimitBeforeDeduplicationAndRejectsWholeForm() {
        val accepted = parseManualEntries("https://example.com/a[1-500].zip\nhttps://example.com/b[1-500].zip\nhttps://example.com/plain.zip")
        assertEquals(1000, accepted.generatedCount)
        assertEquals(1001, accepted.entries.size)
        val rejected = parseManualEntries("https://example.com/plain.zip\nhttps://example.com/[1-600].zip\nhttps://example.com/[1-600].zip")
        assertTrue(rejected.rangeLimitExceeded)
        assertTrue(rejected.entries.isEmpty())
        assertTrue(rejected.rangePreview.isEmpty())
        assertTrue(parseManualEntries("https://example.com/[0-${"9".repeat(80)}].zip").rangeLimitExceeded)
    }

    @Test fun deduplicatesExpandedAndLiteralUrlsInInputOrder() {
        val parsed = parseManualEntries("https://example.com/02.zip\nhttps://example.com/[01-03].zip\nhttps://example.com/03.zip")
        assertEquals(3, parsed.generatedCount)
        assertEquals(listOf("02", "01", "03"), parsed.entries.map { it.url.substringAfterLast('/').substringBefore('.') })
        assertEquals(listOf("https://example.com/01.zip", "https://example.com/03.zip"), parsed.rangePreview.map { it.url })
    }

    @Test fun previewSkipsOrdinaryLinksAndIncludesEndpointsAcrossTemplates() {
        val parsed = parseManualEntries(
            "https://example.com/before.zip\nhttps://example.com/a[01-02].zip\nhttps://example.com/between.zip\n" +
                "https://example.com/b[03-04].zip\nhttps://example.com/after.zip",
        )
        assertEquals(4, parsed.generatedCount)
        assertEquals(7, parsed.entries.size)
        assertEquals(listOf("https://example.com/a01.zip", "https://example.com/b04.zip"), parsed.rangePreview.map { it.url })
    }

    @Test fun doesNotCloneLiteralFilenameOrChecksumAcrossGeneratedBatch() {
        val parsed = parseManualEntries("https://example.com/[01-02].zip\n  out=fixed.zip\n  checksum=sha-256:abcd")
        assertTrue(parsed.entries.all { it.fileName.isEmpty() && it.checksum.isEmpty() })
        val singleton = parseManualEntries("https://example.com/[01-01].zip\n  out=fixed.zip\n  checksum=sha-256:abcd")
        assertEquals(UrlEntry("https://example.com/01.zip", "fixed.zip", "sha-256:abcd"), singleton.entries.single())
        assertEquals(singleton.entries, singleton.rangePreview)
    }

    @Test fun literalParserAndExcludedSourcesDoNotExpandRangesOrHitManualLimit() {
        val url = "https://example.com/[0001-9999].zip"
        assertEquals(listOf(UrlEntry(url)), parseEntries(url))
        assertEquals(listOf(UrlEntry(url)), parseEntries("download: $url", loose = true))
        val excluded = parseManualEntries(url, setOf(url))
        assertEquals(listOf(UrlEntry(url)), excluded.entries)
        assertEquals(0, excluded.generatedCount)
        assertFalse(excluded.rangeLimitExceeded)
    }

    @Test fun txtExtractionPreservesBalancedTerminalBracketsAndEntryMetadata() {
        val url = "https://example.com/[001-215]"
        assertEquals(
            listOf(UrlEntry(url, "fixed.zip", "sha-256:abcd")),
            parseEntries("download: ($url).\n  out=fixed.zip\n  checksum=sha-256:abcd", loose = true),
        )
    }

    @Test fun aria2CommandsAndTheirContinuationsAreNotReinterpretedAsManualTemplates() {
        assertTrue(parseManualEntries("aria2c 'https://example.com/[1-3].zip'").entries.isEmpty())
        val parsed = parseManualEntries("aria2c \\\nhttps://example.com/[1-3000].zip\nhttps://example.com/manual[1-2].zip")
        assertFalse(parsed.rangeLimitExceeded)
        assertEquals(2, parsed.generatedCount)
        assertEquals("https://example.com/[1-3000].zip", parsed.entries.first().url)
        assertEquals(3, parsed.entries.size)
        val same = "https://example.com/[01-02].zip"
        assertEquals(
            listOf(same, "https://example.com/01.zip", "https://example.com/02.zip"),
            parseManualEntries("aria2c \\\n$same\n$same").entries.map { it.url },
        )
    }

    @Test fun preservesWindowsCommandContinuationButNotAnEscapedMarkerOrUrlPath() {
        val template = "https://example.com/[01-02].zip"
        val windows = parseManualEntries("\"C:\\Program Files\\aria2\\aria2c.exe\" ^\n$template")
        assertEquals(listOf(UrlEntry(template)), windows.entries)
        assertEquals(2, parseManualEntries("aria2c ^^\n$template").generatedCount)
        assertEquals(2, parseManualEntries("https://example.com/aria2c \\\n$template").generatedCount)
    }
}
