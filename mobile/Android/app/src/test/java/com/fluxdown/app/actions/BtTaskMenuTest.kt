package com.fluxdown.app.actions

import com.fluxdown.app.R
import com.fluxdown.app.feature.task.btFilePriorityLabel
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.BtFilePriority
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BtTaskMenuTest {
    private fun task(
        url: String = "magnet:?xt=urn:btih:abc",
        status: TaskStatus = TaskStatus.Downloading,
        superSeeding: Boolean = false,
        id: String = "t",
    ) = Task(
        taskId = id, url = url, originUrl = "", fileName = "a", saveDir = "/d",
        status = status, downloadedBytes = 1, totalBytes = 10,
        btSuperSeeding = superSeeding,
    )

    @Test
    fun hiddenUnlessEverySelectedTaskIsBt() {
        assertNull(btTaskMenu(emptyList()))
        assertNull(btTaskMenu(listOf(task(url = "https://example.com/a.torrent"))))
        assertNull(btTaskMenu(listOf(task(), task(url = "ed2k://|file|a|1|h|/", id = "e"))))
        // 大小写不敏感 + torrent-file://
        assertEquals(true, btTaskMenu(listOf(task(url = "TORRENT-FILE:///x.torrent")))?.limits)
    }

    @Test
    fun recheckOnlyWhenNoneCompleted() {
        assertTrue(btTaskMenu(listOf(task(status = TaskStatus.Paused)))!!.recheck)
        assertTrue(btTaskMenu(listOf(task(status = TaskStatus.Failed)))!!.recheck)
        assertFalse(btTaskMenu(listOf(task(status = TaskStatus.Completed)))!!.recheck)
        assertFalse(btTaskMenu(listOf(task(), task(status = TaskStatus.Completed, id = "c")))!!.recheck)
    }

    @Test
    fun limitsOnlyForSingleTask() {
        assertTrue(btTaskMenu(listOf(task()))!!.limits)
        assertFalse(btTaskMenu(listOf(task(), task(id = "b")))!!.limits)
    }

    @Test
    fun superSeedingCheckedOnlyWhenAllOnAndTargetIsInverse() {
        val on = btTaskMenu(listOf(task(superSeeding = true), task(superSeeding = true, id = "b")))!!
        assertTrue(on.superSeedingOn)
        assertFalse(on.superSeedingTarget)
        val mixed = btTaskMenu(listOf(task(superSeeding = true), task(id = "b")))!!
        assertFalse(mixed.superSeedingOn)
        assertTrue(mixed.superSeedingTarget)
        // 完成（做种中）的任务也可以切超级做种
        assertTrue(btTaskMenu(listOf(task(status = TaskStatus.Completed)))!!.superSeedingTarget)
    }

    @Test
    fun superSeedingChangeSkipsTasksAlreadyAtTarget() {
        val a = task(superSeeding = true, id = "a")
        val b = task(id = "b")
        assertEquals(listOf(b), superSeedingChange(listOf(a, b), true))
        assertEquals(listOf(a), superSeedingChange(listOf(a, b), false))
        assertTrue(superSeedingChange(listOf(b), false).isEmpty())
    }

    @Test
    fun limitDigitsKeepsOnlyDigitsAndCapsLength() {
        assertEquals("1024", limitDigits(" 1,024 KiB"))
        assertEquals("", limitDigits("-"))
        assertEquals("1".repeat(15), limitDigits("1".repeat(40)))
        assertEquals("2", limitDigits("１2")) // 全角数字不是 ASCII 数字，丢弃
    }

    @Test
    fun filePriorityLabelsCoverFourLevels() {
        assertEquals(R.string.btFilePrioritySkip, btFilePriorityLabel(BtFilePriority.Skip))
        assertEquals(R.string.btFilePriorityNormal, btFilePriorityLabel(BtFilePriority.Normal))
        assertEquals(R.string.btFilePriorityHigh, btFilePriorityLabel(BtFilePriority.High))
        assertEquals(R.string.btFilePriorityMaximum, btFilePriorityLabel(BtFilePriority.Maximum))
    }
}
