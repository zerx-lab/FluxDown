package com.fluxdown.bridge

import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostEvent
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSignal
import com.fluxdown.core.host.HostSnapshot
import com.fluxdown.core.host.ImportBatch
import com.fluxdown.core.host.ImportItem
import com.fluxdown.core.host.ImportProgress
import com.fluxdown.core.host.ImportRetry
import com.fluxdown.core.host.ImportSummary
import com.fluxdown.core.host.ImportUpdate
import com.fluxdown.core.model.BtFile
import com.fluxdown.core.model.Category
import com.fluxdown.core.model.CloudDevice
import com.fluxdown.core.model.FileExistsAction
import com.fluxdown.core.model.HlsOption
import com.fluxdown.core.model.PluginCandidate
import com.fluxdown.core.model.HostInfo
import com.fluxdown.core.model.LinkDevice
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.QueueOrderEntry
import com.fluxdown.core.model.RssSource
import com.fluxdown.core.model.RuntimeStats
import com.fluxdown.core.model.SeedingStatus
import com.fluxdown.core.model.Segment
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.model.SelectionRequest
import com.fluxdown.core.model.SourceBytes
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskGroup
import com.fluxdown.core.model.TaskRuntime
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.model.VariantOption
import com.fluxdown.core.update.AppUpdateConfig as CoreUpdateConfig
import com.fluxdown.core.update.AppUpdateSignal
import com.fluxdown.core.update.AppUpdateStatus
import com.fluxdown.core.update.ReleaseNote
import com.fluxdown.core.update.UpdateFailure
import com.fluxdown.core.update.UpdateManualReason
import com.fluxdown.core.update.UpdatePhase
import kotlinx.coroutines.CancellationException

/*
 * UniFFI 生成的 `*Dto` ↔ `:core` 模型的边界转换（u64→Long、u32→Int 一次性完成）。
 * 生成类型与 `:core` 端口同名者（`HostSession`）在本包内以生成版为准，端口一律经别名 / 全限定名引用。
 */

/** 调用 Rust：把绑定层异常归一成 [HostException]；原生库加载失败（LinkageError）同样转成可展示的失败。 */
internal suspend inline fun <T> guarded(block: () -> T): T = try {
    block()
} catch (e: FluxException) {
    throw e.toHost()
} catch (e: CancellationException) {
    throw e
} catch (e: LinkageError) {
    throw HostException(HostErrorCode.Internal, message = "native engine unavailable: ${e.message}", cause = e)
}

internal fun FluxException.toHost(): HostException = when (this) {
    is FluxException.Rpc -> HostException(code.toCore(), reason, retryable, detail, this)
    is FluxException.Transport -> HostException(HostErrorCode.Unavailable, null, true, detail, this)
    is FluxException.Closed -> HostException(HostErrorCode.Cancelled, null, false, "session closed", this)
}

internal fun HostErrorDto.toHost(): HostException = HostException(code.toCore(), reason, retryable, message)

internal fun ErrorCodeDto.toCore(): HostErrorCode = when (this) {
    ErrorCodeDto.PROTOCOL_INCOMPATIBLE -> HostErrorCode.ProtocolIncompatible
    ErrorCodeDto.UNAUTHORIZED -> HostErrorCode.Unauthorized
    ErrorCodeDto.INVALID_ARGUMENT -> HostErrorCode.InvalidArgument
    ErrorCodeDto.NOT_FOUND -> HostErrorCode.NotFound
    ErrorCodeDto.CONFLICT -> HostErrorCode.Conflict
    ErrorCodeDto.UNAVAILABLE -> HostErrorCode.Unavailable
    ErrorCodeDto.TIMEOUT -> HostErrorCode.Timeout
    ErrorCodeDto.CANCELLED -> HostErrorCode.Cancelled
    ErrorCodeDto.UNSUPPORTED -> HostErrorCode.Unsupported
    ErrorCodeDto.INTERNAL -> HostErrorCode.Internal
}

