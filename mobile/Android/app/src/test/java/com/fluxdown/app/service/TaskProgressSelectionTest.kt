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
        selection.select(transfers)
        selection.dismiss("task-2")

        val updated = transfers.map { it.copy(downloadedBytes = it.downloadedBytes + 1) }
        assertEquals(listOf("task-1", "task-3", "task-4", "task-5"), ids(selection, updated))
    }

    @Test fun taskCanNotifyAgainAfterLeavingDownloading() {
        val selection = TaskProgressSelection()
        val task = transfer("a")
        val other = transfer("b")
        selection.select(listOf(task, other))
        selection.dismiss(task.taskId)
        assertEquals(listOf("b"), ids(selection, listOf(task.copy(status = TaskStatus.Preparing), other)))
        assertEquals(listOf("a", "b"), ids(selection, listOf(task, other)))

        selection.dismiss(task.taskId)
        assertEquals(listOf("b"), ids(selection, listOf(other)))
        assertEquals(listOf("a"), ids(selection, listOf(task)))
    }

    private fun ids(selection: TaskProgressSelection, transfers: List<Transfer>) = selection.select(transfers).map { it.taskId }

    private fun transfer(id: String, status: TaskStatus = TaskStatus.Downloading) =
        Transfer(id, "$id.bin", 10, 100, 10, status)
}
