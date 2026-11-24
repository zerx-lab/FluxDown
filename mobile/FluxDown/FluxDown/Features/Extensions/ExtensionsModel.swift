import FluxDomain
import FluxUI
import Foundation
import Observation
import SwiftUI
#if FULL_FLAVOR

/// S11 插件页的状态与操作（Web `PluginsTab` / `MarketSection` 的逻辑层）。
///
/// 插件列表本身来自 `daemon.plugins` 分区（事件驱动），这里只持有进行中的操作、市场目录与待确认弹窗。
@MainActor
@Observable
final class ExtensionsModel {
    nonisolated enum InstallPhase: Equatable {
        case uploading(Double)
        case installing
    }

    nonisolated enum MarketPhase: Equatable {
        case idle, loading, loaded
        case failed(String)
    }

    /// 权限确认请求（新装列全部权限；更新只列新增权限）。
    nonisolated struct PermissionRequest: Identifiable, Equatable {
        let entry: MarketEntry
        let installed: PluginDto?
        /// 确认期间市场里的版本变了：按最新版本重新确认。
        var versionChanged = false

        var id: String { entry.id }
        var isUpdate: Bool { installed != nil }
        var permissions: [String] { PluginMarket.permissionsToConfirm(entry, installed: installed) }
    }

    nonisolated enum Tab: Hashable {
        case plugins, components
    }

    /// 由插件页根视图呈现的 Sheet（行滑动 / 详情页按钮共用）。
    nonisolated enum PluginSheet: Identifiable, Hashable {
        case settings(String)
        case auth(String)
        case loadError(String)

        var id: String {
            switch self {
            case let .settings(identity): "settings:\(identity)"
            case let .auth(identity): "auth:\(identity)"
            case let .loadError(identity): "error:\(identity)"
            }
        }
    }

    var tab: Tab = .plugins
    var sheet: PluginSheet?
    private(set) var busy: Set<String> = []
    private(set) var installPhase: InstallPhase?
    var operationFailure: String?
    /// 安装 / 重载成功但缺基础组件：弹窗提醒（提醒式，不阻断）。
    var missingComponents: [String]?
    struct ConflictNotice {
        let identity: String
        let plugins: [PluginDto]
    }

    private var conflictNotices: [ConflictNotice] = []
    var conflictNotice: ConflictNotice? { conflictNotices.first }

    func dismissConflictNotice() {
        if !conflictNotices.isEmpty { conflictNotices.removeFirst() }
    }

    /// 递增即要求所有被推入的子页返回（「前往组件设置」前先回到根页）。
    private(set) var popRequest = 0

    // 市场
    private(set) var marketPhase: MarketPhase = .idle
    private(set) var marketEntries: [MarketEntry] = []
    private(set) var marketPending: Set<String> = []
    var permissionRequest: PermissionRequest?
    private(set) var siteDefaults: [PluginSiteDefault] = []
    private(set) var siteDefaultBusy: Set<String> = []

    @ObservationIgnored private let container: AppContainer
    @ObservationIgnored private var marketRequested = false

    /// 单个插件包大小上限（超限报 `pluginErrorPackageTooLarge`）。
    nonisolated static let maxPackageBytes = 10 * 1024 * 1024

    init(container: AppContainer) {
        self.container = container
    }

    private var session: any HostSession { container.session }

    private func toast(_ text: String, _ tone: ToastTone) {
        container.toasts.show(text: text, tone: tone)
    }

    func reportOperationFailure(_ key: String, _ error: HostError) {
        operationFailure = L(key, ["message": ExtensionErrorText.pluginFailure(error)])
    }

    func reportPackageImportFailure(_ error: any Error) {
        operationFailure = ExtensionErrorText.packageImportFailure(error)
    }

    func requestPop() { popRequest += 1 }

    func goToComponents() {
        requestPop()
        tab = .components
    }

    // MARK: 已安装插件

    nonisolated private struct FeedbackAuditParams: Encodable, Sendable {
        let identity: String
        let limit: Int
    }

    nonisolated private struct FeedbackAuditPage: Decodable, Sendable {
        nonisolated struct Entry: Decodable, Sendable { let outcome: String }
        let entries: [Entry]
    }