internal fun HostSignalDto.toCore(): HostSignal = when (this) {
    is HostSignalDto.Snapshot -> HostSignal.Snapshot(snapshot.toCore())
    is HostSignalDto.Event -> HostSignal.Event(event.toCore())
    is HostSignalDto.Stale -> HostSignal.Stale
    is HostSignalDto.Fatal -> HostSignal.Fatal(error.toHost())
}

internal fun HostSnapshotDto.toCore() = HostSnapshot(
    info = info.toCore(),
    daemonConnected = daemonConnected,
    tasks = tasks.map { it.toCore() },
    runtime = runtime.associate { it.taskId to it.toCore() },
    queues = queues.map { it.toCore() },
    queueOrder = queueOrder.map { it.toCore() },
    groups = groups.map { it.toCore() },
    stats = stats.toCore(),
    priorityTaskId = priorityTaskId,
    pendingSelections = pendingSelections.map { it.toCore() },
    config = config,
    configRevision = configRevision.toLong(),
    rssSources = rssSources.map { it.toCore() },
    cloudDevices = cloudDevices.map { it.toCore() },
    linkDevices = linkDevices.map { it.toCore() },
    categories = categories.map { it.toCore() },
    sections = sections,
    tasksLoading = tasksLoading,
    taskTotal = taskTotal.toLong(),
)

internal fun HostEventDto.toCore(): HostEvent = when (this) {
    is HostEventDto.TaskChanged -> HostEvent.TaskChanged(task.toCore())
    is HostEventDto.TaskDeleted -> HostEvent.TaskDeleted(taskId)
    is HostEventDto.TaskProgress -> HostEvent.TaskProgress(
        taskId = taskId,
        status = status,
        downloadedBytes = downloadedBytes,
        totalBytes = totalBytes,
        speed = speed,
        uploadSpeed = uploadSpeed,
        fileName = fileName,
        errorMessage = errorMessage,
        uploadedBytes = uploadedBytes,
        seedingStatus = seedingStatus,
    )
    is HostEventDto.TaskRuntimeChanged -> HostEvent.TaskRuntimeChanged(runtime.toCore())
    is HostEventDto.QueuesChanged -> HostEvent.QueuesChanged(queues.map { it.toCore() })
    is HostEventDto.QueuePositionsChanged -> HostEvent.QueuePositionsChanged(order.map { it.toCore() })
    is HostEventDto.QueueOrderChanged -> HostEvent.QueueOrderChanged(removed, added.map { it.toCore() })
    is HostEventDto.GroupsChanged -> HostEvent.GroupsChanged(groups.map { it.toCore() })
    is HostEventDto.FileMissingChanged -> HostEvent.FileMissingChanged(updates)
    is HostEventDto.PriorityTaskChanged -> HostEvent.PriorityTaskChanged(taskId)
    is HostEventDto.RuntimeStatsChanged -> HostEvent.RuntimeStatsChanged(stats.toCore())
    is HostEventDto.DaemonConnectionChanged -> HostEvent.DaemonConnectionChanged(connected)
    is HostEventDto.SelectionPending -> HostEvent.SelectionPending(request.toCore())
    is HostEventDto.SelectionResolved -> HostEvent.SelectionResolved(requestId)
    is HostEventDto.ConfigChanged -> HostEvent.ConfigChanged(values, revision.toLong())
    is HostEventDto.RssSourcesChanged -> HostEvent.RssSourcesChanged(sources.map { it.toCore() })
    is HostEventDto.CloudDevicesChanged -> HostEvent.CloudDevicesChanged(devices.map { it.toCore() })
    is HostEventDto.LinkedDevicesChanged -> HostEvent.LinkedDevicesChanged(devices.map { it.toCore() })
    is HostEventDto.CategoriesChanged -> HostEvent.CategoriesChanged(categories.map { it.toCore() })
    is HostEventDto.SectionChanged -> HostEvent.SectionChanged(name, json)
    is HostEventDto.Notice -> HostEvent.Notice(name, json)
}

