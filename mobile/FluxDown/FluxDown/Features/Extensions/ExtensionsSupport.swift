import CoreImage
import CoreImage.CIFilterBuiltins
import FluxDomain
import FluxUI
import Foundation
import SwiftUI
import UIKit

// 扩展 / Webhook / API 服务三页共用的小构件：分区解码缓存、错误文案、换行布局、剪贴板、二维码。
// 插件 / 组件专属的构件（错误文案、外链、二维码、组件标题）只在 full flavor 编译（`FULL_FLAVOR`）。

// MARK: - 分区解码缓存

/// 通用分区（`HostState.sections`）的解码缓存：只在 JSON 变化时重新解码（`HostState.section` 每次调用都解码）。
@MainActor
final class SectionMemo<Value: Decodable & Sendable> {
    private let empty: Value
    private var source: Data?
    private var cached: Value
    private var primed = false

    init(empty: Value) {
        self.empty = empty
        cached = empty
    }

    func value(_ data: Data?) -> Value {
        if primed, data == source { return cached }
        primed = true
        source = data
        if let data, let decoded = try? ProtocolJSON.makeDecoder().decode(Value.self, from: data) {
            cached = decoded
        } else {
            cached = empty
        }
        return cached
    }
}

#if FULL_FLAVOR
// MARK: - 错误文案

enum ExtensionErrorText {
    /// 插件 / 组件 / 市场错误：先按 `reason`（`ErrorReason` wire 名）映射可操作文案，再按码回退
    /// （`web/.../extensions/errors.ts` ↔ GPUI `error_text`）。
    static func describe(_ error: HostError) -> String {
        if let reason = error.reason, let key = reasonKeys[reason] { return L(key) }
        switch error.code {
        case .invalidArgument, .notFound: return L("localServiceInvalidArgument")
        case .conflict: return L("localServiceConflict")
        case .unsupported: return L("settingsUnsupportedOnPlatform")
        default: return L("localServiceActionFailed")
        }
    }

    static func pluginFailure(_ error: HostError) -> String {
        guard let detail = error.pluginDiagnostic else { return describe(error) }
        switch detail.kind {
        case "manifestUnreadable": return L("pluginDiagnosticManifestUnreadable")
        case "manifestUnknownField": return L("pluginDiagnosticManifestUnknownField")
        case "manifestJsonInvalid": return L("pluginDiagnosticManifestJsonInvalid")
        case "manifestIdentityInvalid": return L("pluginDiagnosticManifestIdentityInvalid")
        case "manifestNameMissing": return L("pluginDiagnosticManifestNameMissing")
        case "manifestVersionInvalid": return L("pluginDiagnosticManifestVersionInvalid")
        case "manifestMinAppVersionInvalid": return L("pluginDiagnosticManifestMinAppVersionInvalid")
        case "manifestUnknownPermission": return L("pluginDiagnosticManifestUnknownPermission")
        case "manifestAuthPermissionMissing": return L("pluginDiagnosticManifestAuthPermissionMissing")
        case "scriptCompileFailed": return L("pluginDiagnosticScriptCompileFailed")
        case "scriptCompileFailedAt":
            guard let line = detail.line, let column = detail.column else {
                return L("pluginDiagnosticScriptCompileFailed")
            }
            return L("pluginDiagnosticScriptCompileFailedAt", ["line": line, "column": column])
        case "missingFile":
            guard let path = detail.path, !path.isEmpty else { return describe(error) }
            return L("pluginDiagnosticMissingFile", ["path": path])
        case "loadFailed": return L("pluginDiagnosticLoadFailed")
        case "notDevPlugin": return L("pluginDiagnosticNotDevPlugin")
        default: return describe(error)
        }
    }

    /// File picker and read errors can contain local paths and provider details.
    static func packageImportFailure(_ error: any Error) -> String? {
        let key: String
        switch (error as? CocoaError)?.code {
        case .userCancelled: return nil
        case .fileNoSuchFile, .fileReadNoSuchFile: key = "mobileFileNotFound"
        case .fileReadNoPermission: key = "pluginImportFileNoPermission"
        default: key = "pluginImportFileUnreadable"
        }
        return L("pluginOpInstallFailed", ["message": L(key)])
    }

