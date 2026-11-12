package com.fluxdown.core.protocol

import java.text.Normalizer
import java.util.Locale

/* 网络与代理页的纯逻辑：代理模式、端口输入、测试请求、行目录与站点凭据过滤。无 UI / 无主机依赖，便于单测（同 iOS NetworkLogic）。 */

/** `proxy_mode` 的取值（顺序即选择列表顺序，同 GPUI / Web / iOS）。 */
enum class NetworkProxyMode(val wire: String) {
    None("none"), System("system"), Manual("manual"), Auto("auto");

    companion object {
        /** 主机值 → 模式；缺省 / 未知值按「不使用代理」（同 Web `ProxySettings`）。 */
        fun of(wire: String?): NetworkProxyMode = entries.firstOrNull { it.wire == wire } ?: None
    }
}

/** `proxy_port` 的文本规则：只保留 ASCII 数字，提交时钳位到 1…65535；空 = 未配置。 */
object NetworkPortInput {
    val range: IntRange = 1..65535

    /** 过滤掉非数字字符（粘贴 `:8080`、全角数字等）。 */
    fun digits(text: String): String = text.filter { it in '0'..'9' }

    /** 提交结果：[wire] 为写入的字符串（空串 = 清空），[adjusted] 为被钳位改写后的数值（未越界 / 清空为 null）。 */
    data class Commit(val wire: String, val adjusted: Int?)

    /** 超出 `Int` 的数字饱和后再钳位，而不是当作非法输入。 */
    fun commit(text: String): Commit {
        val filtered = digits(text)
        if (filtered.isEmpty()) return Commit("", null)
        val parsed = filtered.toLongOrNull() ?: Long.MAX_VALUE
        val clamped = parsed.coerceIn(range.first.toLong(), range.last.toLong())
        return Commit(clamped.toString(), if (clamped != parsed) clamped.toInt() else null)
    }
}

/** 连通性测试请求的组装与失败文案。 */
object NetworkProxyTest {
    /**
     * 连通性测试按来源组装（手动分组与系统分组各自一条，互不依赖当前 `proxy_mode`）：
     * - [manual]：表单里的 `proxy_*` 五个键。任何模式下手动分组都可测；
     * - [system]：已检测到的系统代理（类型 / 地址 / 端口，无凭据）；未检测到返回 null。
     */
    fun manual(form: SettingsForm): ProxyTestRequest = ProxyTestRequest(
        proxyType = form.string("proxy_type", "http"),
        host = form.string("proxy_host"),
        port = form.string("proxy_port"),
        username = form.string("proxy_username"),
        password = form.string("proxy_password"),
    )

    fun system(dto: SystemProxyDto?): ProxyTestRequest? =
        if (dto != null && dto.detected) ProxyTestRequest(dto.proxyType, dto.host, dto.port.toString()) else null

    /** 失败详情：代理端点不接受 TLS 握手（把 HTTP 代理选成了 HTTPS）给出可操作的提示；否则取主机错误文案。 */
    fun failureDetail(message: String, fallback: String, tlsHint: String): String {
        if (message.contains("did not accept a TLS handshake", ignoreCase = true)) return tlsHint
        val trimmed = message.trim()
        return trimmed.ifEmpty { fallback }
    }
}

/** 本页的每一行（页面渲染与设置搜索共用同一份可见性判定）。行 id 同 iOS（`network.<name>`）。 */
enum class NetworkRow(val rowId: String, val group: Group, val configKey: String?) {
    Mode("network.mode", Group.Mode, "proxy_mode"),
    Type("network.type", Group.Manual, "proxy_type"),
    Host("network.host", Group.Manual, "proxy_host"),
    Port("network.port", Group.Manual, "proxy_port"),
    Username("network.username", Group.Manual, "proxy_username"),
    Password("network.password", Group.Manual, "proxy_password"),
    NoList("network.noList", Group.Manual, "proxy_no_list"),
    Test("network.test", Group.Manual, null),
    BtProxyEnabled("network.btProxyEnabled", Group.BtProxy, "bt_proxy_enabled"),
    BtProxySource("network.btProxySource", Group.BtProxy, "bt_proxy_source"),
    BtProxyPeers("network.btProxyPeers", Group.BtProxy, "bt_proxy_peers"),
    Rules("network.rules", Group.Rules, "proxy_rules"),
    RulesAdd("network.rulesAdd", Group.Rules, null),
    RulesTest("network.rulesTest", Group.Rules, null),
    SiteAuth("network.siteAuth", Group.SiteAuth, null),
    SiteAuthAdd("network.siteAuthAdd", Group.SiteAuth, null),
    SiteAuthClear("network.siteAuthClear", Group.SiteAuth, null);

