package com.fluxdown.core.protocol

import com.fluxdown.core.model.TaskStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class RssTest {
    @Test
    fun sizeLiteralParsesAndRoundTrips() {
        assertEquals(200L shl 20, RssSizeLiteral.parse("200M"))
        assertEquals((1.5 * (1L shl 30)).toLong(), RssSizeLiteral.parse(" 1.5 GB "))
        assertEquals(1024L, RssSizeLiteral.parse("1024"))
        assertNull(RssSizeLiteral.parse("1.5.2G"))
        assertNull(RssSizeLiteral.parse("-1M"))
        assertEquals(0L, RssSizeLiteral.field(" "))
        assertEquals("2G", RssSizeLiteral.format(2L shl 30))
        assertEquals("1500", RssSizeLiteral.format(1500))
        assertEquals("", RssSizeLiteral.format(0))
    }

    @Test
    fun maxPerFetchAcceptsOnlyOneToHundred() {
        assertEquals(1, parseRssMaxPerFetch("1"))
        assertEquals(100, parseRssMaxPerFetch(" 100 "))
        assertNull(parseRssMaxPerFetch("0"))
        assertNull(parseRssMaxPerFetch("101"))
        assertNull(parseRssMaxPerFetch("1.5"))
    }

    @Test
    fun detailKeepsUnknownAndRuntimeFieldsOnWriteBack() {
        val raw = Json.parse("""{"sourceId":"s1","url":"https://a/feed","providerId":"bili","providerConfig":"{}","failCount":3,"unreadCount":2,"maxPerFetch":5}""")
        val d = RssSourceDetail.fromJson(raw)!!
        val out = d.copy(name = "N").toJson()
        assertEquals("bili", out.str("providerId"))
        assertEquals(3, out.int("failCount"))
        assertEquals("N", out.str("name"))
        assertEquals(5, out.int("maxPerFetch"))
        assertNull(RssSourceDetail.fromJson(Json.parse("""{"sourceId":"x"}""")))
        // 新建：缺省内置 provider
        assertEquals("rss", RssSourceDetail(url = "u").toJson().str("providerId"))
    }

    // ── 条目（R2，同 iOS RssProtocolTests / RssItemsTests） ──

    private fun item(
        guid: String,
        title: String = "T",
        status: Int = 0,
        taskId: String = "",
        pubDate: Long = 0,
    ) = RssItemDto(sourceId = "s", guid = guid, title = title, pubDate = pubDate, status = status, taskId = taskId)

    @Test
    fun itemDecodesStatusReasonAndTaskLink() {
        val items = RssItemDto.listFromJson(
            Json.parse(
                """[{"sourceId":"s1","guid":"g1","title":"Show - 02 [1080p]","link":"https://x/1","enclosureUrl":"https://x/1.torrent",
                "enclosureLength":734003200,"pubDate":1700000000,"fetchedAt":1700000050,"status":1,"taskId":"t9","episodeKey":"show:2","reason":"","imageCount":2},
                {"sourceId":"s1","guid":"g2","title":"Other","link":"","enclosureUrl":"","enclosureLength":0,"pubDate":0,"fetchedAt":1,
                "status":3,"taskId":"","episodeKey":"","reason":"too_large"},
                {"sourceId":"s1","guid":"g3","title":"Future","link":"https://x/3","enclosureUrl":"","status":42,"reason":"brand_new_reason"},
                {"title":"no guid"}]""",
            ),
        )
        assertEquals(3, items.size)
        assertEquals(RssItemStatus.Downloaded, items[0].state)
        assertEquals("t9", items[0].taskId)
        assertEquals(734_003_200L, items[0].enclosureLength)
        assertEquals("https://x/1.torrent", items[0].effectiveLink)
        assertEquals(2, items[0].imageCount)
        assertEquals(RssItemStatus.Filtered, items[1].state)
        assertEquals(RssReason.TooLarge, items[1].reasonCode)
        assertEquals(0, items[1].imageCount)
        assertEquals(RssItemStatus.Unknown, items[2].state)
        assertEquals(RssReason.Unknown, items[2].reasonCode)
        assertEquals("https://x/3", items[2].effectiveLink)
    }

    @Test
    fun reasonCodesWithoutTextAreHidden() {
        for (code in listOf("not_included", "excluded", "too_small", "too_large", "dup_episode", "torrent_fetch_failed")) {
            assertEquals(code, true, RssReason.fromWire(code).hasText)
        }
        for (code in listOf("seed_skipped", "", "brand_new")) {
            assertEquals(code, false, RssReason.fromWire(code).hasText)
        }
    }

    @Test
    fun itemActionParamsOmitGuidForReadAll() {
        assertEquals(
            """{"sourceId":"s1","action":"readAll"}""",
            rssItemActionParams("s1", RssItemAction.ReadAll, guid = "ignored").toJson(),
        )
        assertEquals(
            """{"sourceId":"s1","guid":"g/1","action":"download"}""",
            rssItemActionParams("s1", RssItemAction.Download, guid = "g/1").toJson().replace("\\/", "/"),
        )
    }

    @Test
    fun itemsChangedNoticeCarriesSnapshotAndTitles() {
        val n = RssItemsChangedNotice.fromJson(
            Json.parse("""{"sourceId":"s1","notifyTitles":["A","B"],"items":[{"sourceId":"s1","guid":"g","title":"T"}]}"""),
        )!!
        assertEquals("s1", n.sourceId)
        assertEquals(listOf("A", "B"), n.notifyTitles)
        assertEquals(listOf("g"), n.items.map { it.guid })
        assertNull(RssItemsChangedNotice.fromJson(Json.parse("""{"items":[]}""")))
    }

    @Test
    fun nonDownloadedStatusesMapToTheirOwnChips() {
        assertEquals(RssChipKind.New, RssItems.chip(item("a", status = 0), null).kind)
        assertEquals(RssChipKind.Ignored, RssItems.chip(item("a", status = 2), null).kind)
        assertEquals(RssChipKind.Filtered, RssItems.chip(item("a", status = 3), null).kind)
        assertEquals(RssChipKind.Duplicate, RssItems.chip(item("a", status = 4), null).kind)
        assertEquals(RssChipKind.History, RssItems.chip(item("a", status = 5), null).kind)
        // 未知状态按「新」处理
        assertEquals(RssChipKind.New, RssItems.chip(item("a", status = 42), null).kind)
    }

    @Test
    fun downloadedItemFollowsLinkedTaskState() {
        val downloaded = item("a", status = 1, taskId = "t1")
        fun kind(task: RssLinkedTask?) = RssItems.chip(downloaded, task).kind
        assertEquals(RssChipKind.Pending, kind(RssLinkedTask(TaskStatus.Pending)))
        assertEquals(RssChipKind.Downloading, kind(RssLinkedTask(TaskStatus.Downloading, fraction = 0.4)))
        assertEquals(RssChipKind.Paused, kind(RssLinkedTask(TaskStatus.Paused)))
        assertEquals(RssChipKind.Completed, kind(RssLinkedTask(TaskStatus.Completed)))
        assertEquals(RssChipKind.Incomplete, kind(RssLinkedTask(TaskStatus.Completed, fileMissing = true)))
        assertEquals(RssChipKind.Error, kind(RssLinkedTask(TaskStatus.Failed)))
        assertEquals(RssChipKind.Preparing, kind(RssLinkedTask(TaskStatus.Preparing)))
        assertEquals(RssChipKind.TaskCreated, kind(RssLinkedTask(TaskStatus.Unknown)))
        assertEquals(0.4, RssItems.chip(downloaded, RssLinkedTask(TaskStatus.Downloading, fraction = 0.4)).progress)
        assertNull(RssItems.chip(downloaded, RssLinkedTask(TaskStatus.Paused)).progress)
    }

    @Test
    fun deletedOrUnlinkedTaskIsMarkedMissing() {
        assertEquals(RssChipKind.TaskMissing, RssItems.chip(item("a", status = 1, taskId = "gone"), null).kind)
        assertEquals(
            RssChipKind.TaskMissing,
            RssItems.chip(item("a", status = 1, taskId = ""), RssLinkedTask(TaskStatus.Completed)).kind,
        )
        assertEquals(RssChipTone.Failure, RssItems.chip(item("a", status = 1, taskId = "gone"), null).tone)
    }

    @Test
    fun visibleFiltersByTitleCaseInsensitively() {
        val items = listOf(item("1", title = "One Piece 1080p", pubDate = 10), item("2", title = "Bleach", pubDate = 20))
        assertEquals(listOf("1"), RssItems.visible(items, "  PIECE ", oldestFirst = false).map { it.guid })
        assertEquals(listOf("2", "1"), RssItems.visible(items, "", oldestFirst = false).map { it.guid })
    }

    @Test
    fun visibleSortsByDateWithUndatedLastAndStableTies() {
        val items = listOf(
            item("undated-a", pubDate = 0),
            item("old", pubDate = 100),
            item("tie-a", pubDate = 200),
            item("undated-b", pubDate = -1),
            item("new", pubDate = 300),
            item("tie-b", pubDate = 200),
        )
        assertEquals(
            listOf("new", "tie-a", "tie-b", "old", "undated-a", "undated-b"),
            RssItems.visible(items, "", oldestFirst = false).map { it.guid },
        )
        assertEquals(
            listOf("old", "tie-a", "tie-b", "new", "undated-a", "undated-b"),
            RssItems.visible(items, "", oldestFirst = true).map { it.guid },
        )
    }
}
