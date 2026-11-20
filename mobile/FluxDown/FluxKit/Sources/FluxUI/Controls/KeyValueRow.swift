import SwiftUI
import UIKit

/// 详情键值行（§9.9）：`LabeledContent`，键 `.secondary`、值右对齐；AX 字号下键在上、值在下。
///
/// - `monospaced`：哈希 / 路径 / 链接用等宽 footnote，不截断、可换行。
/// - `tone`：状态语义文字（失败 / 做种 / 警告）用 §3.E 的加深文字色。
/// - 值可选中（`.textSelection(.enabled)`）；`copyable` 时长按上下文菜单提供「复制」
///   （写入 `UIPasteboard` + success 触感，并回调 `onCopy` 供页面弹 Toast）。
/// - `copyLabel` 由调用方本地化。
/// - `copyValue` 可提供未本地化的诊断原文；缺省复制显示值。
public struct KeyValueRow: View {
    public let key: String
    public let value: String
    public let monospaced: Bool
    public let copyable: Bool
    public let tone: BadgeTone?
    public let copyLabel: String
    public let onCopy: (() -> Void)?
    private let copyValue: String?

    @State private var copyCount = 0
    @Environment(\.fluxAccent) private var accent

    public init(
        key: String,
        value: String,
        monospaced: Bool = false,
        copyable: Bool = false,
        tone: BadgeTone? = nil,
        copyLabel: String = "Copy",
        copyValue: String? = nil,
        onCopy: (() -> Void)? = nil
    ) {
        self.key = key
        self.value = value
        self.monospaced = monospaced
        self.copyable = copyable
        self.tone = tone
        self.copyLabel = copyLabel
        self.copyValue = copyValue
        self.onCopy = onCopy
    }

    public var body: some View {
        LabeledContent {
            valueText
        } label: {
            Text(key).foregroundStyle(.secondary)
        }
        .labeledContentStyle(KeyValueLabeledContentStyle())
        .frame(minHeight: 44)
        .contextMenu {
            if copyable {
                Button(copyLabel, systemImage: FluxSymbol.copy, action: copy)
            }
        }
        .sensoryFeedback(FluxHaptic.success.sensoryFeedback, trigger: copyCount)
        .accessibilityElement(children: .combine)
        .accessibilityActions {
            if copyable {
                Button(copyLabel, action: copy)
            }
        }
    }

    private var valueText: some View {
        Text(value)
            .font(monospaced ? .fluxMono : .subheadline)
            .foregroundStyle(tone?.textColor(accent: accent) ?? .primary)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func copy() {
        UIPasteboard.general.string = copyValue ?? value
        copyCount += 1
        onCopy?()
    }
}

/// 键值布局：常规字号键左值右；AX 字号纵排、左对齐。
private struct KeyValueLabeledContentStyle: LabeledContentStyle {
    @Environment(\.dynamicTypeSize) private var typeSize

    func makeBody(configuration: Configuration) -> some View {
        if typeSize.isAccessibilitySize {
            VStack(alignment: .leading, spacing: 2) {
                configuration.label.font(.subheadline)
                configuration.content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                configuration.label.font(.subheadline)
                Spacer(minLength: 0)
                configuration.content.multilineTextAlignment(.trailing)
            }
        }
    }
}

#Preview("KeyValueRow") {
    List {
        KeyValueRow(key: "Status", value: "Downloading")
        KeyValueRow(key: "Error", value: "Connection reset by peer", tone: .failure)
        KeyValueRow(key: "Save to", value: "/var/mobile/Containers/Data/Downloads", monospaced: true, copyable: true)
        KeyValueRow(key: "SHA-256", value: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08", monospaced: true, copyable: true)
    }
}

#Preview("KeyValueRow · 深色 / AX3") {
    List {
        KeyValueRow(key: "URL", value: "https://example.com/files/ubuntu-24.04.iso", monospaced: true, copyable: true)
        KeyValueRow(key: "Status", value: "Seeding", tone: .success)
    }
    .dynamicTypeSize(.accessibility3)
    .preferredColorScheme(.dark)
}
