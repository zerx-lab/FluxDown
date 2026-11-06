import FluxDomain
import Foundation
internal import FluxRustBindings

// UniFFI DTO ↔ FluxDomain 模型的一次性转换（与 Android `:bridge` 的 `Mapping.kt` 逐字段对应）。
// 生成的 `*Dto` 类型不外泄出本模块。

extension ErrorCodeDto {
    var domain: HostErrorCode {
        switch self {
        case .protocolIncompatible: .protocolIncompatible
        case .unauthorized: .unauthorized
        case .invalidArgument: .invalidArgument
        case .notFound: .notFound
        case .conflict: .conflict
        case .unavailable: .unavailable
        case .timeout: .timeout
        case .cancelled: .cancelled
        case .unsupported: .unsupported
        case .internal: .internal
        }
    }
}

extension FluxError {
    var domain: HostError {
        switch self {
        case let .Rpc(code, reason, retryable, detail):
            HostError(code.domain, reason: reason, retryable: retryable, message: detail)
        case let .Transport(detail):
            HostError(.unavailable, retryable: true, message: detail)
        case .Closed:
            HostError(.cancelled, message: "session closed")
        }
    }
}

/// 生成方法统一 `throws`（无类型）：归一为 `HostError`；未预期的错误（UniFFI 内部错误）记为 internal。
func hostError(_ error: any Error) -> HostError {
    if let flux = error as? FluxError { return flux.domain }
    if let host = error as? HostError { return host }
    return HostError(.internal, message: String(describing: error))
}

extension HostErrorDto {
    var domain: HostError { HostError(code.domain, reason: reason, retryable: retryable, message: message) }
}

extension TaskDto {
    var domain: DownloadTask {
        DownloadTask(
            taskId: taskId,
            url: url,
            originUrl: originUrl,
            fileName: fileName,
            saveDir: saveDir,
            status: TaskStatus(wire: status),
            downloadedBytes: downloadedBytes,
            totalBytes: totalBytes,
            errorMessage: errorMessage,
            createdAt: createdAt,
            completedAt: completedAt,
            queueId: queueId,
            groupId: groupId,
            rssSourceId: rssSourceId,
            fileMissing: fileMissing,
            autoRoute: autoRoute,
            sourceBytes: SourceBytes(cdn: sourceCdn, proxy: sourceProxy, nic: sourceNic),
            uploadedBytes: uploadedBytes,
            seedingStatus: SeedingStatus(wire: seedingStatus),
            seedingMessage: seedingMessage,
            seedingTimeSecs: seedingTimeSecs,
            btSequential: btSequential,
            btFirstLastPiecesFirst: btFirstLastPiecesFirst
        )
    }
}

extension TaskRuntimeDto {
    var domain: TaskRuntime {
        TaskRuntime(
            taskId: taskId,
            sampleSequence: sampleSequence,
            activeTransfers: activeTransfers,
            connectedPeers: connectedPeers,
            totalBytes: totalBytes,
            segments: segments.map {
                Segment(
                    index: $0.index,
                    startByte: $0.startByte,
                    endByte: $0.endByte,
                    downloadedBytes: $0.downloadedBytes,
                    active: $0.active
                )
            }
        )
    }
}

extension QueueOrderDto {
    var domain: QueueOrderEntry { QueueOrderEntry(taskId: taskId, order: order) }
}

extension QueueDto {
    var domain: TaskQueue {
        TaskQueue(
            queueId: queueId,
            name: name,
            speedLimitKbps: speedLimitKbps,
            uploadLimitKbps: uploadLimitKbps,
            maxConcurrent: maxConcurrent,
            defaultSaveDir: defaultSaveDir,
            position: position,
            isRunning: isRunning,
            scheduleEnabled: scheduleEnabled,
            scheduleStart: scheduleStart,
            scheduleStop: scheduleStop,
            scheduleDays: scheduleDays
        )
    }
}

extension GroupDto {
    var domain: DownloadGroup {
        DownloadGroup(groupId: groupId, name: name, sourceUrl: sourceUrl, saveDir: saveDir, createdAt: createdAt)
    }
}

extension RuntimeStatsDto {
    var domain: RuntimeStats {
        RuntimeStats(
            activeTasks: activeTasks,
            pendingTasks: pendingTasks,
            totalDownloadBps: totalDownloadBps,
            totalUploadBps: totalUploadBps,
            diskFreeBytes: diskFreeBytes,
            saveDir: saveDir,
            retryPendingTasks: retryPendingTasks
        )
    }
}