    private static let reasonKeys: [String: String] = [
        "marketUnreachable": "pluginErrorMarketUnreachable",
        "marketIndexInvalid": "pluginErrorMarketIndexInvalid",
        "marketIndexRollback": "pluginErrorMarketIndexRollback",
        "pluginNotInMarket": "pluginErrorNotInMarket",
        "pluginYanked": "pluginErrorYanked",
        "pluginDownloadFailed": "pluginErrorDownloadFailed",
        "pluginPackageTooLarge": "pluginErrorPackageTooLarge",
        "pluginPackageInvalid": "pluginErrorPackageInvalid",
        "marketVersionChanged": "pluginErrorMarketVersionChanged",
    ]
}
#endif

// MARK: - 换行布局

/// 徽标流：子视图按行排列，放不下自动换行。
nonisolated struct FlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        var width: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > maxWidth {
                y += rowHeight + spacing
                x = 0
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
            width = max(width, x - spacing)
        }
        return CGSize(width: width, height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += rowHeight + spacing
                x = bounds.minX
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

// MARK: - 剪贴板与链接

enum ExtensionsClipboard {
    @MainActor
    static func copy(_ text: String) {
        UIPasteboard.general.string = text
    }
}

#if FULL_FLAVOR
/// 外链：只放行 http(s)，其余原样当文本展示（挑战 / 主页均不可信）。
struct SafeLinkText: View {
    let text: String
    var font: Font = .footnote

    var body: some View {
        if let url = PluginAuth.safeHTTPURL(text) {
            Link(text, destination: url)
                .font(font)
                .lineLimit(1)
                .truncationMode(.middle)
        } else {
            Text(text)
                .font(font)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }
}

// MARK: - 二维码

enum QRCodeImage {
    /// 本地用 `CIQRCodeGenerator`（纠错级别 M）编码原文，不解析也不请求其中的 URL；
    /// 空串 / 超长 / 编码失败返回 nil（调用侧保留文本与复制入口）。
    @MainActor
    static func make(_ text: String, scale: CGFloat = 10) -> UIImage? {
        guard !text.isEmpty, text.count <= PluginAuth.maxQRTextLength else { return nil }
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext()
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cgImage)
    }
}
#endif

// MARK: - 版本 / 时间文案

enum ExtensionsFormat {
    /// Unix 毫秒 → 本地化的日期 + 时间（本地时区；GPUI `format_unix` 的 iOS 等价写法）。
    static func timestamp(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
        return date.formatted(date: .abbreviated, time: .shortened)
    }

    /// 毫秒延迟 → 本地化的缩写时长（如 `120 ms`）。
    static func latency(_ ms: Int64) -> String {
        Duration.milliseconds(ms).formatted(.units(allowed: [.milliseconds], width: .abbreviated))
    }

    /// 失败且无错误文本时的 HTTP 状态文案。
    static func httpStatus(_ code: Int) -> String {
        L("mobileWebhookHttpStatus", ["code": code])
    }

    /// 推送结果摘要：成功 `状态码 · 延迟`；失败为错误信息或 HTTP 状态码。
    static func deliverySummary(_ delivery: WebhookDelivery) -> String {
        if delivery.success { return "\(delivery.statusCode) · \(latency(delivery.latencyMs))" }
        return delivery.error.isEmpty ? httpStatus(delivery.statusCode) : delivery.error
    }
}

#if FULL_FLAVOR
/// 组件标题键（依赖提醒与组件页共用）。
enum ComponentTitles {
    static func titleKey(_ kind: ComponentKind) -> String? {
        switch kind {
        case .ffmpeg: "componentsFfmpegTitle"
        case .ytdlp: "componentsYtdlpTitle"
        case .unknown: nil
        }
    }

    static func title(wire: String) -> String {
        if let key = titleKey(ComponentKind(wire: wire)) { return L(key) }
        return wire
    }
}
#endif
