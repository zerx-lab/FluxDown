import FluxDomain
import FluxUI
import SwiftUI

/// V3 设备详情：云端受信任设备（完整：信息 / 下发 / 重命名 / 删除 / 该设备的远程任务）与局域网已配对设备（信息 / 下发 / 解除配对）。
/// 设备在别处被删除 / 解除配对（或主机切换、退出登录）时自动返回上一页并提示。
struct DeviceDetailScreen: View {
    nonisolated enum Target: Hashable {
        /// `CloudDeviceRecord.id`（云端行 id）。
        case cloud(id: String)
        case link(fingerprint: String)
    }

    let target: Target

    var body: some View {
        switch target {
        case let .cloud(id): CloudDeviceDetail(id: id)
        case let .link(fingerprint): LinkDeviceDetail(fingerprint: fingerprint)
        }
    }
}

private struct DeviceDispatchRequest: Identifiable {
    let target: DispatchTarget
    let sourceHostID: String

    var id: String { target.id }
}

// MARK: - 云端设备

private struct CloudDeviceDetail: View {
    @Environment(AppContainer.self) private var container
    @Environment(DevicesModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let id: String
    @State private var renaming: CloudDeviceRecord?
    @State private var deleting: CloudDeviceRecord?
    @State private var dispatchRequest: DeviceDispatchRequest?
    /// 已由本页（删除成功）或消失检测返回，避免重复提示 / 重复 dismiss。
    @State private var leaving = false

    var body: some View {
        let state = container.store.state
        let record = model.cloudRecords.first { $0.id == id }
        let gone = dispatchRequest == nil && record == nil && model.cloudPhase != .loading
        Group {
            if let record {
                detailList(record, state: state)
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityLabel(L("mobileLoading"))
            }
        }
        .navigationTitle(record?.name ?? L("accountDeviceDetailTitle"))
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: gone, initial: true) { _, isGone in
            guard isGone, !leaving else { return }
            leaving = true
            container.toasts.show(text: L("mobileDeviceGone"), tone: .info, systemImage: "minus.circle")
            dismiss()
        }
        .cloudDeviceRenameAlert(renaming: $renaming)
        .sheet(item: $dispatchRequest) { request in
            DispatchSheet(target: request.target, sourceHostID: request.sourceHostID)
        }
    }