internal fun HostInfoDto.toCore() = HostInfo(
    serviceName = serviceName,
    serviceVersion = serviceVersion,
    protocolVersion = protocolVersion.toInt(),
    capabilities = capabilities.toSet(),
)

internal fun TaskDto.toCore() = Task(
    taskId = taskId,
    url = url,
    originUrl = originUrl,
    fileName = fileName,
    saveDir = saveDir,
    status = TaskStatus.of(status),
    downloadedBytes = downloadedBytes,
    totalBytes = totalBytes,
    errorMessage = errorMessage,
    createdAt = createdAt,
    completedAt = completedAt,
    queueId = queueId,
    groupId = groupId,
    rssSourceId = rssSourceId,
    fileMissing = fileMissing,
    autoRoute = autoRoute,
    sourceBytes = SourceBytes(cdn = sourceCdn, proxy = sourceProxy, nic = sourceNic),
    uploadedBytes = uploadedBytes,
    seedingStatus = SeedingStatus.of(seedingStatus),
    seedingMessage = seedingMessage,
    seedingTimeSecs = seedingTimeSecs,
    btSequential = btSequential,
    btFirstLastPiecesFirst = btFirstLastPiecesFirst,
    btDownloadLimitBps = btDownloadLimitBps,
    seedUploadLimitBps = seedUploadLimitBps,
    btSuperSeeding = btSuperSeeding,
)

internal fun SegmentDto.toCore() = Segment(
    index = index,
    startByte = startByte,
    endByte = endByte,
    downloadedBytes = downloadedBytes,
    active = active,
)

internal fun TaskRuntimeDto.toCore() = TaskRuntime(
    taskId = taskId,
    sampleSequence = sampleSequence.toLong(),
    activeTransfers = activeTransfers?.toInt(),
    connectedPeers = connectedPeers?.toInt(),
    totalBytes = totalBytes,
    segments = segments.map { it.toCore() },
)

internal fun QueueDto.toCore() = Queue(
    queueId = queueId,
    name = name,
    speedLimitKbps = speedLimitKbps,
    uploadLimitKbps = uploadLimitKbps,
    maxConcurrent = maxConcurrent,
    defaultSaveDir = defaultSaveDir,
    position = position,
    isRunning = isRunning,
    scheduleEnabled = scheduleEnabled,
    scheduleStart = scheduleStart,
    scheduleStop = scheduleStop,
    scheduleDays = scheduleDays,
)

internal fun QueueOrderDto.toCore() = QueueOrderEntry(taskId = taskId, order = order)

internal fun GroupDto.toCore() = TaskGroup(
    groupId = groupId,
    name = name,
    sourceUrl = sourceUrl,
    saveDir = saveDir,
    createdAt = createdAt,
)

internal fun RuntimeStatsDto.toCore() = RuntimeStats(
    activeTasks = activeTasks.toInt(),
    pendingTasks = pendingTasks.toInt(),
    totalDownloadBps = totalDownloadBps,
    totalUploadBps = totalUploadBps,
    diskFreeBytes = diskFreeBytes?.toLong(),
    saveDir = saveDir,
    retryPendingTasks = retryPendingTasks.toInt(),
)

internal fun SelectionRequestDto.toCore() = SelectionRequest(
    requestId = requestId,
    taskId = taskId,
    kind = kind.toCore(),
    defaultChoice = defaultChoice.toCore(),
    deadlineUnixMs = deadlineUnixMs,
)

