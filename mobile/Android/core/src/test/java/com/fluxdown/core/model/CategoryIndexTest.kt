package com.fluxdown.core.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 任务列表分类：网址规则固定优先（匹配来源地址），其后扩展名 / 正则，最后 other。 */
class CategoryIndexTest {
    private fun custom(id: String, position: Int, extensions: List<String> = emptyList(), regex: String = "", url: String = "") =
        Category(id = id, name = id, icon = "file", extensions = extensions, regexPattern = regex, urlPattern = url, position = position)

    private val other = Category.BUILTIN.first { it.isOther }.copy(position = 100)
    private val all = Category.BUILTIN.first { it.isAll }

    @Test fun urlRuleBeatsExtensionRuleRegardlessOfListPosition() {
        val zip = custom("zip", 1, extensions = listOf("zip"))
        val site = custom("site", 2, url = "https://mirror.test/*")
        val index = CategoryIndex(listOf(all, zip, site, other))
        assertEquals("site", index.categoryOf("a.zip", "https://mirror.test/a.zip")?.id)
        assertEquals("zip", index.categoryOf("a.zip", "https://x.test/a.zip")?.id)
        assertEquals(other, index.categoryOf("a.exe", "https://x.test/a.exe"))
    }

    @Test fun urlRuleBeatsRegexRuleAndUrlRulesKeepListOrder() {
        val regex = custom("rx", 1, regex = "\\.iso$")
        val first = custom("first", 2, url = "https://mirror.test/*")
        val second = custom("second", 3, url = "https://mirror.test/iso/*")
        val index = CategoryIndex(listOf(regex, first, second, other))
        assertEquals("first", index.categoryOf("ubuntu.iso", "https://mirror.test/iso/ubuntu.iso")?.id)
        assertEquals("rx", index.categoryOf("ubuntu.iso", "https://other.test/ubuntu.iso")?.id)
    }

    @Test fun fileNameOnlyClassificationUsesRegexBeforeExtensionAndIgnoresUrlRules() {
        val episodes = custom("episodes", 1, regex = """^S\d+E\d+""")
        val video = custom("video", 2, extensions = listOf(".mkv"))
        val anyUrl = custom("site", 0, url = "*")
        val index = CategoryIndex(listOf(video, episodes, anyUrl, other))

        assertEquals(episodes, index.categoryOfFileName("s01e02.MKV"))
        assertEquals(video, index.categoryOfFileName("trailer.MKV"))
        assertEquals(other, index.categoryOfFileName("notes.txt"))
    }

    @Test fun wildcardIsAnchoredCaseInsensitiveWithQuestionMark() {
        val site = custom("site", 1, url = "HTTPS://Example.com/file?.zip")
        val index = CategoryIndex(listOf(site, other))
        assertEquals("site", index.categoryOf("x", "https://example.com/file1.zip")?.id)
        // 锚定完整匹配：多余前缀 / 后缀与多个字符都不命中。
        assertEquals(other, index.categoryOf("x", "https://example.com/file12.zip"))
        assertEquals(other, index.categoryOf("x", "https://example.com/file1.zip?x=1"))
        assertEquals(other, index.categoryOf("x", "see https://example.com/file1.zip"))
        // 通配符之外的正则元字符按字面处理。
        val dotted = CategoryIndex(listOf(custom("dot", 1, url = "https://a.test/v1.0/*"), other))
        assertEquals(other, dotted.categoryOf("x", "https://a.test/v1x0/f"))
        assertEquals("dot", dotted.categoryOf("x", "https://a.test/v1.0/f")?.id)
    }

    @Test fun unknownFileNameOnlyLetsUrlRulesParticipate() {
        val any = custom("rx", 1, regex = ".*")
        val site = custom("site", 2, url = "https://mirror.test/*")
        val index = CategoryIndex(listOf(any, site, other))
        assertEquals("site", index.categoryOf("", "https://mirror.test/download")?.id)
        assertEquals(other, index.categoryOf("", "https://x.test/download"))
    }

    @Test fun blankOrInvalidUrlPatternNeverMatches() {
        val blank = custom("blank", 1, url = "   ")
        val index = CategoryIndex(listOf(blank, other))
        assertEquals(other, index.categoryOf("a.zip", "https://x.test/a.zip"))
        assertNull(UrlWildcard.compile("   "))
    }

    @Test fun matchesHonorsUrlRulesForCategoryFilters() {
        val zip = custom("zip", 1, extensions = listOf("zip"))
        val site = custom("site", 2, url = "https://mirror.test/*")
        val index = CategoryIndex(listOf(all, zip, site, other))
        val url = "https://mirror.test/a.zip"
        assertTrue(index.matches(all, "a.zip", url))
        assertTrue(index.matches(site, "a.zip", url))
        assertTrue(index.matches(zip, "a.zip", url))
        assertFalse(index.matches(other, "a.zip", url))
        // 只命中网址规则的任务不属于 other，也不属于扩展名分类。
        assertTrue(index.matches(site, "blob", url))
        assertFalse(index.matches(other, "blob", url))
        assertFalse(index.matches(zip, "blob", url))
        assertTrue(index.matches(other, "blob", "https://x.test/blob"))
    }

    @Test fun shareUrlPrefersOriginUrlOverTaskUrl() {
        val site = custom("site", 1, url = "https://origin.test/*")
        val index = CategoryIndex(listOf(site, other))
        assertEquals("site", index.categoryOf("a.bin", shareUrlOf(originUrl = "https://origin.test/page", url = "https://cdn.test/a.bin"))?.id)
        assertEquals(other, index.categoryOf("a.bin", shareUrlOf(originUrl = "", url = "https://cdn.test/a.bin")))
        assertEquals("site", index.categoryOf("a.bin", shareUrlOf(originUrl = "", url = "https://origin.test/a.bin"))?.id)
    }

    /** 列表分类传的就是 `Task.shareUrl`：`originUrl` 非空取它，否则取 `url`。 */
    private fun shareUrlOf(originUrl: String, url: String): String = Task(
        taskId = "t", url = url, originUrl = originUrl, fileName = "a.bin", saveDir = "/d",
        status = TaskStatus.Completed, downloadedBytes = 1, totalBytes = 1,
    ).shareUrl
}
