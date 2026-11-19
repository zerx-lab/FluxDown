import FluxDomain
import FluxUI
import SwiftUI

/// X4 · 单个「文件已存在」：文件 + 已有 / 新下载对比 + 动作（重命名为主，覆盖为破坏性，跳过按协议允许）。
///
/// 既是只有一条询问时的根内容，也是多条列表里点进去的详情。倒计时（最早到期的请求）由调用方放在页脚。
struct FileConflictCard: View {
    let request: SelectionRequest
    let conflict: FileConflict
    let resolver: SelectionResolver
    /// 动作区页脚：自动重命名倒计时。
    let deadline: Date

    private func answer(_ action: FileExistsAction) {
        resolver.resolve(request, .fileExists(action: action), successToast: nil, userInitiated: true, onSuccess: {})
    }

    var body: some View {
        List {
            Section {
                HStack(spacing: 12) {
                    KindIcon(kind: FileKind.from(fileName: conflict.fileName), size: 36)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(conflict.fileName)
                            .font(.subheadline.monospaced())
                            .lineLimit(2)
                            .truncationMode(.middle)
                        Text(conflict.saveDir)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                .accessibilityElement(children: .combine)
            } header: {
                Text(L("fileConflictDesc")).textCase(nil)
            }

            Section {
                FileConflictSide(
                    title: L("fileConflictExisting"),
                    size: conflict.existingSize.map { Format.bytes(unsigned: $0).description },
                    modifiedUnixMs: conflict.existingModifiedUnixMs
                )
                FileConflictSide(
                    title: L("fileConflictIncoming"),
                    size: conflict.incomingSize.flatMap { $0 > 0 ? Format.bytes($0).description : nil },
                    modifiedUnixMs: nil
                )
            }

            Section {
                Button { answer(.rename) } label: {
                    Label(L("fileConflictRenameAs", ["name": conflict.renamePreview]), systemImage: FluxSymbol.conflictRename)
                        .fontWeight(.semibold)
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
            }

            Section {
                FileConflictActionRow(
                    title: L("fileConflictOverwrite"),
                    hint: L("fileConflictOverwriteHint"),
                    systemImage: FluxSymbol.conflictOverwrite,
                    role: .destructive
                ) { answer(.overwrite) }
                if conflict.allows(.skip) {
                    FileConflictActionRow(
                        title: L("fileConflictSkip"),
                        hint: L("fileConflictSkipHint"),
                        systemImage: FluxSymbol.conflictSkip,
                        role: nil
                    ) { answer(.skip) }
                }
                FileConflictActionRow(
                    title: L("fileConflictCancelDownload"),
                    hint: L("fileConflictCancelHint"),
                    systemImage: FluxSymbol.conflictCancel,
                    role: nil
                ) { resolver.cancel(request, onSuccess: {}) }
            } footer: {
                FileConflictCountdown(deadline: deadline)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(L("fileConflictTitle"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// 已有 / 新下载的一侧：标题 + 大小 +（已有文件的）修改时间。
private struct FileConflictSide: View {
    let title: String
    let size: String?
    let modifiedUnixMs: Int64?

    var body: some View {
        LabeledContent(title) {
            VStack(alignment: .trailing, spacing: 2) {
                Text(size ?? L("fileConflictSizeUnknown"))
                    .monospacedDigit()
                if let modifiedUnixMs {
                    Text(L("fileConflictModified", ["time": FileConflictFormat.relativeModified(unixMs: modifiedUnixMs)]))
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// 次级动作行：标题 + 说明（说明是决策信息，常驻显示而不是藏进长按）。
private struct FileConflictActionRow: View {
    let title: String
    let hint: String
    let systemImage: String
    let role: ButtonRole?
    let action: () -> Void

    var body: some View {
        Button(role: role, action: action) {
            VStack(alignment: .leading, spacing: 2) {
                Label(title, systemImage: systemImage)
                Text(hint)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .accessibilityHint(hint)
    }
}

/// 「N 秒后自动重命名」：按 deadline 每秒重算；临近到期用警示色。
struct FileConflictCountdown: View {
    let deadline: Date

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let seconds = max(0, Int(deadline.timeIntervalSince(context.date).rounded(.up)))
            Text(L("fileConflictAutoRenameIn", ["seconds": seconds]))
                .monospacedDigit()
                .foregroundStyle(seconds <= 5 ? Color.fdStatusWarningText : Color.secondary)
        }
    }
}

enum FileConflictFormat {
    /// 已有文件的修改时间（与任务行同一套「刚刚 / N 分钟前 …」文案）。
    static func relativeModified(unixMs: Int64) -> String {
        RowText.live(units: .legacy).relative(unixMs / 1000, now: Int64(Date().timeIntervalSince1970))
    }

    /// 列表行副标题：已有大小 · 修改时间（未知项省略）。
    static func summary(_ conflict: FileConflict) -> String {
        var parts: [String] = []
        if let size = conflict.existingSize { parts.append(Format.bytes(unsigned: size).description) }
        if let ms = conflict.existingModifiedUnixMs { parts.append(relativeModified(unixMs: ms)) }
        if let incoming = conflict.incomingSize, incoming > 0 { parts.append("→ " + Format.bytes(incoming).description) }
        return parts.joined(separator: " · ")
    }
}