internal fun SelectionKindDto.toCore(): SelectionKind = when (this) {
    is SelectionKindDto.Hls -> SelectionKind.Hls(options.map { HlsOption(it.index, it.bandwidth, it.width, it.height) })
    is SelectionKindDto.Bt -> SelectionKind.Bt(files.map { BtFile(it.index, it.path, it.size) }, saveDir)
    is SelectionKindDto.Variant -> SelectionKind.Variant(
        options.map {
            VariantOption(it.index, it.label, it.container, it.bandwidth, it.width, it.height, it.totalBytes)
        },
    )
    is SelectionKindDto.FileExists -> SelectionKind.FileExists(
        fileName = fileName,
        saveDir = saveDir,
        existingSize = existingSize?.toLong(),
        existingModifiedUnixMs = existingModifiedUnixMs,
        incomingSize = incomingSize,
        renamePreview = renamePreview,
        actions = actions.map { it.toCore() },
    )
    is SelectionKindDto.Plugin -> SelectionKind.Plugin(
        site = site,
        url = url,
        candidates = candidates.map { PluginCandidate(it.identity, it.name, it.version, it.description) },
    )
}

internal fun FileExistsActionDto.toCore(): FileExistsAction = when (this) {
    FileExistsActionDto.RENAME -> FileExistsAction.Rename
    FileExistsActionDto.OVERWRITE -> FileExistsAction.Overwrite
    FileExistsActionDto.SKIP -> FileExistsAction.Skip
}

internal fun FileExistsAction.toDto(): FileExistsActionDto = when (this) {
    FileExistsAction.Rename -> FileExistsActionDto.RENAME
    FileExistsAction.Overwrite -> FileExistsActionDto.OVERWRITE
    FileExistsAction.Skip -> FileExistsActionDto.SKIP
}

internal fun SelectionOutcomeDto.toCore(): SelectionOutcome = when (this) {
    is SelectionOutcomeDto.Hls -> SelectionOutcome.Hls(index)
    is SelectionOutcomeDto.Bt -> SelectionOutcome.Bt(indices, saveDir)
    is SelectionOutcomeDto.Variant -> SelectionOutcome.Variant(index)
    is SelectionOutcomeDto.FileExists -> SelectionOutcome.FileExists(action.toCore())
    is SelectionOutcomeDto.Cancelled -> SelectionOutcome.Cancelled
    is SelectionOutcomeDto.Plugin -> SelectionOutcome.Plugin(identity, remember)
}

internal fun SelectionOutcome.toDto(): SelectionOutcomeDto = when (this) {
    is SelectionOutcome.Hls -> SelectionOutcomeDto.Hls(index)
    is SelectionOutcome.Bt -> SelectionOutcomeDto.Bt(indices, saveDir)
    is SelectionOutcome.Variant -> SelectionOutcomeDto.Variant(index)
    is SelectionOutcome.FileExists -> SelectionOutcomeDto.FileExists(action.toDto())
    is SelectionOutcome.Plugin -> SelectionOutcomeDto.Plugin(identity, remember)
    SelectionOutcome.Cancelled -> SelectionOutcomeDto.Cancelled
}

internal fun RssSourceDto.toCore() = RssSource(
    sourceId = sourceId,
    name = name,
    url = url,
    enabled = enabled,
    autoDownload = autoDownload,
    intervalMinutes = intervalMinutes,
    lastSuccessAt = lastSuccessAt,
    lastError = lastError,
    failCount = failCount,
    unreadCount = unreadCount,
)

internal fun CloudDeviceDto.toCore() = CloudDevice(
    deviceId = deviceId,
    name = name,
    platform = platform,
    isOnline = isOnline,
    isCurrent = isCurrent,
    appVersion = appVersion,
    defaultSaveDir = defaultSaveDir,
    pathStyle = pathStyle,
)

internal fun LinkDeviceDto.toCore() = LinkDevice(
    fingerprint = fingerprint,
    name = name,
    platform = platform,
    online = online,
    defaultSaveDir = defaultSaveDir,
    pathStyle = pathStyle,
    customName = customName,
    appVersion = appVersion,
    cloudDeviceId = cloudDeviceId,
    revoked = revoked,
    pairedAt = pairedAt,
    lastSeenAt = lastSeenAt,
    grantsControl = grantsControl,
    controllable = controllable,
)

