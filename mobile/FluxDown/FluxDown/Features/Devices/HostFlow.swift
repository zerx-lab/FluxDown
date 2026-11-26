import FluxDomain
import FluxUI

/// 主机切换 / 移除的唯一分发点（设备页与添加主机共用，同 Android `HostActions`）：给出 Toast，
/// 串行化由 `AppContainer.switchHost` 保证；确认框只用于移除（由调用方的 `.alert` 承担）。
@MainActor
enum HostFlow {
    /// 主机连接失败 → 用户可读文案：先按 code（Unauthorized / Timeout / Unavailable / ProtocolIncompatible），其余回退通用失败。
    /// `paired` = 该主机是经局域网配对添加的：被拒时说明授权已撤销、需要重新配对，而不是密钥错误。
    static func errorText(_ error: HostError, paired: Bool = false) -> String {
        switch error.code {
        case .unauthorized: L(paired ? "mobileHostRepairNeeded" : "webLoginInvalidKey")
        case .timeout: L("mobileHostErrTimeout")
        case .unavailable: L("webLoginUnreachable")
        case .protocolIncompatible: L("mobileHostErrIncompatible")
        default: L("localServiceActionFailed")
        }
    }

    /// 已选主机可用时不弹提示；确认当前远端会保存选择，确认本机只阻止自动恢复。
    @discardableResult
    static func switchTo(_ ref: HostRef, container: AppContainer) async -> Bool {
        if ref.id == container.host.id, !(container.session is UnavailableSession) {
            if !ref.isLocal {
                if case .success = await container.switchHost(ref) { return true }
                return false
            }
            container.supersedeHostRestore()
            return true
        }
        let name = ref.localizedName
        container.toasts.show(
            text: L("mobileHostSwitching", ["name": name]),
            tone: .info,
            systemImage: FluxSymbol.switchHost
        )
        switch await container.switchHost(ref) {
        case .success:
            container.toasts.show(text: L("mobileHostSwitched", ["name": name]), tone: .success, systemImage: FluxSymbol.done)
            return true
        case let .failure(error):
            let repair = needsRepair(error, ref: ref, container: container)
            let reason = errorText(error, paired: repair)
            if case let .remote(_, _, endpoint) = ref, repair {
                container.toasts.show(
                    text: L("mobileHostSwitchFailed", ["name": name, "reason": reason]),
                    tone: .error,
                    systemImage: FluxSymbol.offline,
                    actionTitle: L("mobileHostRepair")
                ) { [router = container.router] in router.presentRepair(address: endpoint) }
            } else {
                container.toasts.show(
                    text: L("mobileHostSwitchFailed", ["name": name, "reason": reason]),
                    tone: .error,
                    systemImage: FluxSymbol.offline
                )
            }
            return false
        }
    }

    /// 经局域网配对添加的主机被拒（Unauthorized）= 主机侧撤销了这台手机的控制授权（或重装 / 重配对后凭据失效）：
    /// 不再提示「更换访问密钥」，而是引导重新配对。
    static func needsRepair(_ error: HostError, ref: HostRef, container: AppContainer) -> Bool {
        error.code == .unauthorized && container.linkFingerprint(ofHost: ref.id) != nil
    }

    /// 移除已保存的远程主机（本机不可移除）。移除当前主机时 `AppContainer.removeHost` 会先回到本机。
    static func remove(_ ref: HostRef, container: AppContainer) async {
        guard case let .remote(id, name, _) = ref else { return }
        switch await container.removeHost(id: id) {
        case .success:
            container.toasts.show(text: L("mobileHostRemoved", ["name": name]), tone: .info, systemImage: FluxSymbol.delete)
        case let .failure(error):
            container.toasts.show(text: ErrorText.describe(error), tone: .error, systemImage: "exclamationmark.circle")
        }
    }
}

extension HostRef {
    /// 主机显示名：本机走 i18n（存储里的 displayName 不随语言变化），远端沿用用户起的名称。
    var localizedName: String {
        switch self {
        case .local: L("mobileHostLocalName")
        case let .remote(_, name, _): name
        }
    }

    /// 主机副标题：本机 = 进程内引擎；远端 = 地址。
    var localizedSubtitle: String {
        switch self {
        case .local: L("mobileHostLocalEngine")
        case let .remote(_, _, endpoint): endpoint
        }
    }

    /// 本机 = 当前设备图形（iPhone / iPad）；远端 = `FluxSymbol.remoteHost`。全 App 的主机图标都走这里。
    var symbolName: String {
        isLocal ? FluxSymbol.thisDevice : FluxSymbol.remoteHost
    }
}