extension FileExistsActionDto {
    var domain: FileExistsAction {
        switch self {
        case .rename: .rename
        case .overwrite: .overwrite
        case .skip: .skip
        }
    }
}

extension FileExistsAction {
    var dto: FileExistsActionDto {
        switch self {
        case .rename: .rename
        case .overwrite: .overwrite
        case .skip: .skip
        }
    }
}

extension SelectionKindDto {
    var domain: SelectionKind {
        switch self {
        case let .hls(options):
            .hls(options.map { HlsOption(index: $0.index, bandwidth: $0.bandwidth, width: $0.width, height: $0.height) })
        case let .bt(files, saveDir):
            .bt(files.map { BtFile(index: $0.index, path: $0.path, size: $0.size) }, saveDir: saveDir)
        case let .variant(options):
            .variant(options.map {
                VariantOption(
                    index: $0.index,
                    label: $0.label,
                    container: $0.container,
                    bandwidth: $0.bandwidth,
                    width: $0.width,
                    height: $0.height,
                    totalBytes: $0.totalBytes
                )
            })
        case let .fileExists(fileName, saveDir, existingSize, existingModifiedUnixMs, incomingSize, renamePreview, actions):
            .fileExists(FileConflict(
                fileName: fileName,
                saveDir: saveDir,
                existingSize: existingSize,
                existingModifiedUnixMs: existingModifiedUnixMs,
                incomingSize: incomingSize,
                renamePreview: renamePreview,
                actions: actions.map(\.domain)
            ))
        case let .plugin(site, url, candidates):
            .plugin(site: site, url: url, candidates: candidates.map {
                PluginCandidate(identity: $0.identity, name: $0.name, version: $0.version, description: $0.description)
            })
        }
    }
}

extension SelectionOutcomeDto {
    var domain: SelectionOutcome {
        switch self {
        case let .hls(index): .hls(index: index)
        case let .bt(indices, saveDir): .bt(indices: indices, saveDir: saveDir)
        case let .variant(index): .variant(index: index)
        case let .fileExists(action): .fileExists(action: action.domain)
        case let .plugin(identity, remember): .plugin(identity: identity, remember: remember)
        case .cancelled: .cancelled
        }
    }
}

extension SelectionOutcome {
    var dto: SelectionOutcomeDto {
        switch self {
        case let .hls(index): .hls(index: index)
        case let .bt(indices, saveDir): .bt(indices: indices, saveDir: saveDir)
        case let .variant(index): .variant(index: index)
        case let .fileExists(action): .fileExists(action: action.dto)
        case let .plugin(identity, remember): .plugin(identity: identity, remember: remember)
        case .cancelled: .cancelled
        }
    }
}

extension SelectionRequestDto {
    var domain: SelectionRequest {
        SelectionRequest(
            requestId: requestId,
            taskId: taskId,
            kind: kind.domain,
            defaultChoice: defaultChoice.domain,
            deadlineUnixMs: deadlineUnixMs
        )
    }
}

extension HostInfoDto {
    var domain: HostInfo {
        HostInfo(
            serviceName: serviceName,
            serviceVersion: serviceVersion,
            protocolVersion: protocolVersion,
            capabilities: Set(capabilities)
        )
    }
}

extension RssSourceDto {
    var domain: RssSource {
        RssSource(
            sourceId: sourceId,
            name: name,
            url: url,
            enabled: enabled,
            autoDownload: autoDownload,
            intervalMinutes: intervalMinutes,
            lastSuccessAt: lastSuccessAt,
            lastError: lastError,
            failCount: failCount,
            unreadCount: unreadCount
        )
    }
}

extension CloudDeviceDto {
    var domain: CloudDevice {
        CloudDevice(
            deviceId: deviceId,
            name: name,
            platform: platform,
            isOnline: isOnline,
            isCurrent: isCurrent,
            appVersion: appVersion,
            defaultSaveDir: defaultSaveDir,
            pathStyle: pathStyle.map(PathStyle.init(wire:))
        )
    }
}

extension LinkDeviceDto {
    var domain: LinkDevice {
        LinkDevice(
            fingerprint: fingerprint,
            name: name,
            platform: platform,
            online: online,
            defaultSaveDir: defaultSaveDir,
            pathStyle: pathStyle.map(PathStyle.init(wire:)),
            customName: customName,
            appVersion: appVersion,
            cloudDeviceId: cloudDeviceId,
            revoked: revoked,
            pairedAt: pairedAt,
            lastSeenAt: lastSeenAt
        )
    }
}

