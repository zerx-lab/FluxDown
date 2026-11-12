import FluxDomain
import FluxUI
import SwiftUI
import UIKit
import UserNotifications

/// S10 · 通知：系统授权卡、完成 / 失败 / 选择请求通知开关、免打扰下载、Webhook 入口。
///
/// 通知由客户端按主机状态本地发出（`NotificationService`）；授权**懒申请**——进入本页看到说明后由用户点「允许通知」，
/// 或首次打开某个通知开关时申请。没有灵动岛 / 实时活动行（本 App 没有此功能）。
struct NotifyPage: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase

    @Bindable private var device = DeviceSettings.shared
    private let service = NotificationService.shared

    @State private var isRequesting = false
    @State private var isSendingTest = false

    static let completeItem = SettingsItem(
        id: "notify.onComplete", key: "download.notify_on_complete", titleKey: "notifyOnComplete", detailKey: "notifyOnCompleteDesc"
    )
    static let permissionID = "notify.permission"
    static let actionsID = "notify.actions"
    static let failureID = "notify.onFailure"
    static let selectionID = "notify.selection"
    static let webhookID = "notify.webhook"

    private var downloadContext: SettingsDownloadContext {
        SettingsDownloadContext(form: editor.form, isLocalHost: container.isLocalHost)
    }

    var body: some View {
        let readOnly = store.state.isReadOnly
        let ctx = downloadContext
        let completeOn = editor.form.bool(Self.completeItem.key)
        SettingsPage(title: L("settingsCatNotify"), showsReadOnlyBanner: true, showsSyncLegend: true) {
            permissionSection
            Section {
                completeRow
                    .disabled(readOnly)
                toggleRow(
                    id: Self.actionsID, title: L("mobileNotifActions"), detail: L("mobileNotifActionsDesc"), synced: false,
                    isOn: $device.notifyActions
                )
                .disabled(!completeOn)
                toggleRow(
                    id: Self.failureID, title: L("mobileNotifOnFailure"), detail: L("mobileNotifOnFailureDesc"), synced: false,
                    isOn: failureBinding
                )
                SettingsText(title: L("mobileNotifSelection"), detail: L("mobileNotifSelectionDesc"))
                    .settingsRow(Self.selectionID)
            } header: {
                Text(L("notifyGroupSystem"))
            } footer: {
                Text(L("mobileNotifSystemFooter"))
            }

            if SettingsDownloadRow.silentDownload.isVisible(in: ctx) {
                Section(L("silentDownload")) {
                    ConfigToggleRow(row: .silentDownload)
                    if SettingsDownloadRow.silentAutoCategory.isVisible(in: ctx) {
                        ConfigToggleRow(row: .silentAutoCategory)
                            .transition(.opacity.combined(with: .move(edge: .top)))
                    }
                    if SettingsDownloadRow.silentSkipSelection.isVisible(in: ctx) {
                        ConfigToggleRow(row: .silentSkipSelection)
                            .transition(.opacity.combined(with: .move(edge: .top)))
                    }
                }
                .disabled(readOnly)
            }

            if FeatureFlags.webhooks {
                Section {
                    NavigationLink(value: SettingsRoute.webhook) {
                        SettingsTileLabel(
                            title: L("webhookNavTitle"), subtitle: L("mobileNotifWebhookDesc"),
                            symbol: "bolt.horizontal.fill", color: .indigo
                        )
                    }
                    .settingsRow(Self.webhookID)
                } header: {
                    Text(L("notifyGroupWebhook"))
                }
            }
        }
        .fluxAnimation(.smooth, value: ctx.form)
        .fluxAnimation(.smooth, value: service.authorization)
        .task { await service.refreshAuthorization() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await service.refreshAuthorization() } }
        }
    }

    // MARK: 授权卡

    private struct Permission {
        var symbol: String
        var color: Color
        var title: String
        var detail: String
    }

    private var permission: Permission {
        switch service.authorization {
        case .authorized, .ephemeral:
            Permission(
                symbol: "bell.badge.fill", color: Color.fdStatusSeedingText,
                title: L("mobileNotifPermOnTitle"), detail: L("mobileNotifPermOnDetail")
            )
        case .provisional:
            Permission(
                symbol: "bell.fill", color: Color.fdStatusWarningText,
                title: L("mobileNotifPermQuietTitle"), detail: L("mobileNotifPermQuietDetail")
            )
        case .denied:
            Permission(
                symbol: "bell.slash.fill", color: Color.fdStatusWarningText,
                title: L("mobileNotifPermDeniedTitle"), detail: L("mobileNotifPermDeniedDetail")
            )
        case .notDetermined:
            Permission(
                symbol: "bell.badge", color: Color.secondary,
                title: L("mobileNotifPermAskTitle"), detail: L("mobileNotifPermAskDetail")
            )
        @unknown default:
            Permission(
                symbol: "bell", color: Color.secondary,
                title: L("mobileNotifPermAskTitle"), detail: L("mobileNotifPermAskDetail")
            )
        }
    }

    private var permissionSection: some View {
        let card = permission
        return Section {
            VStack(alignment: .leading, spacing: 12) {
                Label {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(card.title).font(.headline)
                        Text(card.detail)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                } icon: {
                    Image(systemName: card.symbol)
                        .font(.title2)
                        .foregroundStyle(card.color)
                        .accessibilityHidden(true)
                }
            }
            .accessibilityElement(children: .contain)
            .settingsRow(Self.permissionID)
            permissionActions
        }
    }

    @ViewBuilder
    private var permissionActions: some View {
        switch service.authorization {
        case .notDetermined:
            SettingsActionRow(title: L("mobileNotifAllow"), systemImage: "bell.badge", isRunning: isRequesting) { requestPermission() }
        case .provisional:
            SettingsActionRow(title: L("mobileNotifUpgrade"), systemImage: "bell.badge", isRunning: isRequesting) { requestPermission() }
        case .denied:
            SettingsActionRow(title: L("doctorActionOpenSettings"), systemImage: "gearshape") { openSystemSettings() }
        case .authorized, .ephemeral:
            SettingsActionRow(title: L("doctorActionTestNotification"), systemImage: "paperplane", isRunning: isSendingTest) { sendTest() }
        @unknown default:
            EmptyView()
        }
    }

    private func requestPermission() {
        guard !isRequesting else { return }
        isRequesting = true
        Task {
            await service.requestAuthorization()
            isRequesting = false
        }
    }

    private func sendTest() {
        guard !isSendingTest else { return }
        isSendingTest = true
        Task {
            let sent = await service.sendTest()
            isSendingTest = false
            container.toasts.show(
                text: sent ? L("doctorTestNotificationSent") : L("mobileNotifTestFailed"), tone: sent ? .success : .error
            )
        }
    }

    private func openSystemSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        openURL(url) { accepted in
            if !accepted { container.toasts.show(text: L("mobileNoSettingsApp"), tone: .error) }
        }
    }

    /// 首次打开通知开关时（尚未询问过）申请授权。
    private func requestIfUndetermined() {
        guard service.authorization == .notDetermined else { return }
        requestPermission()
    }

    // MARK: 开关行

    private var completeRow: some View {
        let item = Self.completeItem
        return Toggle(isOn: Binding(
            get: { editor.form.bool(item.key) },
            set: { on in
                editor.set(item.key, SettingsConfigForm.wire(on))
                if on { requestIfUndetermined() }
            }
        )) {
            SettingsText(title: item.title, detail: item.detail, synced: item.isSynced)
        }
        .tint(Color.fdToggleOn)
        .settingsRow(item.id, failureKey: item.key)
    }

    private var failureBinding: Binding<Bool> {
        Binding(
            get: { device.notifyOnFailure },
            set: { on in
                device.notifyOnFailure = on
                if on { requestIfUndetermined() }
            }
        )
    }

    private func toggleRow(id: String, title: String, detail: String, synced: Bool, isOn: Binding<Bool>) -> some View {
        Toggle(isOn: isOn) {
            SettingsText(title: title, detail: detail, synced: synced)
        }
        .tint(Color.fdToggleOn)
        .settingsRow(id)
    }
}

