import FluxDomain
import SwiftUI

/// 多选底栏「删除」待确认的请求：本机任务 + 适用于删除的远程任务（`RemoteTaskRules.canIssue(.delete)` 已过滤）。
nonisolated struct SelectionDeleteRequest {
    let localTasks: [DownloadTask]
    let remoteTasks: [RemoteTaskDto]

    var count: Int { localTasks.count + remoteTasks.count }
}

extension View {
    /// 多选删除确认框：保留文件 / 连同文件两档（本机与远程共用同一选择；远程的「连同文件」由目标设备在线时执行）。
    func selectionDeleteDialog(
        _ request: Binding<SelectionDeleteRequest?>,
        onConfirm: @escaping (SelectionDeleteRequest, _ withFiles: Bool) -> Void
    ) -> some View {
        alert(
            request.wrappedValue.map { $0.count == 1 ? L("deleteTask") : L("mobileDeleteNTitle", ["n": $0.count]) } ?? "",
            isPresented: Binding(get: { request.wrappedValue != nil }, set: { if !$0 { request.wrappedValue = nil } }),
            presenting: request.wrappedValue
        ) { pending in
            Button(L("deleteTaskAndFile"), role: .destructive) { onConfirm(pending, true) }
            Button(L("deleteTask"), role: .destructive) { onConfirm(pending, false) }
            Button(L("cancel"), role: .cancel) {}
        } message: { pending in
            if pending.count == 1 {
                let name = pending.localTasks.first?.fileName ?? pending.remoteTasks.first?.displayName ?? ""
                Text(L("deleteConfirmDescKeepFile", ["fileName": name]))
            } else {
                Text(L("mobileDeleteNMessage"))
            }
        }
    }
}
