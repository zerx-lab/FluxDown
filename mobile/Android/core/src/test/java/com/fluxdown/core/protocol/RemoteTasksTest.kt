package com.fluxdown.core.protocol

import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoteTasksTest {
    @Test
    fun parsesSectionLenientlyAndKeepsUnknownStatus() {
        val tasks = RemoteTaskDto.listFromJson(
            Json.parse(
                """[{"id":"a","toDevice":"mac","url":"https://x/a.zip","fileName":null,"status":"downloading",
                   "totalBytes":100,"downloadedBytes":40,"speed":7,"progress":0.4,"updatedAt":"2026-01-02"},
                   {"id":"b","status":"teleporting"},
                   {"toDevice":"no-id"}]""",
            ),
        )
        assertEquals(listOf("a", "b"), tasks.map { it.id })
        assertEquals("", tasks[0].fileName)
        assertEquals(RemoteTaskStatus.Downloading, tasks[0].status)
        assertEquals(100L, tasks[0].totalBytes)
        assertEquals(0.4, tasks[0].progress, 1e-9)
        assertEquals(RemoteTaskStatus.Unknown("teleporting"), tasks[1].status)
        assertNull(tasks[1].totalBytes)
        assertTrue(RemoteTaskDto.listFromJson(Json.parse("null")).isEmpty())
    }

    @Test
    fun commandMatrixMatchesGpui() {
        val all = RemoteCommandAction.entries
        fun allowed(s: RemoteTaskStatus) = all.filter { s.allows(it) }
        val running = listOf(RemoteCommandAction.Pause, RemoteCommandAction.Cancel, RemoteCommandAction.Delete)
        assertEquals(running, allowed(RemoteTaskStatus.Pending))
        assertEquals(running, allowed(RemoteTaskStatus.Accepted))
        assertEquals(running, allowed(RemoteTaskStatus.Downloading))
        assertEquals(listOf(RemoteCommandAction.Resume, RemoteCommandAction.Cancel, RemoteCommandAction.Delete), allowed(RemoteTaskStatus.Paused))
        for (s in listOf(RemoteTaskStatus.Completed, RemoteTaskStatus.Failed, RemoteTaskStatus.Canceled)) {
            assertEquals(listOf(RemoteCommandAction.Delete), allowed(s))
        }
        assertTrue(allowed(RemoteTaskStatus.Unknown("x")).isEmpty())
    }

    @Test
    fun pauseResumeNeedTargetOnlineButUnknownPresenceDoesNotBlock() {
        val running = RemoteTaskDto("t", status = RemoteTaskStatus.Downloading)
        assertFalse(RemoteTaskRules.canIssue(RemoteCommandAction.Pause, running, targetOnline = false))
        assertTrue(RemoteTaskRules.canIssue(RemoteCommandAction.Pause, running, targetOnline = null))
        assertTrue(RemoteTaskRules.canIssue(RemoteCommandAction.Cancel, running, targetOnline = false))
        assertEquals(RemoteCommandAction.Pause, RemoteTaskRules.primaryAction(running))
        assertEquals(RemoteCommandAction.Resume, RemoteTaskRules.primaryAction(running.copy(status = RemoteTaskStatus.Paused)))
        assertNull(RemoteTaskRules.primaryAction(running.copy(status = RemoteTaskStatus.Completed)))
    }

    @Test
    fun visibleDropsMirrorsOfCurrentDevice() {
        val tasks = listOf(
            RemoteTaskDto("done-new", toDevice = "mac", status = RemoteTaskStatus.Completed, updatedAt = "2026-03"),
            RemoteTaskDto("run-old", toDevice = "mac", status = RemoteTaskStatus.Downloading, updatedAt = "2026-01"),
            RemoteTaskDto("mine", toDevice = "me", status = RemoteTaskStatus.Downloading, updatedAt = "2026-04"),
            RemoteTaskDto("run-new", toDevice = "pc", status = RemoteTaskStatus.Paused, updatedAt = "2026-02"),
        )
        val visible = RemoteTaskRules.visible(tasks, currentDeviceId = "me")
        assertEquals(listOf("done-new", "run-old", "run-new"), visible.map { it.id })
        assertEquals(4, RemoteTaskRules.visible(tasks, currentDeviceId = null).size)
    }

    @Test
    fun createdAtParsesIsoWithOrWithoutFraction() {
        assertEquals(1_767_225_600L, RemoteTaskDto("a", createdAt = "2026-01-01T00:00:00Z").createdAtSeconds)
        assertEquals(1_767_225_600L, RemoteTaskDto("a", createdAt = "2026-01-01T08:00:00.123456+08:00").createdAtSeconds)
        assertEquals(0L, RemoteTaskDto("a", createdAt = "").createdAtSeconds)
        assertEquals(0L, RemoteTaskDto("a", createdAt = "yesterday").createdAtSeconds)
    }

    @Test
    fun commandParamsOmitMissingCommandId() {
        assertEquals(
            """{"taskId":"t","action":"delete","deleteFiles":true}""",
            RemoteCommandParams("t", RemoteCommandAction.Delete, deleteFiles = true).toJson().toJson(),
        )
    }

    @Test
    fun commandManyParamsJson() {
        assertEquals(
            """{"taskIds":["a","b"],"action":"delete","deleteFiles":true}""",
            RemoteCommandManyParams(listOf("a", "b"), RemoteCommandAction.Delete, deleteFiles = true).toJson().toJson(),
        )
        assertEquals(
            """{"taskIds":[],"action":"pause","deleteFiles":false}""",
            RemoteCommandManyParams(emptyList(), RemoteCommandAction.Pause).toJson().toJson(),
        )
        assertEquals("agent.remote.commandMany", HostMethod.agentRemoteCommandMany)
        assertEquals("agent.remoteBatch", HostCapability.agentRemoteBatch)
        assertEquals(1000, REMOTE_COMMAND_MANY_MAX)
    }

    @Test
    fun commandManyResultParsesLeniently() {
        val r = RemoteCommandManyResult.fromJson(
            Json.parse(
                """{"succeeded":["a"],"failed":[
                   {"taskId":"b","error":{"code":"notFound","retryable":false,"reason":"remoteTaskGone"}},
                   {"taskId":"c","error":{"code":"unavailable","retryable":true,"reason":"targetDeviceOffline"}},
                   {"taskId":"d","error":{"code":"future","retryable":false}},
                   {"error":{"code":"notFound"}}]}""",
            ),
        )
        assertEquals(listOf("a"), r.succeeded)
        assertEquals(listOf("b", "c", "d"), r.failed.map { it.taskId })
        assertEquals(HostErrorCode.NotFound, r.failed[0].code)
        assertEquals("remoteTaskGone", r.failed[0].reason)
        assertEquals(HostErrorCode.Unavailable, r.failed[1].code)
        assertTrue(r.failed[1].retryable)
        assertEquals("targetDeviceOffline", r.failed[1].toException().reason)
        assertEquals(HostErrorCode.Internal, r.failed[2].code)
        assertNull(r.failed[2].reason)
        assertEquals(RemoteCommandManyResult(), RemoteCommandManyResult.fromJson(Json.parse("{}")))
        assertEquals(RemoteCommandManyResult(), RemoteCommandManyResult.fromJson(null))
    }

    @Test
    fun chunksDedupeAndSplitAtMax() {
        val ids = (0 until 2500).map { "t$it" } + listOf("t0", "t1")
        val chunks = RemoteBatch.chunks(ids)
        assertEquals(listOf(1000, 1000, 500), chunks.map { it.size })
        assertEquals(2500, chunks.sumOf { it.size })
        assertEquals("t1000", chunks[1].first())
        assertTrue(RemoteBatch.chunks(emptyList()).isEmpty())
    }

    @Test
    fun eligibleFollowsMatrixAndTargetPresence() {
        val tasks = listOf(
            RemoteTaskDto("run-on", toDevice = "on", status = RemoteTaskStatus.Downloading),
            RemoteTaskDto("run-off", toDevice = "off", status = RemoteTaskStatus.Downloading),
            RemoteTaskDto("paused-on", toDevice = "on", status = RemoteTaskStatus.Paused),
            RemoteTaskDto("done", toDevice = "on", status = RemoteTaskStatus.Completed),
            RemoteTaskDto("odd", toDevice = "on", status = RemoteTaskStatus.Unknown("x")),
        )
        val online = { t: RemoteTaskDto -> t.toDevice == "on" }
        assertEquals(listOf("run-on"), RemoteBatch.eligible(tasks, RemoteCommandAction.Pause, online).map { it.id })
        assertEquals(listOf("paused-on"), RemoteBatch.eligible(tasks, RemoteCommandAction.Resume, online).map { it.id })
        // 取消 / 删除不依赖目标在线；未知状态不可控制。
        assertEquals(listOf("run-on", "run-off", "paused-on"), RemoteBatch.eligible(tasks, RemoteCommandAction.Cancel, online).map { it.id })
        assertEquals(listOf("run-on", "run-off", "paused-on", "done"), RemoteBatch.eligible(tasks, RemoteCommandAction.Delete, online).map { it.id })
    }

    @Test
    fun batchPathCallsManyPerChunkAndTurnsChunkErrorIntoFailures() = runBlocking {
        val ids = (0 until 1500).map { "t$it" }
        val seen = ArrayList<Int>()
        val outcome = RemoteBatch.run(
            ids, RemoteCommandAction.Cancel, deleteFiles = false, batchSupported = true,
            many = { p ->
                seen += p.taskIds.size
                if (seen.size == 1) {
                    RemoteCommandManyResult(
                        p.taskIds.drop(1),
                        listOf(RemoteCommandFailure(p.taskIds.first(), HostErrorCode.NotFound, "remoteTaskGone")),
                    )
                } else {
                    throw HostException(HostErrorCode.Unavailable, retryable = true)
                }
            },
            single = { error("single must not run") },
        )
        assertEquals(listOf(1000, 500), seen)
        assertEquals(999, outcome.succeeded.size)
        assertEquals(501, outcome.failed.size)
        assertEquals("remoteTaskGone", outcome.failed.first().reason)
        assertEquals(HostErrorCode.Unavailable, outcome.failed.last().code)
        assertFalse(outcome.legacyNotFound)
    }

    @Test
    fun fallbackRunsPerIdWithBoundedConcurrencyAndTreatsLegacyGoneAsDoneForDelete() = runBlocking {
        val ids = (0 until 20).map { "t$it" }
        var active = 0
        var peak = 0
        val calls = ArrayList<RemoteCommandParams>()
        val outcome = RemoteBatch.run(
            ids + "t0", RemoteCommandAction.Delete, deleteFiles = true, batchSupported = false,
            many = { error("many must not run") },
            single = { p ->
                active++
                peak = maxOf(peak, active)
                kotlinx.coroutines.yield()
                calls += p
                active--
                when (p.taskId) {
                    "t3" -> throw HostException(HostErrorCode.NotFound)
                    "t5" -> throw HostException(HostErrorCode.Unavailable, "targetDeviceOffline", retryable = true)
                    else -> Unit
                }
            },
        )
        assertTrue("peak=$peak", peak in 1..REMOTE_FALLBACK_CONCURRENCY)
        assertEquals(20, calls.size)
        assertTrue(calls.all { it.deleteFiles && it.action == RemoteCommandAction.Delete })
        assertEquals(19, outcome.succeeded.size)
        assertTrue("t3" in outcome.succeeded)
        assertEquals(listOf("t5"), outcome.failed.map { it.taskId })
        assertTrue(outcome.legacyNotFound)
    }

    @Test
    fun fallbackLegacyNotFoundStillFailsPauseResume() = runBlocking {
        val outcome = RemoteBatch.run(
            listOf("a"), RemoteCommandAction.Pause, deleteFiles = false, batchSupported = false,
            many = { error("many must not run") },
            single = { throw HostException(HostErrorCode.NotFound) },
        )
        assertTrue(outcome.succeeded.isEmpty())
        assertEquals(listOf("a"), outcome.failed.map { it.taskId })
        assertTrue(outcome.legacyNotFound)
    }

    @Test
    fun errorKeyTreatsLegacyNotFoundAsGone() {
        assertEquals("errReasonRemoteTaskGone", RemoteTaskRules.errorKey(HostException(HostErrorCode.NotFound)))
        assertEquals("errReasonRemoteTaskGone", RemoteTaskRules.errorKey(HostException(HostErrorCode.NotFound, "remoteTaskGone")))
        assertEquals("errReasonTargetDeviceOffline", RemoteTaskRules.errorKey(HostException(HostErrorCode.Unavailable, "targetDeviceOffline")))
        assertEquals("errReasonTaskStateConflict", RemoteTaskRules.errorKey(HostException(HostErrorCode.Conflict, "taskStateConflict")))
    }
}