    /** 分组顺序即页面顺序（GPUI `sections/proxy.rs`）：BitTorrent 在手动代理之后、站点规则之前。 */
    enum class Group { Mode, Manual, BtProxy, Rules, SiteAuth }

    /**
     * 站点规则、手动配置与代理模式行只需配置已加载。手动字段在所有模式下都显示
     * （自动模式的备选路径、站点规则与任务级「手动代理」仍读这些字段），与当前模式无关。
     * BitTorrent 分组同样与代理模式无关（BT 有自己的代理来源）；「代理来源」与「节点连接也走代理」仅在 BT 走代理开启时显示。
     * 站点凭据走 `daemon.siteAuth.*`（`daemon.*` 恒可用），与配置是否已加载无关。
     */
    fun isVisible(form: SettingsForm): Boolean = when (group) {
        Group.Mode -> form.isLoaded
        Group.Manual -> form.isLoaded
        Group.BtProxy -> form.isLoaded && (this == BtProxyEnabled || form.bool(BtProxyEnabled.configKey!!))
        Group.Rules -> form.isLoaded
        Group.SiteAuth -> true
    }

    companion object {
        fun visible(form: SettingsForm): List<NetworkRow> =
            entries.filter { it.isVisible(form) }
    }
}

/**
 * BitTorrent 走代理时的提示（GPUI `proxy.rs::bt_proxy_hint`）：仅 `bt_proxy_enabled` 开启时显示其一。
 * 系统代理来源：说明检测时机与生效方式。手动代理来源：未配置服务器优先；其次 HTTPS（BT 不支持）、
 * HTTP / SOCKS4（无法转发 UDP）；SOCKS5 无提示。
 */
enum class BtProxyHint {
    SystemNote, NoServer, UnsupportedType, NoUdp;

    companion object {
        fun of(source: String, proxyType: String, host: String, port: String): BtProxyHint? {
            if (source == "system") return SystemNote
            if (host.isBlank() || port.isBlank()) return NoServer
            return when (proxyType) {
                "https" -> UnsupportedType
                "http", "socks4" -> NoUdp
                else -> null
            }
        }

        fun of(form: SettingsForm): BtProxyHint? {
            if (!form.bool(NetworkRow.BtProxyEnabled.configKey!!)) return null
            return of(
                form.string(NetworkRow.BtProxySource.configKey!!, "manual"),
                form.string("proxy_type", "http"),
                form.string("proxy_host"),
                form.string("proxy_port"),
            )
        }
    }
}

/** 站点凭据列表的过滤与表单校验。 */
object SiteAuthFilter {
    /** 凭据条数达到该值才显示搜索框。 */
    const val SEARCH_THRESHOLD = 6

    fun showsSearch(count: Int): Boolean = count >= SEARCH_THRESHOLD

    /** 站点 / 用户名的大小写、变音、全半角不敏感子串匹配；空查询返回全部。 */
    fun filter(entries: List<SiteAuthEntryDto>, query: String): List<SiteAuthEntryDto> {
        val needle = fold(query.trim())
        if (needle.isEmpty()) return entries
        return entries.filter { fold(it.site).contains(needle) || fold(it.user).contains(needle) }
    }

    /** 表单可保存：站点与用户名均非空（密码允许为空——服务端按「无密码」保存）。 */
    fun canSave(site: String, user: String): Boolean = site.isNotBlank() && user.isNotBlank()

    /** NFKD 分解（全角 → 半角、去变音符号）+ 小写。 */
    private fun fold(text: String): String =
        Normalizer.normalize(text, Normalizer.Form.NFKD).filter { Character.getType(it) != Character.NON_SPACING_MARK.toInt() }
            .lowercase(Locale.ROOT)
}
