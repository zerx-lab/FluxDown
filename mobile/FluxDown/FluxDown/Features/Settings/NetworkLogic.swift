import FluxDomain
import Foundation

// 网络与代理页（S8）的纯逻辑：代理模式、端口输入、测试请求、行目录与站点凭据过滤。无 UI / 无主机依赖，便于单测。

// MARK: - 代理模式

/// `proxy_mode` 的取值（顺序即选择列表顺序，同 GPUI / Web）。
nonisolated enum NetworkProxyMode: String, CaseIterable, Identifiable, Sendable {
    case none, system, manual, auto

    var id: String { rawValue }

    /// 主机值 → 模式；缺省 / 未知值按「不使用代理」（同 Web `ProxySettings`）。
    init(wire: String?) {
        self = wire.flatMap(NetworkProxyMode.init(rawValue:)) ?? .none
    }

    var titleKey: String {
        switch self {
        case .none: "proxyModeNone"
        case .system: "proxyModeSystem"
        case .manual: "proxyModeManual"
        case .auto: "proxyModeAuto"
        }
    }
}

// MARK: - 端口输入

/// `proxy_port` 的文本规则：只保留 ASCII 数字，提交时钳位到 1…65535；空 = 未配置。
enum NetworkPortInput {
    static let range = 1 ... 65535

    /// 过滤掉非数字字符（粘贴 `:8080`、全角数字等）。
    static func digits(_ text: String) -> String {
        text.filter { $0.isASCII && $0.isNumber }
    }

    /// 提交值：`wire` 为写入的字符串（空串 = 清空），`adjusted` 为被钳位改写后的数值（未越界 / 清空为 nil）。
    /// 超出 `Int` 的数字饱和后再钳位，而不是当作非法输入。
    static func commit(_ text: String) -> (wire: String, adjusted: Int?) {
        let filtered = digits(text)
        guard !filtered.isEmpty else { return ("", nil) }
        let parsed = SettingsNumber.parse(filtered) ?? range.upperBound
        let result = SettingsNumber.clamp(parsed, to: range)
        return (String(result.value), result.adjusted ? result.value : nil)
    }
}

// MARK: - 连通性测试请求

enum NetworkProxyTest {
    /// 手动分组的测试：任何模式下都发送表单里的 `proxy_*` 五个键。
    static func manual(form: SettingsConfigForm) -> ProxyTestRequest {
        ProxyTestRequest(
            proxyType: form.string("proxy_type", default: "http"),
            host: form.string("proxy_host"),
            port: form.string("proxy_port"),
            username: form.string("proxy_username"),
            password: form.string("proxy_password")
        )
    }

    /// 系统分组的测试：已检测到的系统代理（类型 / 地址 / 端口，无凭据）；未检测到返回 nil。
    static func system(dto: SystemProxyDto?) -> ProxyTestRequest? {
        guard let dto, dto.detected else { return nil }
        return ProxyTestRequest(proxyType: dto.proxyType, host: dto.host, port: String(dto.port))
    }

    /// 失败详情：代理端点不接受 TLS 握手（把 HTTP 代理选成了 HTTPS）给出可操作的提示；否则取主机错误文案。
    static func failureDetail(message: String, fallback: String, tlsHint: String) -> String {
        if message.range(of: "did not accept a TLS handshake", options: .caseInsensitive) != nil { return tlsHint }
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? fallback : trimmed
    }
}

// MARK: - 行目录

