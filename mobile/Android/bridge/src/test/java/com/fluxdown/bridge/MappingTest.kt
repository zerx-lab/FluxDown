package com.fluxdown.bridge

import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostEvent
import com.fluxdown.core.host.HostSignal
import com.fluxdown.core.host.ImportBatch
import com.fluxdown.core.host.ImportItem
import com.fluxdown.core.host.ImportProgress
import com.fluxdown.core.host.ImportRetry
import com.fluxdown.core.host.ImportRules
import com.fluxdown.core.host.ImportUpdate
import com.fluxdown.core.model.BtFile
import com.fluxdown.core.model.FileExistsAction
import com.fluxdown.core.model.QueueOrderEntry
import com.fluxdown.core.model.PluginCandidate
import com.fluxdown.core.model.SeedingStatus
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.update.AppUpdateSignal
import com.fluxdown.core.update.UpdateFailure
import com.fluxdown.core.update.UpdateManualReason
import com.fluxdown.core.update.UpdatePhase
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class MappingTest {
    @Test
    fun everyErrorCodeMapsToItsNamesake() {
        for (dto in ErrorCodeDto.entries) {
            val core = dto.toCore()
            assertEquals(dto.name.replace("_", "").lowercase(), core.name.lowercase())
        }
    }

    @Test
    fun rpcExceptionKeepsReasonAndRetryable() {
        val host = FluxException.Rpc(ErrorCodeDto.CONFLICT, "revisionConflict", true, "stale revision").toHost()
        assertEquals(HostErrorCode.Conflict, host.code)
        assertEquals("revisionConflict", host.reason)
        assertTrue(host.retryable)
        assertEquals("stale revision", host.message)
    }

    @Test
    fun transportIsRetryableUnavailable() {
        val host = FluxException.Transport("connection refused").toHost()
        assertEquals(HostErrorCode.Unavailable, host.code)
        assertTrue(host.retryable)
        assertNull(host.reason)
        assertEquals("connection refused", host.message)
    }

    @Test
    fun fatalSignalCarriesTheError() {
        val signal = HostSignalDto.Fatal(HostErrorDto(ErrorCodeDto.UNAUTHORIZED, null, false, "bad key")).toCore()
        val error = (signal as HostSignal.Fatal).error
        assertEquals(HostErrorCode.Unauthorized, error.code)
        assertEquals("bad key", error.message)
        assertSame(HostSignal.Stale, HostSignalDto.Stale.toCore())
    }

    @Test
    fun taskDtoMapsStatusSourceBytesAndSeeding() {
        val task = task(status = 5, seeding = 1).toCore()
        assertEquals(TaskStatus.Preparing, task.status)
        assertEquals(SeedingStatus.Seeding, task.seedingStatus)
        assertEquals(6L, task.sourceBytes.total)
        assertEquals(1_700_000_000L, task.createdAt)
        assertEquals(TaskStatus.Unknown, task(status = 99, seeding = 99).toCore().status)
    }

    @Test
    fun unsignedFieldsConvertToLong() {
        val runtime = TaskRuntimeDto("t", ULong.MAX_VALUE.shr(1), 3u, null, 10, emptyList()).toCore()
        assertEquals(Long.MAX_VALUE, runtime.sampleSequence)
        assertEquals(3, runtime.activeTransfers)
        assertNull(runtime.connectedPeers)

        val config = HostEventDto.ConfigChanged(mapOf("k" to "v"), 42uL).toCore() as HostEvent.ConfigChanged
        assertEquals(42L, config.revision)
        assertEquals(mapOf("k" to "v"), config.values)
    }

    @Test
    fun progressEventIsPassedThroughUnchanged() {
        val event = HostEventDto.TaskProgress("t", 4, 1, 2, 3, 4, "f", "deleted", 5, 0).toCore()
        assertEquals(HostEvent.TaskProgress("t", 4, 1, 2, 3, 4, "f", "deleted", 5, 0), event)
    }

    @Test
    fun queueOrderEventsKeepKeysAndRemovals() {
        assertEquals(
            HostEvent.QueuePositionsChanged(listOf(QueueOrderEntry("a", 10), QueueOrderEntry("b", 20))),
            HostEventDto.QueuePositionsChanged(listOf(QueueOrderDto("a", 10), QueueOrderDto("b", 20))).toCore(),
        )
        assertEquals(
            HostEvent.QueueOrderChanged(listOf("a"), listOf(QueueOrderEntry("c", 30))),
            HostEventDto.QueueOrderChanged(listOf("a"), listOf(QueueOrderDto("c", 30))).toCore(),
        )
    }

    @Test
    fun sectionAndNoticeEventsKeepNameAndJson() {
        assertEquals(
            HostEvent.SectionChanged("agent.gateway", """{"takeoverEnabled":true}"""),
            HostEventDto.SectionChanged("agent.gateway", """{"takeoverEnabled":true}""").toCore(),
        )
        assertEquals(
            HostEvent.Notice("duplicateTorrent", """{"type":"duplicateTorrent"}"""),
            HostEventDto.Notice("duplicateTorrent", """{"type":"duplicateTorrent"}""").toCore(),
        )
    }

    @Test
    fun selectionOutcomesRoundTrip() {
        val outcomes = listOf(
            SelectionOutcome.Hls(2),
            SelectionOutcome.Bt(listOf(0, 3), saveDir = "/new"),
            SelectionOutcome.Bt(listOf(1), saveDir = null),
            SelectionOutcome.Variant(1),
            SelectionOutcome.Plugin("com.x.a", remember = true),
            SelectionOutcome.Plugin("com.x.b", remember = false),
            SelectionOutcome.FileExists(FileExistsAction.Rename),
            SelectionOutcome.FileExists(FileExistsAction.Overwrite),
            SelectionOutcome.FileExists(FileExistsAction.Skip),
            SelectionOutcome.Cancelled,
        )
        for (outcome in outcomes) assertEquals(outcome, outcome.toDto().toCore())
    }

    @Test
    fun fileExistsKindKeepsEveryField() {
        val dto = SelectionKindDto.FileExists(
            fileName = "a.bin",
            saveDir = "/data/dl",
            existingSize = 1_048_576uL,
            existingModifiedUnixMs = 1_700_000_000_000L,
            incomingSize = null,
            renamePreview = "a (1).bin",
            actions = listOf(FileExistsActionDto.RENAME, FileExistsActionDto.OVERWRITE),
        )
        assertEquals(
            SelectionKind.FileExists(
                fileName = "a.bin",
                saveDir = "/data/dl",
                existingSize = 1_048_576L,
                existingModifiedUnixMs = 1_700_000_000_000L,
                incomingSize = null,
                renamePreview = "a (1).bin",
                actions = listOf(FileExistsAction.Rename, FileExistsAction.Overwrite),
            ),
            dto.toCore(),
        )
    }

    @Test
    fun pluginKindKeepsCandidates() {
        val dto = SelectionKindDto.Plugin(
            site = "bilibili.com",
            url = "https://www.bilibili.com/video/BV1",
            candidates = listOf(PluginCandidateDto("com.x.a", "A", "1.0.0", "desc a"), PluginCandidateDto("com.x.b", "B", "2.0.0", "")),
        )
        assertEquals(
            SelectionKind.Plugin(
                site = "bilibili.com",
                url = "https://www.bilibili.com/video/BV1",
                candidates = listOf(PluginCandidate("com.x.a", "A", "1.0.0", "desc a"), PluginCandidate("com.x.b", "B", "2.0.0", "")),
            ),
            dto.toCore(),
        )
    }

    @Test
    fun btKindKeepsSaveDir() {
        val dto = SelectionKindDto.Bt(
            files = listOf(BtFileDto(0, "a.bin", 12L)),
            saveDir = "/data",
        )
        assertEquals(SelectionKind.Bt(listOf(BtFile(0, "a.bin", 12)), "/data"), dto.toCore())
    }

    @Test
    fun updateStatusMapsSizesAndOptionals() {
        val dto = AppUpdateStatusDto(
            phase = UpdatePhaseDto.UP_TO_DATE, currentVersion = "1.0.0", channel = "stable", latestVersion = "1.0.0",
            hasUpdate = false, manualReason = null, assetName = "", assetSize = 5_000_000_000uL, downloadedBytes = 7uL,
            installPending = false, downloadUrl = "", releasePageUrl = "", notes = emptyList(), failure = null,
            errorDetail = "", checkedAtMs = 1_700_000_000_000uL,
        )
        val core = dto.toCore()
        assertEquals(UpdatePhase.UpToDate, core.phase)
        assertEquals(5_000_000_000L, core.assetSize)
        assertEquals(7L, core.downloadedBytes)
        assertEquals(1_700_000_000_000L, core.checkedAtMs)
        assertNull(core.manualReason)
        assertNull(core.failure)
    }

    @Test
    fun updateStatusMapsReasonFailureAndNotes() {
        val dto = AppUpdateStatusDto(
            phase = UpdatePhaseDto.FAILED, currentVersion = "1.0.0", channel = "frontier", latestVersion = "1.1.0",
            hasUpdate = true, manualReason = UpdateManualReasonDto.MANAGED_PACKAGE, assetName = "a.apk", assetSize = 10uL,
            downloadedBytes = 10uL, installPending = true, downloadUrl = "https://d", releasePageUrl = "https://r",
            notes = listOf(AppReleaseNoteDto(version = "1.1.0", publishedAt = "2026-01-01", body = "fix")),
            failure = UpdateFailureDto.ELEVATION_CANCELLED, errorDetail = "x", checkedAtMs = 1uL,
        )
        val core = dto.toCore()
        assertEquals(UpdateManualReason.ManagedPackage, core.manualReason)
        assertEquals(UpdateFailure.ElevationCancelled, core.failure)
        assertEquals(1, core.notes.size)
        assertEquals("fix", core.notes[0].body)
        assertTrue(core.installPending)
    }

    @Test
    fun updateSignalsAndEnumsRoundTrip() {
        val install = AppUpdateSignalDto.InstallRequested("/p/a.apk", "1.1.0").toCore()
        assertEquals(AppUpdateSignal.InstallRequested("/p/a.apk", "1.1.0"), install)
        for (failure in UpdateFailure.entries) assertEquals(failure, failure.toDto().toCore())
        for (reason in UpdateManualReason.entries) assertEquals(reason, reason.toDto().toCore())
        assertEquals(UpdatePhaseDto.entries.size, UpdatePhase.entries.size)
    }

    @Test
    fun importChunkSizeMatchesTheRustConstant() {
        assertEquals(importChunkSize().toInt(), ImportRules.CHUNK_SIZE)
    }

    @Test
    fun importBatchMapsCommonAndItemsToTheWire() {
        val common = CreateTaskRequest(url = "", saveDir = "/d", segments = 4, queueId = "q", startPaused = true, headers = mapOf("X" to "1"), offeredSaveDir = "/o", unattended = true)
        val dto = ImportBatch(common, listOf(ImportItem("https://a/1", "one.bin", "sha256=ab"), ImportItem("https://a/2"))).toDto()
        assertEquals("/d", dto.common.saveDir)
        assertEquals("/o", dto.common.offeredSaveDir)
        assertTrue(dto.common.unattended)
        assertEquals(4, dto.common.segments)
        assertEquals("q", dto.common.queueId)
        assertTrue(dto.common.startPaused)
        assertEquals(mapOf("X" to "1"), dto.common.headers)
        assertEquals(
            listOf(ImportItemDto("https://a/1", "one.bin", "sha256=ab"), ImportItemDto("https://a/2", "", "")),
            dto.items,
        )
    }

    @Test
    fun importProgressMapsCountsAndRetry() {
        val progress = ImportUpdateDto.Progress(ImportProgressDto(1_000u, 256u, 3u, ImportRetryDto(2u, 3u, 4u))).toCore(emptyList())
        assertEquals(
            ImportUpdate.Progress(ImportProgress(1_000, 256, 3, ImportRetry(attempt = 2, max = 3, delaySecs = 4))),
            progress,
        )
        val idle = (ImportUpdateDto.Progress(ImportProgressDto(10u, 0u, 0u, null)).toCore(emptyList()) as ImportUpdate.Progress).progress
        assertNull(idle.retry)
    }

    @Test
    fun importSummaryMapsFirstErrorWithoutEmptyMessage() {
        val rejected = HostErrorDto(ErrorCodeDto.INVALID_ARGUMENT, "invalidChecksum", false, "")
        val summary = (
            ImportUpdateDto.Finished(ImportSummaryDto(300u, 299u, 1u, 0u, false, rejected, listOf(7u))).toCore(emptyList())
                as ImportUpdate.Finished
            ).summary
        assertEquals(300, summary.total)
        assertEquals(299, summary.succeeded)
        assertEquals(1, summary.failed)
        assertEquals(false, summary.cancelled)
        val error = summary.firstError!!
        assertEquals(HostErrorCode.InvalidArgument, error.code)
        assertEquals("invalidChecksum", error.reason)
        // 单条拒绝没有文案：message 回落到 reason，而不是空串
        assertEquals("invalidChecksum", error.message)

        val clean = (
            ImportUpdateDto.Finished(ImportSummaryDto(5u, 0u, 0u, 5u, true, null, emptyList())).toCore(emptyList())
                as ImportUpdate.Finished
            ).summary
        assertTrue(clean.cancelled)
        assertEquals(5, clean.notSubmitted)
        assertNull(clean.firstError)
        assertTrue(clean.unfinished.isEmpty())
    }

    @Test
    fun importSummaryResolvesUnfinishedIndicesAgainstTheSubmittedItems() {
        val items = listOf(ImportItem("https://a/0"), ImportItem("https://a/1", "one.bin", "sha256=ab"), ImportItem("https://b/2"))
        val summary = (
            ImportUpdateDto.Finished(ImportSummaryDto(3u, 1u, 1u, 1u, true, null, listOf(1u, 2u, 99u))).toCore(items)
                as ImportUpdate.Finished
            ).summary
        // 按序号还原为条目（保留文件名 / 校验值）；越界序号被忽略而不是崩溃
        assertEquals(listOf(items[1], items[2]), summary.unfinished)
    }

    @Test
    fun loadingSnapshotKeepsTheLoadingFlagAndTotalInsteadOfLookingEmpty() {
        val loading = snapshot(tasksLoading = true, taskTotal = 230_000u, tasks = emptyList()).toCore()
        assertTrue(loading.tasksLoading)
        assertEquals(230_000L, loading.taskTotal)
        assertTrue(loading.tasks.isEmpty())

        val done = snapshot(tasksLoading = false, taskTotal = 1u, tasks = listOf(task(status = 1, seeding = 0))).toCore()
        assertEquals(false, done.tasksLoading)
        assertEquals(listOf("t1"), done.tasks.map { it.taskId })
    }

    @Test
    fun categoryMappingCarriesUrlPattern() {
        val dto = CategoryDto(
            id = "site", name = "Site", icon = "globe", extensions = emptyList(),
            regexPattern = "", urlPattern = "https://*.example.com/*", position = 2, visible = true, builtinType = null,
        )
        val core = dto.toCore()
        assertEquals("https://*.example.com/*", core.urlPattern)
        assertEquals("", core.regexPattern)
        assertEquals("site", core.id)
    }

    private fun snapshot(tasksLoading: Boolean, taskTotal: ULong, tasks: List<TaskDto>) = HostSnapshotDto(
        info = HostInfoDto("fluxdown-agent", "1.0.0", 17u, emptyList()),
        daemonConnected = true,
        tasksLoading = tasksLoading,
        taskTotal = taskTotal,
        tasks = tasks,
        runtime = emptyList(),
        queues = emptyList(),
        queueOrder = emptyList(),
        groups = emptyList(),
        stats = RuntimeStatsDto(0u, 0u, 0, 0, null, "", 0u),
        priorityTaskId = null,
        pendingSelections = emptyList(),
        config = emptyMap(),
        configRevision = 1u,
        rssSources = emptyList(),
        cloudDevices = emptyList(),
        linkDevices = emptyList(),
        categories = emptyList(),
        sections = emptyMap(),
    )

    private fun task(status: Int, seeding: Int) = TaskDto(
        taskId = "t1", url = "https://example.com/a.bin", originUrl = "", fileName = "a.bin", saveDir = "/data",
        status = status, downloadedBytes = 5, totalBytes = 10, errorMessage = "", createdAt = 1_700_000_000,
        completedAt = 0, queueId = "main", groupId = "", rssSourceId = "", fileMissing = false, autoRoute = "",
        sourceCdn = 1, sourceProxy = 2, sourceNic = 3, uploadedBytes = 0, seedingStatus = seeding,
        seedingMessage = "", seedingTimeSecs = 0,
    )
}
