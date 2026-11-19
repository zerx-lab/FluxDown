package com.fluxdown.core.store

import com.fluxdown.core.model.HlsFragmentProgress
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskRuntime
import com.fluxdown.core.model.TaskStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** 进度通知的聚合规则：未知大小不拖垮进度、剩余时间需全部已知、越界夹紧、只投影活跃任务。 */
class TransferSummaryTest {
    private fun transfer(id: String, down: Long, total: Long, speed: Long) = Transfer(id, "$id.bin", down, total, speed)

    @Test fun aggregatesKnownSizesAndSpeed() {
        val s = TransferSummary.of(listOf(transfer("a", 25, 100, 5), transfer("b", 75, 100, 5)))
        assertEquals(2, s.count)
        assertEquals(0.5f, s.fraction!!, 1e-6f)
        assertEquals(10L, s.speedBps)
        // 剩余 100 B / 10 B/s
        assertEquals(10L, s.etaSeconds)
    }

    @Test fun unknownSizeKeepsProgressOfKnownButDropsEta() {
        val s = TransferSummary.of(listOf(transfer("a", 50, 100, 10), transfer("hls", 4_000, 0, 30)))
        assertEquals(0.5f, s.fraction!!, 1e-6f)
        assertEquals(40L, s.speedBps)
        assertNull(s.etaSeconds)
    }

    @Test fun allUnknownIsIndeterminate() {
        val s = TransferSummary.of(listOf(transfer("a", 10, 0, 1), transfer("b", 20, 0, 1)))
        assertNull(s.fraction)
        assertNull(s.etaSeconds)
        assertNull(TransferSummary.Empty.fraction)
    }

    @Test fun overshootIsClamped() {
        // 收尾时已下载可能短暂超过总大小：单项与聚合都不得超过 100%
        val t = transfer("a", 130, 100, 0)
        assertEquals(1f, t.fraction!!, 0f)
        val s = TransferSummary.of(listOf(t, transfer("b", 0, 100, 0)))
        assertEquals(0.5f, s.fraction!!, 1e-6f)
    }

    @Test fun projectsFragmentProgressWithOrWithoutEstimatedSize() {
        val task = Task(
            taskId = "vod", url = "https://example.com/vod.m3u8", originUrl = "",
            fileName = "vod.mp4", saveDir = "/dl", status = TaskStatus.Downloading,
            downloadedBytes = 8, totalBytes = 80,
        )
        val runtime = TaskRuntime(
            taskId = task.taskId, sampleSequence = 1, activeTransfers = null, connectedPeers = null,
            totalBytes = 80, segments = emptyList(), hlsFragments = HlsFragmentProgress(50, 100),
        )
        val state = HostState(connection = Connection.Live, tasks = listOf(task), runtime = mapOf(task.taskId to runtime))
        assertEquals(0.5f, state.transfers().single().fraction!!, 1e-6f)
        assertEquals(0.5f, state.copy(tasks = listOf(task.copy(totalBytes = 0))).transfers().single().fraction!!, 1e-6f)
        assertEquals(0.1f, state.copy(runtime = emptyMap()).transfers().single().fraction!!, 1e-6f)
        assertNull(state.copy(tasks = listOf(task.copy(totalBytes = 0)), runtime = emptyMap()).transfers().single().fraction)
    }

    @Test fun aggregatesFragmentCountsButDoesNotMixThemWithBytes() {
        val a = transfer("a", 8, 80, 8).copy(hlsFragments = HlsFragmentProgress(50, 100))
        val b = transfer("b", 2, 40, 2).copy(hlsFragments = HlsFragmentProgress(10, 20))
        val c = transfer("c", 1, 0, 1).copy(hlsFragments = HlsFragmentProgress(0, 80))
        val fragmentSummary = TransferSummary.of(listOf(a, b, c))
        assertEquals(0.3f, fragmentSummary.fraction!!, 1e-6f)
        assertNull(fragmentSummary.etaSeconds)

        val mixed = TransferSummary.of(listOf(a, b.copy(hlsFragments = null)))
        assertNull(mixed.fraction)
        assertNull(mixed.etaSeconds)
        assertEquals(10L, mixed.speedBps)
    }

    @Test fun projectsOnlyActiveTasksWhileLive() {
        fun task(id: String, status: TaskStatus) = Task(
            taskId = id, url = "https://example.com/$id", originUrl = "", fileName = "$id.bin", saveDir = "/dl",
            status = status, downloadedBytes = 10, totalBytes = 100,
        )
        val state = HostState(
            connection = Connection.Live,
            tasks = listOf(task("a", TaskStatus.Downloading), task("b", TaskStatus.Paused), task("c", TaskStatus.Preparing)),
            speeds = mapOf("a" to LiveSpeed(down = 7, up = 0)),
        )
        assertEquals(
            listOf(Transfer("a", "a.bin", 10, 100, 7), Transfer("c", "c.bin", 10, 100, 0)),
            state.transfers(),
        )
        assertEquals(emptyList<Transfer>(), state.copy(connection = Connection.Stale).transfers())
    }
}