/// 本页的每一行（页面渲染与设置搜索共用同一份可见性判定）。
nonisolated enum NetworkRow: String, CaseIterable, Identifiable {
    case mode, type, host, port, username, password, noList, test
    case rules, rulesAdd, rulesTest
    case siteAuth, siteAuthAdd, siteAuthClear

    var id: String { "network.\(rawValue)" }

    /// 搜索面包屑里的分组名文案键。
    nonisolated enum Group {
        case mode, manual, rules, siteAuth

        var titleKey: String {
            switch self {
            case .mode: "proxySettings"
            case .manual: "proxyModeManual"
            case .rules: "proxyRulesTitle"
            case .siteAuth: "settingsSiteAuthTitle"
            }
        }
    }

    var group: Group {
        switch self {
        case .mode: .mode
        case .type, .host, .port, .username, .password, .noList, .test: .manual
        case .rules, .rulesAdd, .rulesTest: .rules
        case .siteAuth, .siteAuthAdd, .siteAuthClear: .siteAuth
        }
    }

    var titleKey: String {
        switch self {
        case .mode: "proxySettings"
        case .type: "proxyType"
        case .host: "proxyHost"
        case .port: "proxyPort"
        case .username: "proxyUsername"
        case .password: "proxyPassword"
        case .noList: "proxyNoList"
        case .test: "proxyTestConnection"
        case .rules: "proxyRulesTitle"
        case .rulesAdd: "proxyRulesAdd"
        case .rulesTest: "proxyRouteTest"
        case .siteAuth: "settingsSiteAuthTitle"
        case .siteAuthAdd: "settingsSiteAuthAdd"
        case .siteAuthClear: "settingsSiteAuthClearAll"
        }
    }

    var detailKey: String? {
        switch self {
        case .mode: "proxySettingsDesc"
        case .noList: "proxyNoListDesc"
        case .rules: "proxyRulesDesc"
        case .rulesTest: "proxyRouteTestDesc"
        case .siteAuth: "settingsSiteAuthDesc"
        default: nil
        }
    }

    /// 对应的 daemon 配置键（`proxy_*`；非配置行为 nil）。
    var configKey: String? {
        switch self {
        case .mode: "proxy_mode"
        case .type: "proxy_type"
        case .host: "proxy_host"
        case .port: "proxy_port"
        case .username: "proxy_username"
        case .password: "proxy_password"
        case .noList: "proxy_no_list"
        case .rules: "proxy_rules"
        case .test, .rulesAdd, .rulesTest, .siteAuth, .siteAuthAdd, .siteAuthClear: nil
        }
    }

    var symbol: String {
        switch self {
        case .rules: return "arrow.triangle.branch"
        case .rulesAdd: return "plus" // = FluxSymbol.add；纯逻辑文件不依赖 FluxUI
        case .rulesTest: return "magnifyingglass"
        default: break
        }
        switch group {
        case .mode, .manual, .rules: return "globe"
        case .siteAuth: return "key.fill"
        }
    }

    /// 配置键行 → 目录项（供 `ConfigSegmentedRow` / `ConfigPickerRow`）。
    var item: SettingsItem? {
        configKey.map { SettingsItem(id: id, key: $0, titleKey: titleKey, detailKey: detailKey) }
    }

    /// 手动配置分组只需主机配置已加载（各模式都会用到这些字段，切模式不隐藏）；
    /// 代理模式行与站点规则同样只需配置已加载；
    /// 站点凭据走 `daemon.siteAuth.*`（`daemon.*` 恒可用），与配置是否已加载无关。
    func isVisible(isLoaded: Bool) -> Bool {
        switch group {
        case .mode, .manual, .rules: isLoaded
        case .siteAuth: true
        }
    }

    static func visible(isLoaded: Bool) -> [NetworkRow] {
        allCases.filter { $0.isVisible(isLoaded: isLoaded) }
    }
}

// MARK: - 站点凭据

enum SiteAuthFilter {
    /// 凭据条数达到该值才显示搜索框。
    static let searchThreshold = 6

    static func showsSearch(count: Int) -> Bool { count >= searchThreshold }

    /// 站点 / 用户名的大小写、变音不敏感子串匹配；空查询返回全部。
    static func filter(_ entries: [SiteAuthEntryDto], query: String) -> [SiteAuthEntryDto] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return entries }
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive, .widthInsensitive]
        return entries.filter {
            $0.site.range(of: needle, options: options) != nil || $0.user.range(of: needle, options: options) != nil
        }
    }

    /// 表单可保存：站点与用户名均非空（密码允许为空——服务端按「无密码」保存）。
    static func canSave(site: String, user: String) -> Bool {
        !site.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !user.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
