package com.fluxdown.app.service

import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.Transfer
import org.junit.Assert.assertEquals
import org.junit.Test

class TaskProgressSelectionTest {
    @Test fun limitsProgressNoticesWithoutCountingPreparingTasks() {
        val transfers = listOf(transfer("preparing", TaskStatus.Preparing)) + (1..8).map { transfer("task-$it") }
        assertEquals(listOf("task-1", "task-2", "task-3", "task-4"), ids(TaskProgressSelection(), transfers))
    }

    @Test fun dismissedTaskStaysHiddenAcrossUpdatesAndFreesSlot() {
        val selection = TaskProgressSelection()
        val transfers = (1..5).map { transfer("task-$it") }
        val shown = ids(selection, transfers).toSet()
        selection.dismiss("task-2")

        val updated = transfers.map { it.copy(downloadedBytes = it.downloadedBytes + 1) }
        assertEquals(listOf("task-1", "task-3", "task-4", "task-5"), ids(selection, updated, shown - "task-2"))
    }

    @Test fun reorderingAndNewDownloadsKeepAlreadyVisibleTasks() {
        val selection = TaskProgressSelection()
        val initial = (1..4).map { transfer("task-$it") }
        val shown = ids(selection, initial).toSet()
        val added = transfer("new")
        val reordered = listOf(added) + initial.reversed()
        assertEquals(shown, ids(selection, reordered, shown).toSet())

        val withVacancy = reordered.map { if (it.taskId == "task-2") it.copy(status = TaskStatus.Preparing) else it }
        assertEquals(setOf("task-1", "task-3", "task-4", "new"), ids(selection, withVacancy, shown).toSet())
    }

    @Test fun taskCanNotifyAgainAfterLeavingDownloading() {
        val selection = TaskProgressSelection()
        val task = transfer("a")
        val other = transfer("b")
        selection.dismiss(task.taskId)
        assertEquals(listOf("b"), ids(selection, listOf(task.copy(status = TaskStatus.Preparing), other)))
        assertEquals(listOf("a", "b"), ids(selection, listOf(task, other)))

        selection.dismiss(task.taskId)
        assertEquals(listOf("b"), ids(selection, listOf(other)))
        assertEquals(listOf("a"), ids(selection, listOf(task)))
    }

    private fun ids(selection: TaskProgressSelection, transfers: List<Transfer>, shown: Set<String> = emptySet()) =
        selection.select(transfers, shown).map { it.taskId }

    private fun transfer(id: String, status: TaskStatus = TaskStatus.Downloading) =
        Transfer(id, "$id.bin", 10, 100, 10, status)
}