    func feedbackAuditCounts(identity: String) async -> (error: Int, denied: Int)? {
        let limit = 20
        do {
            let page: FeedbackAuditPage = try await session.call(
                HostMethod.daemonPluginAuditList, params: FeedbackAuditParams(identity: identity, limit: limit)
            )
            var counts = (error: 0, denied: 0)
            for entry in page.entries.prefix(limit) {
                switch entry.outcome {
                case "error": counts.error += 1
                case "denied": counts.denied += 1
                default: break
                }
            }
            return counts
        } catch {
            return nil
        }
    }

    private func withBusy(_ identity: String, _ action: () async -> Void) async {
        guard !busy.contains(identity) else { return }
        busy.insert(identity)
        await action()
        busy.remove(identity)
    }

    func setEnabled(_ plugin: PluginDto, _ enabled: Bool) async {
        await withBusy(plugin.identity) {
            let target = session
            do throws(HostError) {
                try await target.callVoid(
                    HostMethod.daemonPluginSetEnabled,
                    params: PluginSetEnabledParams(identity: plugin.identity, enabled: enabled)
                )
                if enabled { await noteResolverConflicts(target, identity: plugin.identity) }
            } catch {
                toast(L("pluginOpEnabledFailed", ["message": ExtensionErrorText.describe(error)]), .error)
            }
        }
    }

    @discardableResult
    func uninstall(_ plugin: PluginDto) async -> Bool {
        var done = false
        await withBusy(plugin.identity) {
            do throws(HostError) {
                try await session.callVoid(HostMethod.daemonPluginUninstall, params: PluginIdentityParams(identity: plugin.identity))
                toast(L("pluginOpUninstallSuccess"), .success)
                done = true
            } catch {
                toast(L("pluginOpUninstallFailed", ["message": ExtensionErrorText.describe(error)]), .error)
            }
        }
        return done
    }

    // MARK: 站点默认插件

    func loadSiteDefaults() async {
        do throws(HostError) {
            siteDefaults = try await session.call(HostMethod.daemonPluginSiteDefaults)
        } catch {
            // 旧 daemon / 无插件构建不支持该方法：保持空列表，整段不渲染。
            siteDefaults = []
        }
    }

    /// `identity == nil` 移除条目；否则确认 / 修改。完成后重新拉取列表。
    func setSiteDefault(site: String, identity: String?) async {
        guard !siteDefaultBusy.contains(site) else { return }
        siteDefaultBusy.insert(site)
        do throws(HostError) {
            try await session.callVoid(
                HostMethod.daemonPluginSetSiteDefault,
                params: PluginSetSiteDefaultParams(site: site, identity: identity)
            )
        } catch {
            toast(L("pluginSiteDefaultSaveFailed", ["error": ExtensionErrorText.describe(error)]), .error)
        }
        siteDefaultBusy.remove(site)
        await loadSiteDefaults()
    }

    func reload(_ plugin: PluginDto) async {
        await withBusy(plugin.identity) {
            operationFailure = nil
            do throws(HostError) {
                let result: InstalledPlugin = try await session.call(
                    HostMethod.daemonPluginReloadDev,
                    params: PluginIdentityParams(identity: plugin.identity)
                )
                operationFailure = nil
                toast(L("pluginOpReloadSuccess"), .success)
                noteMissing(result)
            } catch {
                reportOperationFailure("pluginOpReloadFailed", error)
            }
        }
    }

    private func noteMissing(_ result: InstalledPlugin) {
        if !result.missingComponents.isEmpty { missingComponents = result.missingComponents }
    }

    private func noteResolverConflicts(_ target: any HostSession, identity: String) async {
        guard !identity.isEmpty, target === session else { return }
        // The PluginsChanged projection may still be behind the mutation response.
        let plugins: [PluginDto]
        do throws(HostError) {
            plugins = try await target.call(HostMethod.daemonPluginList)
        } catch {
            // A failed read does not turn a successful mutation into a failure.
            return
        }
        guard target === session, plugins.contains(where: { plugin in
            plugin.resolverUrlConflicts.contains { plugin.identity == identity || $0.otherIdentity == identity }
        }) else { return }
        conflictNotices.append(ConflictNotice(identity: identity, plugins: plugins))
    }

    // MARK: 文件安装