internal fun CategoryDto.toCore() = Category(
    id = id,
    name = name,
    icon = icon,
    extensions = extensions,
    regexPattern = regexPattern,
    urlPattern = urlPattern,
    position = position,
    visible = visible,
    builtinType = builtinType,
)

// ───────────────────────────── 应用自更新 ─────────────────────────────

internal fun CoreUpdateConfig.toDto() = AppUpdateConfig(
    dataDir = dataDir,
    currentVersion = currentVersion,
    supportedAbis = supportedAbis,
    manualReason = manualReason?.toDto(),
)

internal fun UpdateManualReason.toDto(): UpdateManualReasonDto = when (this) {
    UpdateManualReason.ManagedPackage -> UpdateManualReasonDto.MANAGED_PACKAGE
    UpdateManualReason.NotWritable -> UpdateManualReasonDto.NOT_WRITABLE
    UpdateManualReason.NoAsset -> UpdateManualReasonDto.NO_ASSET
    UpdateManualReason.ElevationUnavailable -> UpdateManualReasonDto.ELEVATION_UNAVAILABLE
    UpdateManualReason.ReadOnlyLocation -> UpdateManualReasonDto.READ_ONLY_LOCATION
    UpdateManualReason.UnofficialBuild -> UpdateManualReasonDto.UNOFFICIAL_BUILD
    UpdateManualReason.Unsupported -> UpdateManualReasonDto.UNSUPPORTED
    UpdateManualReason.Unknown -> UpdateManualReasonDto.UNKNOWN
}

internal fun UpdateManualReasonDto.toCore(): UpdateManualReason = when (this) {
    UpdateManualReasonDto.MANAGED_PACKAGE -> UpdateManualReason.ManagedPackage
    UpdateManualReasonDto.NOT_WRITABLE -> UpdateManualReason.NotWritable
    UpdateManualReasonDto.NO_ASSET -> UpdateManualReason.NoAsset
    UpdateManualReasonDto.ELEVATION_UNAVAILABLE -> UpdateManualReason.ElevationUnavailable
    UpdateManualReasonDto.READ_ONLY_LOCATION -> UpdateManualReason.ReadOnlyLocation
    UpdateManualReasonDto.UNOFFICIAL_BUILD -> UpdateManualReason.UnofficialBuild
    UpdateManualReasonDto.UNSUPPORTED -> UpdateManualReason.Unsupported
    UpdateManualReasonDto.UNKNOWN -> UpdateManualReason.Unknown
}

internal fun UpdateFailure.toDto(): UpdateFailureDto = when (this) {
    UpdateFailure.Network -> UpdateFailureDto.NETWORK
    UpdateFailure.Verify -> UpdateFailureDto.VERIFY
    UpdateFailure.Storage -> UpdateFailureDto.STORAGE
    UpdateFailure.Install -> UpdateFailureDto.INSTALL
    UpdateFailure.ElevationCancelled -> UpdateFailureDto.ELEVATION_CANCELLED
    UpdateFailure.InstallIncomplete -> UpdateFailureDto.INSTALL_INCOMPLETE
    UpdateFailure.Unknown -> UpdateFailureDto.UNKNOWN
}

internal fun UpdateFailureDto.toCore(): UpdateFailure = when (this) {
    UpdateFailureDto.NETWORK -> UpdateFailure.Network
    UpdateFailureDto.VERIFY -> UpdateFailure.Verify
    UpdateFailureDto.STORAGE -> UpdateFailure.Storage
    UpdateFailureDto.INSTALL -> UpdateFailure.Install
    UpdateFailureDto.ELEVATION_CANCELLED -> UpdateFailure.ElevationCancelled
    UpdateFailureDto.INSTALL_INCOMPLETE -> UpdateFailure.InstallIncomplete
    UpdateFailureDto.UNKNOWN -> UpdateFailure.Unknown
}

