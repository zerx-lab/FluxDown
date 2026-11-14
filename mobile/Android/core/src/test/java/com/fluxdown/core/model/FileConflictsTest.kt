package com.fluxdown.core.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class FileConflictsTest {
    private fun conflict(id: String, task: String = "t-$id", actions: List<FileExistsAction>, deadline: Long = 60_000) = SelectionRequest(
        requestId = id,
        taskId = task,
        kind = SelectionKind.FileExists("$id.bin", "/dl", 10L, null, null, "$id (1).bin", actions),
        defaultChoice = SelectionOutcome.FileExists(FileExistsAction.Rename),
        deadlineUnixMs = deadline,
    )

    private fun bt(id: String) = SelectionRequest(id, "t-$id", SelectionKind.Bt(emptyList()), SelectionOutcome.Cancelled, 0)

    private val all = listOf(FileExistsAction.Rename, FileExistsAction.Overwrite, FileExistsAction.Skip)
    private val noSkip = listOf(FileExistsAction.Rename, FileExistsAction.Overwrite)

    @Test
    fun pendingKeepsOnlyFileExistsInOrder() {
        val sel = listOf(conflict("a", actions = all), bt("x"), conflict("b", actions = all))
        assertEquals(listOf("a", "b"), FileConflicts.pending(sel).map { it.requestId })
        assertEquals(setOf("t-a", "t-b"), FileConflicts.taskIds(sel))
        assertTrue(FileConflicts.taskIds(listOf(bt("x"))).isEmpty())
    }

    @Test
    fun deferredBatchStaysHiddenUntilANewRequestArrives() {
        val a = conflict("a", actions = all)
        val b = conflict("b", actions = all)
        assertTrue(FileConflicts.shouldShow(listOf(a), emptySet()))
        assertFalse(FileConflicts.shouldShow(listOf(a), setOf("a")))
        // 新请求到来：整批重新弹出
        assertTrue(FileConflicts.shouldShow(listOf(a, b), setOf("a")))
        // 延后的请求已被答复 / 超时移除：没有待选就不弹
        assertFalse(FileConflicts.shouldShow(emptyList(), setOf("a")))
    }

    @Test
    fun bulkButtonRequiresEveryRequestToAllowTheAction() {
        val a = conflict("a", actions = all)
        val b = conflict("b", actions = noSkip)
        assertTrue(FileConflicts.canBulk(listOf(a), FileExistsAction.Skip))
        assertFalse(FileConflicts.canBulk(listOf(a, b), FileExistsAction.Skip))
        assertTrue(FileConflicts.canBulk(listOf(a, b), FileExistsAction.Rename))
        assertTrue(FileConflicts.canBulk(listOf(a, b), FileExistsAction.Overwrite))
        assertFalse(FileConflicts.canBulk(emptyList(), FileExistsAction.Rename))
    }

    @Test
    fun nearestDeadlineIsTheEarliest() {
        val sel = listOf(conflict("a", actions = all, deadline = 90), conflict("b", actions = all, deadline = 30))
        assertEquals(30L, FileConflicts.nearestDeadline(sel))
        assertNull(FileConflicts.nearestDeadline(emptyList()))
    }

    @Test
    fun btNameOutcomeOnlyWhenChanged() {
        assertNull(btNameOutcome("Movie", "Movie"))
        assertNull(btNameOutcome("Movie", "  Movie  "))
        assertNull(btNameOutcome(" Movie ", "Movie"))
        assertNull(btNameOutcome("Movie", "   "))
        assertNull(btNameOutcome("Movie", ""))
        assertEquals("New", btNameOutcome("Movie", "  New "))
    }

    @Test
    fun onlyBtCancelDeletesTask() {
        assertTrue(SelectionKind.Bt(emptyList()).cancelDeletesTask)
        assertFalse(SelectionKind.Hls(emptyList()).cancelDeletesTask)
        assertFalse(SelectionKind.Variant(emptyList()).cancelDeletesTask)
        assertFalse(SelectionKind.Plugin("s", "u", emptyList()).cancelDeletesTask)
        assertFalse(SelectionKind.FileExists("a", "/d", null, null, null, "a (1)", all).cancelDeletesTask)
    }
}