    private func detailList(_ record: CloudDeviceRecord, state: HostState) -> some View {
        let presenceKnown = model.presenceKnown(state)
        let online = presenceKnown && record.isOnline
        let readOnly = state.isReadOnly
        let sourceHostID = container.host.id
        let statusText = L(CloudPresence.deviceKey(isOnline: record.isOnline, presenceKnown: presenceKnown))
        return List {
            Section {
                DeviceHeader(
                    platform: record.platform,
                    name: record.name,
                    online: online,
                    status: statusText,
                    isCurrent: record.isCurrent
                )
            }

            Section {
                DeviceInfoRow(key: L("accountDeviceFieldOnline"), value: statusText, tone: online ? .success : nil)
                DeviceInfoRow(key: L("accountDeviceFieldPlatform"), value: DevicePresentation.label(platform: record.platform))
                DeviceInfoRow(key: L("accountDeviceFieldAppVersion"), value: record.appVersion)
                DeviceInfoRow(key: L("accountDeviceFieldLastIp"), value: record.lastIp)
                DeviceInfoRow(key: L("accountDeviceFieldCreatedAt"), value: Self.dateText(record.createdAt))
                DeviceInfoRow(key: L("accountDeviceFieldLastSeenAt"), value: Self.dateText(record.lastSeenAt))
                DeviceInfoRow(key: L("accountDeviceFieldSaveDir"), value: record.defaultSaveDir, monospaced: true, copyable: true)
                DeviceInfoRow(key: L("mobileDeviceFieldPathStyle"), value: pathStyleLabel(record.effectivePathStyle))
                DeviceInfoRow(key: L("accountDeviceFieldId"), value: record.deviceId, monospaced: true, copyable: true)
            } footer: {
                if !presenceKnown { Text(L("cloudConnectionStatusHint")) }
            }

            Section {
                if !record.isCurrent, model.cloudRecordsHostID == sourceHostID {
                    Button {
                        // 同机既有云端记录又已配对时，优先投递到在线的直连设备。
                        let attached = state.linkDevices.first {
                            !$0.revoked && !$0.fingerprint.isEmpty && $0.cloudDeviceId == record.deviceId
                        }.flatMap { model.linkInfo($0.fingerprint, state: state) }
                        dispatchRequest = DeviceDispatchRequest(
                            target: DispatchTarget(cloud: record, presenceKnown: presenceKnown, link: attached),
                            sourceHostID: sourceHostID
                        )
                    } label: {
                        Label(L("mobileDeviceDispatchAction"), systemImage: "arrow.down.circle")
                    }
                    .disabled(readOnly)
                }
                Button {
                    renaming = record
                } label: {
                    Label(L("accountDeviceRenameTitle"), systemImage: FluxSymbol.edit)
                }
                .disabled(readOnly)
                Button(role: .destructive) {
                    deleting = record
                } label: {
                    Label(L("accountDeviceDeleteAction"), systemImage: FluxSymbol.delete)
                }
                .disabled(readOnly)
                .cloudDeviceDeleteConfirmation(record, deleting: $deleting) { _ in
                    leaving = true
                    dismiss()
                }
            } footer: {
                if readOnly {
                    Text(L("localServiceDisconnected"))
                } else if !record.isCurrent, presenceKnown, !record.isOnline {
                    Text(L("downloadToOfflineHint"))
                }
            }

            if state.has(HostCapability.agentRemoteTasks), !record.isCurrent {
                remoteTasksSection(record, state: state, presenceKnown: presenceKnown, readOnly: readOnly)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable {
            if let error = await model.loadCloud() {
                container.toasts.show(text: AccountText.error(error), tone: .error)
            }
        }
    }

    private func remoteTasksSection(_ record: CloudDeviceRecord, state: HostState, presenceKnown: Bool, readOnly: Bool) -> some View {
        let tasks = RemoteTaskRules.sorted(model.remoteTasks(state).filter { $0.toDevice == record.deviceId })
        return Section {
            if tasks.isEmpty {
                Text(L("mobileRemoteTasksDeviceEmpty"))
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            ForEach(tasks) { task in
                RemoteTaskRow(
                    task: task,
                    targetName: record.name,
                    showsTarget: false,
                    targetOnline: presenceKnown ? record.isOnline : nil,
                    busy: model.isBusy(task),
                    readOnly: readOnly
                ) { action, deleteFiles in
                    model.issue(action, to: task, deleteFiles: deleteFiles)
                }
            }
        } header: {
            Text(L("remoteTasksGroup"))
        }
    }

    /// 云端 ISO 时间 → 本地化日期；无法解析则原样显示（空串隐藏）。
    private static func dateText(_ iso: String) -> String? {
        let trimmed = iso.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return AccountText.dateTime(iso: trimmed) ?? trimmed
    }
}

// MARK: - 局域网设备

private struct LinkDeviceDetail: View {
    @Environment(AppContainer.self) private var container
    @Environment(DevicesModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let fingerprint: String
    @State private var dispatchRequest: DeviceDispatchRequest?
    @State private var renamingLink: LinkRenameTarget?
    @State private var confirmingUnpair = false
    @State private var unpairing = false
    @State private var leaving = false

    var body: some View {
        let state = container.store.state
        let info = model.linkInfo(fingerprint, state: state)
        let gone = dispatchRequest == nil && info == nil && state.connection == .live
        Group {
            if let info {
                detailList(info, state: state)
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityLabel(L("mobileLoading"))
            }
        }
        .navigationTitle(info?.name ?? L("accountDeviceDetailTitle"))
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: gone, initial: true) { _, isGone in
            guard isGone, !leaving else { return }
            leaving = true
            container.toasts.show(text: L("mobileDeviceGone"), tone: .info, systemImage: "minus.circle")
            dismiss()
        }
        .sheet(item: $dispatchRequest) { request in
            DispatchSheet(target: request.target, sourceHostID: request.sourceHostID)
        }
    }

    private func detailList(_ info: LinkDeviceInfo, state: HostState) -> some View {
        let readOnly = state.isReadOnly
        let sourceHostID = container.host.id
        let revoked = info.revoked
        let online = info.online && !revoked
        let statusText = L(online ? "deviceOnline" : "deviceOffline")
        let sameAccount = DeviceRules.attachedLinkFingerprints(cloud: state.cloudDevices, link: state.linkDevices)
            .contains(info.fingerprint)
        return List {
            Section {
                DeviceHeader(
                    platform: info.platform,
                    name: info.name,
                    online: online,
                    status: statusText,
                    isCurrent: false
                )
                if revoked {
                    StatusBadge(text: L("linkedDeviceRevokedTag"), tone: .warning)
                } else if sameAccount {
                    StatusBadge(text: L("linkedDeviceSameAccountTag"), tone: .neutral)
                }
                if info.grantsControl, !revoked {
                    StatusBadge(text: L("linkControlGrantedTag"), tone: .warning)
                }
            }

            Section {
                DeviceInfoRow(key: L("accountDeviceFieldOnline"), value: statusText, tone: online ? .success : nil)
                DeviceInfoRow(key: L("accountDeviceFieldPlatform"), value: DevicePresentation.label(platform: info.platform))
                DeviceInfoRow(key: L("accountDeviceFieldAppVersion"), value: info.appVersion)
                DeviceInfoRow(key: L("mobileDeviceFieldPairedAt"), value: AccountText.dateTime(unix: info.pairedAt))
                DeviceInfoRow(key: L("accountDeviceFieldLastSeenAt"), value: AccountText.dateTime(unix: info.lastSeenAt))
                DeviceInfoRow(key: L("accountDeviceFieldSaveDir"), value: info.defaultSaveDir, monospaced: true, copyable: true)
                DeviceInfoRow(key: L("mobileDeviceFieldPathStyle"), value: pathStyleLabel(info.effectivePathStyle))
                DeviceInfoRow(key: L("mobileDeviceFieldFingerprint"), value: info.fingerprint, monospaced: true, copyable: true)
            }

            Section {
                if !revoked {
                    Button {
                        dispatchRequest = DeviceDispatchRequest(target: DispatchTarget(link: info), sourceHostID: sourceHostID)
                    } label: {
                        Label(L("mobileDeviceDispatchAction"), systemImage: "arrow.down.circle")
                    }
                    .disabled(readOnly)
                    Button {
                        renamingLink = LinkRenameTarget(fingerprint: info.fingerprint, customName: info.customName ?? "")
                    } label: {
                        Label(L("linkedDeviceRename"), systemImage: FluxSymbol.edit)
                    }
                    .disabled(readOnly)
                }
                Button(role: .destructive) {
                    confirmingUnpair = true
                } label: {
                    HStack {
                        Label(L("linkedDeviceRemove"), systemImage: "minus.circle")
                        if unpairing {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(readOnly || unpairing)
                .alert(
                    L("linkedDeviceRemoveTitle"),
                    isPresented: $confirmingUnpair
                ) {
                    Button(L("linkedDeviceRemove"), role: .destructive) { unpair(info) }
                    Button(L("cancel"), role: .cancel) {}
                } message: {
                    Text(L("linkedDeviceRemoveDesc", ["name": info.name]))
                }
            } footer: {
                if readOnly {
                    Text(L("localServiceDisconnected"))
                } else if revoked {
                    Text(L("linkedDeviceRevokedDesc"))
                } else if !info.online {
                    Text(L("errReasonPeerOffline"))
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable {
            if let error = await model.loadLink() {
                container.toasts.show(text: AccountText.error(error, context: .pairing), tone: .error)
            }
        }
        .linkDeviceRenameAlert(renaming: $renamingLink)
    }

    private func unpair(_ info: LinkDeviceInfo) {
        guard !unpairing else { return }
        unpairing = true
        Task {
            if let error = await model.unpair(fingerprint: info.fingerprint) {
                container.toasts.show(text: AccountText.error(error, context: .pairing), tone: .error)
                unpairing = false
                return
            }
            leaving = true
            dismiss()
        }
    }
}

// MARK: - 共用部件

/// 详情头部：平台图标 56 pt + 名称 + 在线点与状态文字 +「当前设备」徽标。
private struct DeviceHeader: View {
    let platform: String?
    let name: String
    let online: Bool
    let status: String
    let isCurrent: Bool

    var body: some View {
        HStack(spacing: 16) {
            GlyphTile(
                systemImage: DevicePresentation.symbol(platform: platform),
                tint: online ? .primary : .secondary,
                size: 56
            )
            .overlay(alignment: .bottomTrailing) {
                PresenceMark(online: online).offset(x: 4, y: 4)
            }
            VStack(alignment: .leading, spacing: 6) {
                Text(name)
                    .font(.title3.weight(.semibold))
                    .lineLimit(3)
                Text(status)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                if isCurrent {
                    StatusBadge(text: L("accountDeviceCurrent"), tone: .accent)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }
}

/// 信息行：值为空则整行隐藏；可复制的值（设备 ID / 指纹 / 目录）长按复制并 Toast。
private struct DeviceInfoRow: View {
    @Environment(AppContainer.self) private var container

    let key: String
    let value: String?
    var monospaced = false
    var copyable = false
    var tone: BadgeTone?

    var body: some View {
        if let value, !value.isEmpty {
            KeyValueRow(
                key: key,
                value: value,
                monospaced: monospaced,
                copyable: copyable,
                tone: tone,
                copyLabel: L("webCopy")
            ) {
                container.toasts.show(text: L("webCopied"), tone: .success)
            }
        }
    }
}

/// 路径风格显示名（未知风格原样显示；未上报 / 推断不出则不显示）。
private func pathStyleLabel(_ style: PathStyle?) -> String? {
    switch style {
    case nil: nil
    case .windows?: "Windows"
    case .posix?: "POSIX"
    case let .unknown(raw)?: raw.isEmpty ? nil : raw
    }
}