internal fun UpdatePhaseDto.toCore(): UpdatePhase = when (this) {
    UpdatePhaseDto.IDLE -> UpdatePhase.Idle
    UpdatePhaseDto.CHECKING -> UpdatePhase.Checking
    UpdatePhaseDto.UP_TO_DATE -> UpdatePhase.UpToDate
    UpdatePhaseDto.AVAILABLE -> UpdatePhase.Available
    UpdatePhaseDto.DOWNLOADING -> UpdatePhase.Downloading
    UpdatePhaseDto.READY -> UpdatePhase.Ready
    UpdatePhaseDto.INSTALLING -> UpdatePhase.Installing
    UpdatePhaseDto.FAILED -> UpdatePhase.Failed
}

internal fun AppUpdateStatusDto.toCore() = AppUpdateStatus(
    phase = phase.toCore(),
    currentVersion = currentVersion,
    channel = channel,
    latestVersion = latestVersion,
    hasUpdate = hasUpdate,
    manualReason = manualReason?.toCore(),
    assetName = assetName,
    assetSize = assetSize.toLong(),
    downloadedBytes = downloadedBytes.toLong(),
    installPending = installPending,
    downloadUrl = downloadUrl,
    releasePageUrl = releasePageUrl,
    notes = notes.map { ReleaseNote(it.version, it.publishedAt, it.body) },
    failure = failure?.toCore(),
    errorDetail = errorDetail,
    checkedAtMs = checkedAtMs.toLong(),
)

internal fun AppUpdateSignalDto.toCore(): AppUpdateSignal = when (this) {
    is AppUpdateSignalDto.Status -> AppUpdateSignal.Status(status.toCore())
    is AppUpdateSignalDto.InstallRequested -> AppUpdateSignal.InstallRequested(packagePath, version)
}

// ── 批量导入 ──────────────────────────────────────────────────────────────

internal fun CreateTaskRequest.toDto() = CreateTaskRequestDto(
    url = url,
    fileName = fileName,
    saveDir = saveDir,
    segments = segments,
    queueId = queueId,
    startPaused = startPaused,
    cookies = cookies,
    referrer = referrer,
    userAgent = userAgent,
    proxyUrl = proxyUrl,
    checksum = checksum,
    ignoreTlsErrors = ignoreTlsErrors,
    headers = headers,
    httpUser = httpUser,
    httpPassword = httpPassword,
    saveSiteAuth = saveSiteAuth,
    offeredSaveDir = offeredSaveDir,
    unattended = unattended,
)

internal fun ImportBatch.toDto() = ImportBatchDto(
    common = common.toDto(),
    items = items.map { ImportItemDto(url = it.url, fileName = it.fileName, checksum = it.checksum) },
)

/** 单条拒绝没有文案（`message` 只带违规字段名）：空串按「无说明」处理，由 reason / code 兜底。 */
internal fun HostErrorDto.toImportError(): HostException =
    HostException(code.toCore(), reason, retryable, message.ifEmpty { null })

internal fun ImportProgressDto.toCore() = ImportProgress(
    total = total.toInt(),
    done = done.toInt(),
    failed = failed.toInt(),
    retry = retry?.let { ImportRetry(attempt = it.attempt.toInt(), max = it.max.toInt(), delaySecs = it.delaySecs.toInt()) },
)

/** [items] = 提交时各组条目按顺序拼接的列表：Rust 汇总里的 `unfinished` 是其中的序号。 */
internal fun ImportSummaryDto.toCore(items: List<ImportItem>) = ImportSummary(
    total = total.toInt(),
    succeeded = succeeded.toInt(),
    failed = failed.toInt(),
    notSubmitted = notSubmitted.toInt(),
    cancelled = cancelled,
    firstError = firstError?.toImportError(),
    unfinished = unfinished.mapNotNull { items.getOrNull(it.toInt()) },
)

internal fun ImportUpdateDto.toCore(items: List<ImportItem>): ImportUpdate = when (this) {
    is ImportUpdateDto.Progress -> ImportUpdate.Progress(progress.toCore())
    is ImportUpdateDto.Finished -> ImportUpdate.Finished(summary.toCore(items))
}