    /// `.fileImporter` 选中的 `.fxplug` / `.zip`（仅远端主机）：先上传 blob（`POST /api/web/blobs/plugins`）
    /// 再 `daemon.plugin.install`。
    func installFile(_ url: URL) async {
        guard installPhase == nil else { return }
        operationFailure = nil
        installPhase = .installing
        defer { installPhase = nil }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let data: Data
        do {
            data = try Data(contentsOf: url, options: .mappedIfSafe)
        } catch {
            reportPackageImportFailure(error)
            return
        }
        guard data.count <= Self.maxPackageBytes else {
            reportOperationFailure("pluginOpInstallFailed", HostError(.invalidArgument, reason: "pluginPackageTooLarge"))
            return
        }
        let target = session
        do throws(HostError) {
            let result = try await installRemote(data, target: target)
            operationFailure = nil
            toast(L("pluginOpInstallSuccess"), .success)
            await noteResolverConflicts(target, identity: result.identity)
            noteMissing(result)
        } catch {
            reportOperationFailure("pluginOpInstallFailed", error)
        }
    }

    private func installRemote(_ data: Data, target: any HostSession) async throws(HostError) -> InstalledPlugin {
        guard case let .remote(id, _, endpoint) = container.host else {
            throw HostError(.invalidArgument, message: "not a remote host")
        }
        guard let accessKey = try HostRepo().accessKey(id: id) else {
            throw HostError(.unauthorized, message: "access key unavailable")
        }
        installPhase = .uploading(0)
        let blobId = try await PluginBlobUploader.upload(data, endpoint: endpoint, accessKey: accessKey) { [weak self] fraction in
            Task { @MainActor in
                guard let self, case .uploading = self.installPhase else { return }
                self.installPhase = .uploading(fraction)
            }
        }
        installPhase = .installing
        return try await target.call(HostMethod.daemonPluginInstall, params: PluginInstallParams(blobId: blobId))
    }

    // MARK: 市场

    /// 首次进入插件页且连接就绪时拉取一次（GPUI `ensure_market_loaded`）。
    func ensureMarketLoaded() async {
        guard !marketRequested, !container.store.state.isReadOnly else { return }
        await loadMarket()
    }

    func loadMarket() async {
        marketRequested = true
        if marketPhase == .loading { return }
        marketPhase = .loading
        do throws(HostError) {
            let entries: [MarketEntry] = try await session.call(HostMethod.daemonPluginMarketList)
            marketEntries = entries
            marketPhase = .loaded
        } catch {
            let message = ExtensionErrorText.describe(error)
            marketPhase = .failed(message)
            // 已有目录时覆盖层不显示失败态：用 Toast 告知刷新失败。
            if !marketEntries.isEmpty {
                toast(L("marketLoadFailed", ["message": message]), .error)
            }
        }
    }

    /// 市场里每个插件的最新条目。
    var marketLatest: [MarketEntry] { PluginMarket.latestPerPlugin(marketEntries) }

    /// 已安装插件的可用更新（市场目录已加载时）。
    func update(for plugin: PluginDto) -> MarketEntry? {
        guard let entry = marketLatest.first(where: { $0.pluginId == plugin.identity }),
              PluginMarket.action(for: entry, installed: plugin) == .update
        else { return nil }
        return entry
    }

    func requestInstall(_ entry: MarketEntry, installed: PluginDto?) {
        guard !marketPending.contains(entry.pluginId) else { return }
        operationFailure = nil
        let request = PermissionRequest(entry: entry, installed: installed)
        if request.permissions.isEmpty {
            Task { await performInstall(request) }
        } else {
            permissionRequest = request
        }
    }

    func confirmInstall(_ request: PermissionRequest) {
        permissionRequest = nil
        Task { await performInstall(request) }
    }

    private func performInstall(_ request: PermissionRequest) async {
        let entry = request.entry
        guard !marketPending.contains(entry.pluginId) else { return }
        operationFailure = nil
        marketPending.insert(entry.pluginId)
        defer { marketPending.remove(entry.pluginId) }
        let target = session
        do throws(HostError) {
            let result: InstalledPlugin = try await target.call(
                HostMethod.daemonPluginMarketInstall,
                params: PluginMarketInstallParams(pluginId: entry.pluginId, version: entry.version)
            )
            operationFailure = nil
            toast(L(request.isUpdate ? "pluginOpUpdateSuccess" : "pluginOpInstallSuccess"), .success)
            await noteResolverConflicts(target, identity: result.identity)
            noteMissing(result)
        } catch {
            // 用户确认的版本已不是最新：刷新目录，按新版本重新确认权限。
            if error.reason == "marketVersionChanged" {
                await loadMarket()
                if let fresh = marketLatest.first(where: { $0.pluginId == entry.pluginId }), fresh.installable {
                    let next = PermissionRequest(entry: fresh, installed: request.installed, versionChanged: true)
                    if !next.permissions.isEmpty {
                        permissionRequest = next
                        return
                    }
                }
            }
            let key = request.isUpdate ? "pluginOpUpdateFailed" : "pluginOpInstallFailed"
            reportOperationFailure(key, error)
        }
    }
}

