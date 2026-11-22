package com.fluxdown.app.service

import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.Transfer

// Progress updates share Android's per-package rate and notification limits with completion and action alerts.
internal const val MAX_TASK_PROGRESS_NOTIFICATIONS = 4

internal class TaskProgressSelection {
    private val dismissedIds = mutableSetOf<String>()

    fun dismiss(taskId: String) {
        dismissedIds.add(taskId)
    }

    fun select(transfers: List<Transfer>, shownIds: Set<String>): List<Transfer> {
        if (transfers.isEmpty()) {
            dismissedIds.clear()
            return emptyList()
        }
        if (dismissedIds.isNotEmpty()) {
            dismissedIds.retainAll(transfers.mapNotNullTo(HashSet()) {
                if (it.status == TaskStatus.Downloading) it.taskId else null
            })
        }
        // Preparing contributes to the foreground summary, but has no individual download progress yet.
        return buildList(MAX_TASK_PROGRESS_NOTIFICATIONS) {
            // A new task or a queue reorder must not evict an already visible download.
            if (shownIds.isNotEmpty()) {
                for (transfer in transfers) {
                    if (transfer.status != TaskStatus.Downloading || transfer.taskId !in shownIds || transfer.taskId in dismissedIds) continue
                    add(transfer)
                    if (size == MAX_TASK_PROGRESS_NOTIFICATIONS) return@buildList
                }
            }
            for (transfer in transfers) {
                if (transfer.status != TaskStatus.Downloading || transfer.taskId in shownIds || transfer.taskId in dismissedIds) continue
                add(transfer)
                if (size == MAX_TASK_PROGRESS_NOTIFICATIONS) break
            }
        }
    }
}