extension CategoryDto {
    var domain: TaskCategory {
        TaskCategory(
            id: id,
            name: name,
            icon: icon,
            extensions: extensions,
            regexPattern: regexPattern,
            urlPattern: urlPattern,
            position: position,
            visible: visible,
            builtinType: builtinType
        )
    }
}

extension HostSnapshotDto {
    var domain: HostSnapshot {
        HostSnapshot(
            info: info.domain,
            daemonConnected: daemonConnected,
            tasks: tasks.map(\.domain),
            tasksLoading: tasksLoading,
            taskTotal: taskTotal,
            runtime: Dictionary(runtime.map { ($0.taskId, $0.domain) }, uniquingKeysWith: { _, last in last }),
            queues: queues.map(\.domain),
            queueOrder: queueOrder.map(\.domain),
            groups: groups.map(\.domain),
            stats: stats.domain,
            priorityTaskId: priorityTaskId,
            pendingSelections: pendingSelections.map(\.domain),
            config: config,
            configRevision: configRevision,
            rssSources: rssSources.map(\.domain),
            cloudDevices: cloudDevices.map(\.domain),
            linkDevices: linkDevices.map(\.domain),
            categories: categories.map(\.domain),
            sections: sections.mapValues { Data($0.utf8) }
        )
    }
}

extension HostEventDto {
    var domain: HostEvent {
        switch self {
        case let .taskChanged(task): .taskChanged(task.domain)
        case let .taskDeleted(taskId): .taskDeleted(taskId: taskId)
        case let .taskProgress(taskId, status, downloadedBytes, totalBytes, speed, uploadSpeed, fileName, errorMessage, uploadedBytes, seedingStatus):
            .taskProgress(TaskProgress(
                taskId: taskId,
                status: status,
                downloadedBytes: downloadedBytes,
                totalBytes: totalBytes,
                speed: speed,
                uploadSpeed: uploadSpeed,
                fileName: fileName,
                errorMessage: errorMessage,
                uploadedBytes: uploadedBytes,
                seedingStatus: seedingStatus
            ))
        case let .taskRuntimeChanged(runtime): .taskRuntimeChanged(runtime.domain)
        case let .queuesChanged(queues): .queuesChanged(queues.map(\.domain))
        case let .queuePositionsChanged(order): .queuePositionsChanged(order.map(\.domain))
        case let .queueOrderChanged(removed, added): .queueOrderChanged(removed: removed, added: added.map(\.domain))
        case let .groupsChanged(groups): .groupsChanged(groups.map(\.domain))
        case let .fileMissingChanged(updates): .fileMissingChanged(updates)
        case let .priorityTaskChanged(taskId): .priorityTaskChanged(taskId: taskId)
        case let .runtimeStatsChanged(stats): .runtimeStatsChanged(stats.domain)
        case let .daemonConnectionChanged(connected): .daemonConnectionChanged(connected: connected)
        case let .selectionPending(request): .selectionPending(request.domain)
        case let .selectionResolved(requestId): .selectionResolved(requestId: requestId)
        case let .configChanged(values, revision): .configChanged(values: values, revision: revision)
        case let .rssSourcesChanged(sources): .rssSourcesChanged(sources.map(\.domain))
        case let .cloudDevicesChanged(devices): .cloudDevicesChanged(devices.map(\.domain))
        case let .linkedDevicesChanged(devices): .linkedDevicesChanged(devices.map(\.domain))
        case let .categoriesChanged(categories): .categoriesChanged(categories.map(\.domain))
        case let .sectionChanged(name, json): .sectionChanged(name: name, json: Data(json.utf8))
        case let .notice(name, json): .notice(name: name, json: Data(json.utf8))
        }
    }
}

extension HostSignalDto {
    var domain: HostSignal {
        switch self {
        case let .snapshot(snapshot): .snapshot(snapshot.domain)
        case let .event(event): .event(event.domain)
        case .stale: .stale
        case let .fatal(error): .fatal(error.domain)
        }
    }
}

extension CreateTaskRequest {
    var dto: CreateTaskRequestDto {
        CreateTaskRequestDto(
            url: url,
            fileName: fileName,
            saveDir: saveDir,
            segments: segments,
            queueId: queueId,
            startPaused: startPaused,
            cookies: cookies,
            referrer: referrer,
            userAgent: userAgent,
            proxyUrl: proxyUrl,
            checksum: checksum,
            ignoreTlsErrors: ignoreTlsErrors,
            headers: headers,
            httpUser: httpUser,
            httpPassword: httpPassword,
            saveSiteAuth: saveSiteAuth,
            unattended: false,
            offeredSaveDir: offeredSaveDir
        )
    }
}