// MARK: - 远端 blob 上传

/// `POST /api/web/blobs/plugins`：远端 `--server` 主机的文件面（Bearer 访问密钥，原始字节）→ `blobId`。
enum PluginBlobUploader {
    private struct Reply: Decodable {
        let blobId: String?
        let error: String?
    }

    static func upload(
        _ data: Data,
        endpoint: String,
        accessKey: String,
        progress: @escaping @Sendable (Double) -> Void
    ) async throws(HostError) -> String {
        guard let base = ApiAddress.baseURL(endpoint) else {
            throw HostError(.invalidArgument, message: "invalid host address")
        }
        var request = URLRequest(url: base.appendingPathComponent("api/web/blobs/plugins"))
        request.httpMethod = "POST"
        request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization")
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.timeoutInterval = 120
        let delegate = ProgressDelegate(onProgress: progress)
        let body: Data
        let response: URLResponse
        do {
            (body, response) = try await URLSession.shared.upload(for: request, from: data, delegate: delegate)
        } catch let error as URLError where error.code == .cancelled {
            throw HostError(.cancelled)
        } catch let error as URLError {
            throw HostError(.unavailable, retryable: true, message: error.localizedDescription)
        } catch {
            throw HostError(.internal, message: error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw HostError(.unavailable, retryable: true, message: "no HTTP response")
        }
        let reply = try? ProtocolJSON.makeDecoder().decode(Reply.self, from: body)
        if (200 ..< 300).contains(http.statusCode), let blobId = reply?.blobId { return blobId }
        let message = reply?.error ?? "HTTP \(http.statusCode)"
        switch http.statusCode {
        case 401, 403: throw HostError(.unauthorized, message: message)
        case 413: throw HostError(.invalidArgument, reason: "pluginPackageTooLarge", message: message)
        case 400 ..< 500: throw HostError(.invalidArgument, message: message)
        default: throw HostError(.unavailable, retryable: true, message: message)
        }
    }

    /// 上传进度回调（`URLSession` 在自己的队列调用；闭包本身 `@Sendable` 且不可变）。
    private final class ProgressDelegate: NSObject, URLSessionTaskDelegate, Sendable {
        let onProgress: @Sendable (Double) -> Void

        init(onProgress: @escaping @Sendable (Double) -> Void) {
            self.onProgress = onProgress
        }

        func urlSession(
            _ session: URLSession,
            task: URLSessionTask,
            didSendBodyData bytesSent: Int64,
            totalBytesSent: Int64,
            totalBytesExpectedToSend: Int64
        ) {
            guard totalBytesExpectedToSend > 0 else { return }
            onProgress(Double(totalBytesSent) / Double(totalBytesExpectedToSend))
        }
    }
}

// MARK: - 熔断自动禁用提示

/// `PluginAutoDisabled` 一次性通知 → toast（`pluginAutoDisabledToast`）。
/// 只处理挂载之后到达的通知（不重放历史）；可挂在任意常驻视图上获得全局提示。
struct PluginAutoDisabledToasts: ViewModifier {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @State private var handled: Int?
    @State private var plugins = SectionMemo<[PluginDto]>(empty: [])

    func body(content: Content) -> some View {
        content
            .onAppear { if handled == nil { handled = store.notices.last?.id ?? 0 } }
            .onChange(of: store.notices.last?.id) { _, _ in consume() }
    }

    private func consume() {
        let last = handled ?? 0
        let pending = store.notices.filter { $0.id > last }
        guard let newest = pending.last else { return }
        handled = newest.id
        let known = plugins.value(store.state.sections[HostSection.daemonPlugins])
        for notice in pending where notice.name == HostNoticeName.pluginAutoDisabled {
            guard let payload = notice.decode(PluginAutoDisabledNotice.self) else { continue }
            let name = known.first { $0.identity == payload.identity }?.name ?? payload.identity
            let toastKey = payload.reason == "Revoked" ? "pluginRevokedToast" : "pluginAutoDisabledToast"
            container.toasts.show(text: L(toastKey, ["name": name]), tone: .warning)
        }
    }
}

extension View {
    /// 插件被熔断器自动禁用时弹 toast。
    func pluginAutoDisabledToasts() -> some View {
        modifier(PluginAutoDisabledToasts())
    }
}
#endif