extension NotifyPage {
    /// 设置搜索索引：本页所有可见行（与页面渲染共用可见性判定）。
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatNotify")
        let system = "\(name) › \(L("notifyGroupSystem"))"
        let symbol = "bell.badge.fill"
        var entries = [
            SettingsEntry(
                id: permissionID, route: .notify, title: L("mobileNotifPermTitle"), detail: L("mobileNotifPermAskDetail"),
                breadcrumb: name, symbol: symbol
            ),
            SettingsEntry(item: completeItem, route: .notify, breadcrumb: system, symbol: symbol),
            SettingsEntry(
                id: actionsID, route: .notify, title: L("mobileNotifActions"), detail: L("mobileNotifActionsDesc"),
                breadcrumb: system, symbol: symbol
            ),
            SettingsEntry(
                id: failureID, route: .notify, title: L("mobileNotifOnFailure"), detail: L("mobileNotifOnFailureDesc"),
                breadcrumb: system, symbol: symbol
            ),
            SettingsEntry(
                id: selectionID, route: .notify, title: L("mobileNotifSelection"), detail: L("mobileNotifSelectionDesc"),
                breadcrumb: system, symbol: symbol
            ),
        ]
        let download = SettingsDownloadContext(form: ctx.form, isLocalHost: ctx.isLocalHost)
        let silent = "\(name) › \(L("silentDownload"))"
        if SettingsDownloadRow.silentDownload.isVisible(in: download) {
            entries.append(SettingsEntry(item: SettingsDownloadRow.silentDownload.item, route: .notify, breadcrumb: silent, symbol: symbol))
        }
        if SettingsDownloadRow.silentSkipSelection.isVisible(in: download) {
            entries.append(SettingsEntry(item: SettingsDownloadRow.silentSkipSelection.item, route: .notify, breadcrumb: silent, symbol: symbol))
        }
        if FeatureFlags.webhooks {
            entries.append(SettingsEntry(
                id: webhookID, route: .notify, title: L("webhookNavTitle"), detail: L("mobileNotifWebhookDesc"),
                breadcrumb: "\(name) › \(L("notifyGroupWebhook"))", symbol: "bolt.horizontal.fill"
            ))
        }
        return entries
    }

    /// 设置首页读数：授权被拒 / 待授权优先，其次按完成 / 失败通知开关给出「开 / 仅失败 / 关」。
    static func readout(_ ctx: SettingsSearchContext) -> String? {
        readout(
            authorization: NotificationService.shared.authorization,
            notifyOnComplete: ctx.form.bool(completeItem.key),
            notifyOnFailure: DeviceSettings.shared.notifyOnFailure
        )
    }

    static func readout(authorization: UNAuthorizationStatus, notifyOnComplete: Bool, notifyOnFailure: Bool) -> String {
        if authorization == .denied { return L("mobileNotifReadoutBlocked") }
        guard notifyOnComplete || notifyOnFailure else { return L("mobileNotifReadoutOff") }
        if authorization == .notDetermined { return L("mobileNotifReadoutNeedsPermission") }
        return notifyOnComplete ? L("mobileNotifReadoutOn") : L("mobileNotifReadoutFailuresOnly")
    }
}
