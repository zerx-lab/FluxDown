import FluxDomain
import Foundation

/// 构建 flavor（`mobile/FluxDown/scripts/build-core.sh --flavor`）。
///
/// - full（侧载 / 开发默认）：编译条件 `FULL_FLAVOR` 开启，本机引擎含完整 P2P，远端主机的插件 / 组件管理可用。
/// - store（App Store / TestFlight）：不定义 `FULL_FLAVOR`（工程设置 `FLUX_FLAVOR_CONDITIONS` 置空），
///   本机与远端主机**一律**不含 P2P（BT / eD2K）与插件 / 组件功能：入口、页面与处理代码在编译期剔除，
///   审核所见即全部功能（App Review 5.6 / 2.3.1 / 5.2.3）。远端主机上已有的 P2P 任务只按普通任务显示与控制。
nonisolated enum BuildFlavor {
    /// 按 flavor 选文案键：store 用不含 P2P 字样的通用键。
    static func key(full: String, store: String) -> String {
        #if FULL_FLAVOR
        return full
        #else
        return store
        #endif
    }
}

/// 需要 P2P 能力才能处理的链接（magnet / ed2k / `torrent-file://` 哨兵）。store 构建对任何主机都不提交。
nonisolated enum P2PLink {
    static func isP2P(_ url: String) -> Bool {
        let proto = protocolOf(url)
        return proto == .bt || proto == .ed2k
    }
}

nonisolated extension SelectionRequest {
    /// 本构建是否处理该选择请求。store 不含 BT 文件选择与插件（候选插件 / 插件变体）选择：这类请求不弹出、
    /// 不提醒，由其它客户端答复或主机按超时默认值解析；主机关闭了 BT「自动选择默认项」时 BT 请求没有超时，
    /// 任务停在「准备中」直到其它客户端作答，或在本端暂停 / 删除（暂停会撤销请求，继续时重新询问）。
    var isPresentable: Bool {
        #if FULL_FLAVOR
        return true
        #else
        switch kind {
        case .bt, .plugin, .variant: return false
        case .hls, .fileExists: return true
        }
        #endif
    }
}

nonisolated extension Array where Element == SelectionRequest {
    /// 本构建可呈现的选择请求（保持主机给出的顺序）。
    var presentable: [SelectionRequest] { filter(\.isPresentable) }
}

nonisolated extension DownloadTask {
    /// 本构建是否按「做种中」呈现该任务。store 不含做种：远端主机的做种任务按已完成显示，
    /// 不提供暂停做种、做种页与做种限制。
    var presentsSeeding: Bool {
        #if FULL_FLAVOR
        return isSeeding
        #else
        return false
        #endif
    }

    /// 本构建是否提供「继续做种」。
    var presentsResumeSeeding: Bool {
        #if FULL_FLAVOR
        return canResumeSeeding
        #else
        return false
        #endif
    }
}
