import FluxDomain
import FluxUI
import SwiftUI

extension View {
    /// 标签根页标题：iOS 26 `inlineLarge`——大字标题与工具栏按钮同处导航栏一行，不再单独占一整行大标题区
    /// （`.large` 在按钮行下方再叠约 52pt 标题行，空态 / 短列表时顶部大片留白）。
    /// `subtitle` 显示在标题下方（当前主机 · 连接状态），同样不额外占行。
    func rootNavigationTitle(_ title: String, subtitle: String? = nil) -> some View {
        navigationTitle(title)
            .navigationSubtitle(subtitle ?? "")
            .toolbarTitleDisplayMode(.inlineLarge)
    }
}

extension Connection {
    /// 标题副行里的连接状态（在线时不显示）。
    var statusText: String? {
        switch self {
        case .live: nil
        case .connecting: L("mobileHostConnConnecting")
        case .stale: L("mobileHostConnStale")
        case .failed: L("mobileHostConnFailed")
        }
    }
}

/// 根页标题副行：「主机名」或「主机名 · 连接状态」。
func hostTitleSubtitle(_ host: HostRef, status: String?) -> String {
    status.map { host.localizedName + " · " + $0 } ?? host.localizedName
}

/// 根页标题菜单（`toolbarTitleMenu`）：单选已保存主机 + 「添加主机」。下载 / 订阅共用。
struct HostTitleMenuItems: View {
    /// 当前主机的连接状态文案（在线 = nil），附在当前主机名后。
    let status: String?

    @Environment(AppContainer.self) private var container

    var body: some View {
        Picker(L("mobileHostSwitchTitle"), selection: Binding(
            get: { container.host.id },
            set: { id in
                guard let ref = container.hosts.first(where: { $0.id == id }) else { return }
                Task { await HostFlow.switchTo(ref, container: container) }
            }
        )) {
            ForEach(container.hosts) { ref in
                Label(title(ref), systemImage: ref.symbolName).tag(ref.id)
            }
        }
        .pickerStyle(.inline)
        .disabled(container.isSwitching)
        Divider()
        Button(L("mobileHostAdd"), systemImage: FluxSymbol.add) { container.router.sheet = .addHost }
    }

    private func title(_ ref: HostRef) -> String {
        ref.id == container.host.id ? hostTitleSubtitle(ref, status: status) : ref.localizedName
    }
}
