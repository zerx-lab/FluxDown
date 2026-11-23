package com.fluxdown.app.feature.downloads

import com.fluxdown.app.data.ViewPrefs
import com.fluxdown.app.feature.task.CategoryIndexCache
import com.fluxdown.app.feature.task.buildDetailModel
import com.fluxdown.core.model.HlsFragmentProgress
import com.fluxdown.core.model.Segment
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskRuntime
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.LiveSpeed
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class HlsProgressTest {
    private val task = Task(
        taskId = "vod", url = "https://example.com/vod.m3u8", originUrl = "",
        fileName = "vod.mp4", saveDir = "/dl", status = TaskStatus.Downloading,
        downloadedBytes = 8, totalBytes = 80,
    )
    private val state = HostState(tasks = listOf(task), speeds = mapOf(task.taskId to LiveSpeed(down = 8, up = 0)))
    private val runtime = TaskRuntime(
        taskId = task.taskId, sampleSequence = 1, activeTransfers = null, connectedPeers = null,
        totalBytes = 80, segments = listOf(Segment(0, 0, 79, 8, true)),
        hlsFragments = HlsFragmentProgress(50, 100),
    )
    private val cache = DeriveCache()

    private fun row(state: HostState): TaskItem {
        val input = DeriveInput(state, ViewPrefs(), DownloadsFilter(), emptySet())
        return derive(input, cache, nowMs = 0, interactionMs = 0).list.entries.filterIsInstance<RowEntry>().single().item
    }

    @Test fun fragmentCountTakesPriorityOverAvailableByteRanges() {
        val vod = state.copy(runtime = mapOf(task.taskId to runtime))
        val item = row(vod)
        assertEquals(0.5f, item.progress!!, 1e-6f)
        assertEquals(0.5f, item.flow!!.single().filled, 1e-6f)
        assertEquals(HlsFragmentProgress(50, 100), item.hlsFragments)
        assertNull(item.eta)

        val detail = buildDetailModel(vod, task.taskId, CategoryIndexCache())!!
        assertEquals(0.5f, detail.progress!!, 1e-6f)
        assertEquals(0.5f, detail.flowSegments.single().filled, 1e-6f)
    }

    @Test fun missingFragmentCountRestoresByteProgressAndEta() {
        row(state.copy(runtime = mapOf(task.taskId to runtime)))
        val legacy = state.copy(runtime = mapOf(task.taskId to runtime.copy(segments = emptyList(), hlsFragments = null)))
        val item = row(legacy)
        assertEquals(0.1f, item.progress!!, 1e-6f)
        assertEquals(0.1f, item.flow!!.single().filled, 1e-6f)
        assertNull(item.hlsFragments)
        assertEquals(9L, item.eta)

        val detail = buildDetailModel(legacy, task.taskId, CategoryIndexCache())!!
        assertEquals(0.1f, detail.progress!!, 1e-6f)
        assertEquals(0.1f, detail.flowSegments.single().filled, 1e-6f)
    }
}
