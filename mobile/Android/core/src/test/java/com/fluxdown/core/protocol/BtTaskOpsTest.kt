package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** BT 文件表 / 任务限速纯逻辑（wire 形状同 `native/protocol/tests/protocol/bt_ops_wire.rs`）。 */
class BtTaskOpsTest {
    private fun file(index: Int, priority: BtFilePriority = BtFilePriority.Normal, size: Long = 10, downloaded: Long = 0) =
        BtTaskFile(index, "dir/f$index.bin", size, downloaded, priority)

    @Test
    fun parsesFilesDtoWithBigSizesAndDeepPaths() {
        val json = Json.parse(
            """{"editable":true,"files":[
              {"index":0,"path":"Show/Season 1/第一集 🎬.mkv","size":5368709120,"downloaded":2684354560,"priority":7},
              {"index":3,"path":"README","size":0,"priority":0}
            ]}""",
        )
        val files = BtFiles.fromJson(json)!!
        assertTrue(files.editable)
        assertEquals(2, files.files.size)
        val big = files.files[0]
        assertEquals(5_368_709_120L, big.size)
        assertEquals("第一集 🎬.mkv", big.name)
        assertEquals("Show/Season 1", big.directory)
        assertEquals(BtFilePriority.Maximum, big.priority)
        assertEquals(0.5f, big.progress, 0.0001f)
        val readme = files.files[1]
        assertEquals(3, readme.index)
        assertEquals("README", readme.name)
        assertEquals("", readme.directory)
        assertEquals(0L, readme.downloaded)
        assertEquals(BtFilePriority.Skip, readme.priority)
        assertEquals(1f, readme.progress, 0f)
    }

    @Test
    fun olderOrEmptyResultDefaultsSanely() {
        assertNull(BtFiles.fromJson(Json.parse("null")))
        val empty = BtFiles.fromJson(Json.parse("{}"))!!
        assertEquals(false, empty.editable)
        assertTrue(empty.files.isEmpty())
        // 缺 index 的行丢弃；缺 priority 按普通
        val partial = BtFiles.fromJson(Json.parse("""{"files":[{"path":"a"},{"index":1,"path":"b","size":4,"downloaded":9}]}"""))!!
        assertEquals(1, partial.files.size)
        assertEquals(BtFilePriority.Normal, partial.files[0].priority)
        assertEquals(1f, partial.files[0].progress, 0f)
    }

    @Test
    fun priorityWireMapping() {
        assertEquals(listOf(0, 1, 6, 7), BtFilePriority.entries.map { it.wire })
        assertEquals(BtFilePriority.Skip, BtFilePriority.of(0))
        assertEquals(BtFilePriority.Normal, BtFilePriority.of(4))
        assertEquals(BtFilePriority.High, BtFilePriority.of(6))
        assertEquals(BtFilePriority.Maximum, BtFilePriority.of(9))
        assertEquals(BtFilePriority.Normal, BtFilePriority.of(-1))
    }

    @Test
    fun priorityChangeSendsOnlyChangedFiles() {
        val files = listOf(file(0), file(1, BtFilePriority.High), file(2))
        val change = btFilePriorityChange("t", files, setOf(0, 1), BtFilePriority.High)
        assertEquals(
            """{"taskId":"t","priorities":[{"index":0,"priority":6}]}""",
            (change as BtFilePriorityChange.Send).params.toJson(),
        )
        assertEquals(BtFilePriorityChange.NoChange, btFilePriorityChange("t", files, setOf(1), BtFilePriority.High))
    }

    @Test
    fun skippingTheLastWantedFileIsRefused() {
        val files = listOf(file(0, BtFilePriority.Skip), file(1))
        assertEquals(BtFilePriorityChange.KeepOne, btFilePriorityChange("t", files, setOf(1), BtFilePriority.Skip))
        val three = listOf(file(0), file(1), file(2, BtFilePriority.Skip))
        assertTrue(btFilePriorityChange("t", three, setOf(0), BtFilePriority.Skip) is BtFilePriorityChange.Send)
        assertEquals(BtFilePriorityChange.KeepOne, btFilePriorityChange("t", three, setOf(0, 1), BtFilePriority.Skip))
    }

    @Test
    fun kibTextRoundsUpAndShowsZeroForUnlimited() {
        assertEquals("0", BtTaskLimits.kibText(0))
        assertEquals("0", BtTaskLimits.kibText(-5))
        assertEquals("1", BtTaskLimits.kibText(1))
        assertEquals("512", BtTaskLimits.kibText(524_288))
        assertEquals("2", BtTaskLimits.kibText(1500))
    }

    @Test
    fun parseKibRejectsGarbage() {
        assertEquals(0L, BtTaskLimits.parseKib("  "))
        assertEquals(300L, BtTaskLimits.parseKib(" 300 "))
        assertNull(BtTaskLimits.parseKib("-1"))
        assertNull(BtTaskLimits.parseKib("1.5"))
        assertNull(BtTaskLimits.parseKib("abc"))
        assertNull(BtTaskLimits.parseKib("99999999999999999999"))
        assertEquals(BtTaskLimits.MAX_KIB, BtTaskLimits.parseKib(BtTaskLimits.MAX_KIB.toString()))
    }

    @Test
    fun limitsSendOnlyChangedFields() {
        val onlyDown = BtTaskLimits.change("t", 1500, 0, "100", "0")
        assertEquals(
            """{"taskId":"t","downloadLimitBps":102400}""",
            (onlyDown as BtTaskLimits.Result.Send).params.toJson(),
        )
        // 未改动的 1500 B/s 显示为 2，不因取整被回写
        assertEquals(BtTaskLimits.Result.NoChange, BtTaskLimits.change("t", 1500, 2048, "2", "2"))
        val both = BtTaskLimits.change("t", 0, 4096, "", "0") as BtTaskLimits.Result.Send
        assertEquals("""{"taskId":"t","uploadLimitBps":0}""", both.params.toJson())
        assertEquals(BtTaskLimits.Result.Invalid, BtTaskLimits.change("t", 0, 0, "x", "0"))
    }
}
