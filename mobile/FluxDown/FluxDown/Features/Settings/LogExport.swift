import FluxDomain
import FluxUI
import Foundation
import Observation
import SwiftUI
import os

// 日志工具（03-settings §12 / §13）：日志占用上限、当前日志大小、导出日志。
// `LogsSection` 同时用于「诊断」页与「关于」页。

// MARK: - 远端 HTTP 导出

/// 远端 `--server` 主机的日志导出：`GET <host>/api/web/logs/export`（Bearer 访问密钥）。
/// `agent.diagnostics.exportLogs` 会写主机上的任意路径，服务器模式的网关拒绝它，只留 HTTP 这条路（同 Web `exportLogs`）。
nonisolated enum LogExportHTTP {
    static let path = "/api/web/logs/export"

    /// 保存的主机地址（`host[:port]` / `http(s)://…` / `ws(s)://…[/base][/rpc]`）→ 导出地址。
    /// 反向代理子路径保留；`/rpc`、查询串、片段丢弃（同 `native/mobile` 的 `normalize_endpoint`）。
    static func exportURL(endpoint: String) -> URL? {
        var text = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        if !text.contains("://") { text = "http://" + text }
        guard var components = URLComponents(string: text) else { return nil }
        switch components.scheme?.lowercased() {
        case "ws", "http": components.scheme = "http"
        case "wss", "https": components.scheme = "https"
        default: return nil
        }
        guard let host = components.host, !host.isEmpty else { return nil }
        var base = components.path
        while base.hasSuffix("/") { base.removeLast() }
        if base.hasSuffix("/rpc") { base.removeLast(4) }
        components.path = base + path
        components.query = nil
        components.fragment = nil
        return components.url
    }

    /// 下载到 `file`；失败映射为 `HostError`（401 / 403 = 访问密钥被拒）。
    @concurrent
    static func download(from url: URL, accessKey: String, to file: URL) async throws(HostError) {
        var request = URLRequest(url: url)
        request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 180 // 主机现打包日志，大目录需要时间
        let temporary: URL
        let response: URLResponse
        do {
            (temporary, response) = try await URLSession.shared.download(for: request)
        } catch let error as URLError where error.code == .cancelled {
            throw HostError(.cancelled)
        } catch let error as URLError where error.code == .appTransportSecurityRequiresSecureConnection {
            throw HostError(.unavailable, reason: "cleartextBlocked", message: error.localizedDescription)
        } catch let error as URLError {
            throw HostError(.unavailable, retryable: true, message: error.localizedDescription)
        } catch {
            throw HostError(.internal, message: error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw HostError(.unavailable, retryable: true, message: "no HTTP response")
        }
        guard (200 ..< 300).contains(http.statusCode) else {
            switch http.statusCode {
            case 401, 403: throw HostError(.unauthorized, message: "HTTP \(http.statusCode)")
            default: throw HostError(.unavailable, retryable: http.statusCode >= 500, message: "HTTP \(http.statusCode)")
            }
        }
        do {
            try FileManager.default.moveItem(at: temporary, to: file)
        } catch {
            throw HostError(.internal, message: error.localizedDescription)
        }
    }
}

// MARK: - 日志目录大小

nonisolated enum LogSizes {
    private static let log = Logger(subsystem: "com.fluxdown.app", category: "diagnostics")

    /// 目录（递归）里普通文件的总字节数；目录不在本机 → nil。
    @concurrent
    static func directorySize(_ path: String) async -> UInt64? {
        guard !path.isEmpty else { return nil }
        let url = URL(fileURLWithPath: path, isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), isDirectory.boolValue else { return nil }
        guard let walker = FileManager.default.enumerator(
            at: url, includingPropertiesForKeys: [.isRegularFileKey, .fileSizeKey], options: [.skipsHiddenFiles]
        ) else { return nil }
        var total: UInt64 = 0
        while let next = walker.nextObject() {
            guard let file = next as? URL else { continue }
            do {
                let values = try file.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                if values.isRegularFile == true, let size = values.fileSize { total += UInt64(size) }
            } catch {
                log.notice("log size: \(error.localizedDescription, privacy: .public)")
            }
        }
        return total
    }

    /// 去重后（agent / daemon 可能共用目录）累加；全部不在本机 → nil。
    static func total(of directories: [String]) async -> UInt64? {
        var seen: Set<String> = []
        var sum: UInt64?
        for directory in directories where !directory.isEmpty {
            let key = URL(fileURLWithPath: directory).standardizedFileURL.path
            guard seen.insert(key).inserted else { continue }
            if let size = await directorySize(directory) { sum = (sum ?? 0) + size }
        }
        return sum
    }
}

// MARK: - 模型

@MainActor
@Observable
final class LogExportModel {
    private(set) var isExporting = false
    /// 最近一次导出的失败说明（行内显示）。
    private(set) var failure: String?
    /// 本机日志总大小；远端主机 / 目录不在本机 → nil（不显示该行）。
    private(set) var sizeText: String?

    @ObservationIgnored private let log = Logger(subsystem: "com.fluxdown.app", category: "diagnostics")

    /// 刷新当前日志大小：只在本机主机上统计（日志目录路径来自 `agent.diagnostics.logPaths`）。
    func refreshSize(container: AppContainer) async {
        guard container.isLocalHost, container.store.state.connection == .live else {
            sizeText = nil
            return
        }
        do throws(HostError) {
            let paths: LogPathsDto = try await container.session.call(HostMethod.agentDiagnosticsLogPaths)
            let total = await LogSizes.total(of: [paths.agentLogDir, paths.daemonLogDir])
            sizeText = total.map { Format.bytes(unsigned: $0).description }
        } catch {
            log.notice("log paths unavailable: \(error.description, privacy: .public)")
            sizeText = nil
        }
    }

    func export(container: AppContainer) async {
        guard !isExporting else { return }
        isExporting = true
        failure = nil
        defer { isExporting = false }

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("fluxdown-log-export-\(UUID().uuidString)", isDirectory: true)
        defer { cleanUp(directory) }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let target = directory.appendingPathComponent("fluxdown-logs.zip")
            let produced: URL
            switch container.host {
            case .local:
                produced = try await exportLocally(container: container, target: target)
            case let .remote(id, _, endpoint):
                try await exportRemotely(id: id, endpoint: endpoint, target: target)
                produced = target
            }
            guard await NotifyActivityPresenter.present(items: [produced]) else {
                failure = L("logExportFailed")
                return
            }
            container.toasts.show(text: L("logExportSuccessNotice"), tone: .success)
        } catch let error as HostError {
            failure = Self.text(for: error)
        } catch {
            log.error("log export failed: \(error.localizedDescription, privacy: .public)")
            failure = L("logExportFailed")
        }
    }

    private func exportLocally(container: AppContainer, target: URL) async throws(HostError) -> URL {
        let result: LogExportResult = try await container.session.call(
            HostMethod.agentDiagnosticsExportLogs, params: LogExportParams(targetPath: target.path)
        )
        let reported = URL(fileURLWithPath: result.path)
        if FileManager.default.fileExists(atPath: reported.path) { return reported }
        if FileManager.default.fileExists(atPath: target.path) { return target }
        throw HostError(.internal, message: "exported archive is missing")
    }

    private func exportRemotely(id: String, endpoint: String, target: URL) async throws(HostError) {
        guard let url = LogExportHTTP.exportURL(endpoint: endpoint) else {
            throw HostError(.invalidArgument, message: "invalid host address")
        }
        guard let key = try HostRepo().accessKey(id: id) else {
            throw HostError(.unauthorized, message: "access key unavailable")
        }
        try await LogExportHTTP.download(from: url, accessKey: key, to: target)
    }

    private func cleanUp(_ directory: URL) {
        do {
            try FileManager.default.removeItem(at: directory)
        } catch {
            log.notice("temp cleanup failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    static func text(for error: HostError) -> String {
        if error.code == .unauthorized { return L("webLoginInvalidKey") }
        if error.reason == "cleartextBlocked" { return L("mobileLogExportCleartext") }
        return ErrorText.describe(error)
    }
}

// MARK: - 视图

/// 日志分组：占用上限（`log_max_size_mb`，MB）、当前日志大小（仅本机）、导出日志（分享面板）。
struct LogsSection: View {
    @Environment(AppContainer.self) private var container
    @Environment(HostStore.self) private var store
    @Environment(ConfigEditor.self) private var editor
    @Environment(AppearanceStore.self) private var appearance

    @State private var model = LogExportModel()

    static let maxSizeItem = SettingsItem(
        id: "logs.maxSize", key: "log_max_size_mb", titleKey: "logMaxSize", detailKey: "logMaxSizeDesc"
    )
    static let exportID = "logs.export"

    var body: some View {
        let readOnly = store.state.isReadOnly
        Section {
            if editor.form.has(Self.maxSizeItem.key) {
                ConfigNumberRow(
                    item: Self.maxSizeItem, range: 1 ... 1024, fallback: 10,
                    unitHint: L("mobileLogSizeUnitLabel", ["unit": appearance.fileSizeUnits == .iec ? "MiB" : "MB"])
                )
                    .disabled(readOnly)
            }
            if let size = model.sizeText {
                SettingsInfoRow(title: L("mobileLogCurrentSize"), value: size)
                    .settingsRow("logs.currentSize")
            }
            VStack(alignment: .leading, spacing: 6) {
                SettingsActionRow(
                    title: L("logExportButton"), runningTitle: L("mobileLogExporting"),
                    systemImage: FluxSymbol.share, isRunning: model.isExporting
                ) {
                    Task { await model.export(container: container) }
                }
                .disabled(readOnly && !model.isExporting)
                if let failure = model.failure {
                    SettingsStatusLine(text: failure, tone: .failure)
                        .transition(.opacity)
                }
            }
            .settingsRow(Self.exportID)
        } header: {
            Text(L("mobileLogsTitle"))
        } footer: {
            Text(L("logExportDesc"))
        }
        .fluxAnimation(.smooth, value: model.failure)
        .task(id: LogRefreshKey(host: container.host.id, live: store.state.connection == .live)) {
            await model.refreshSize(container: container)
        }
    }

    private nonisolated struct LogRefreshKey: Hashable {
        var host: String
        var live: Bool
    }

    /// 搜索索引（日志行）：由「诊断」页并入。
    static func searchEntries(_ ctx: SettingsSearchContext, route: SettingsRoute, breadcrumb: String) -> [SettingsEntry] {
        var entries: [SettingsEntry] = []
        if ctx.form.has(maxSizeItem.key) {
            entries.append(SettingsEntry(item: maxSizeItem, route: route, breadcrumb: breadcrumb, symbol: "text.page.badge.magnifyingglass"))
        }
        entries.append(SettingsEntry(
            id: exportID, route: route, title: L("logExportButton"), detail: L("logExportDesc"),
            breadcrumb: breadcrumb, symbol: FluxSymbol.share
        ))
        return entries
    }
}
