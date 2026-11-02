package com.fluxdown.app.feature.devices

/** 默认监听端口：`fluxdown-agent --server` 缺省 `[::]:17800`（`native/agent/src/server_mode.rs::DEFAULT_BIND`）。 */
private const val DEFAULT_HTTP_PORT = 17800
private const val DEFAULT_HTTPS_PORT = 443

private val AllowedSchemes = setOf("http", "https", "ws", "wss")
private val SecureSchemes = setOf("https", "wss")

/** 地址解析结果。成功时 [Ok.endpoint] 恒为 `http(s)://host:port`（桥接层再规范到 `ws(s)://…/rpc`）。 */
internal sealed interface EndpointParse {
    /** [host] 为不含端口的主机（IPv6 带方括号）；[cleartext] = 明文 HTTP 且非回环。 */
    data class Ok(val endpoint: String, val host: String, val cleartext: Boolean) : EndpointParse

    data object Empty : EndpointParse

    data object BadAddress : EndpointParse

    data object BadPort : EndpointParse
}

/**
 * 解析用户输入的主机地址：`host`、`host:port`、`http(s)://host[:port][/…]`（`ws(s)://` 视同）。
 * 未写协议按 http；未写端口 http 用 17800、https 用 443（与 `agent.link.probe` 的地址规则一致）。
 * 路径 / 查询 / 片段被丢弃：`/rpc` 由桥接层补齐，不支持带前缀路径的反向代理。
 */
internal fun parseHostEndpoint(input: String): EndpointParse {
    val raw = input.trim()
    if (raw.isEmpty()) return EndpointParse.Empty

    val scheme = if ("://" in raw) raw.substringBefore("://").lowercase() else "http"
    if (scheme !in AllowedSchemes) return EndpointParse.BadAddress
    val secure = scheme in SecureSchemes
    val rest = if ("://" in raw) raw.substringAfter("://") else raw

    val authority = rest.substringBefore('/').substringBefore('?').substringBefore('#')
    if (authority.isEmpty() || '@' in authority || authority.any { it.isWhitespace() }) return EndpointParse.BadAddress

    val host: String
    val portText: String?
    if (authority.startsWith("[")) {
        val close = authority.indexOf(']')
        if (close <= 1) return EndpointParse.BadAddress
        host = authority.substring(0, close + 1)
        val tail = authority.substring(close + 1)
        portText = when {
            tail.isEmpty() -> null
            tail.startsWith(":") -> tail.substring(1)
            else -> return EndpointParse.BadAddress
        }
        if (!host.substring(1, host.length - 1).all { it.isHexDigit() || it == ':' || it == '.' }) return EndpointParse.BadAddress
    } else {
        val colons = authority.count { it == ':' }
        if (colons > 1) return EndpointParse.BadAddress
        if (colons == 1) {
            host = authority.substringBefore(':')
            portText = authority.substringAfter(':')
        } else {
            host = authority
            portText = null
        }
        if (!validHostName(host)) return EndpointParse.BadAddress
    }

    val port = when {
        portText == null -> if (secure) DEFAULT_HTTPS_PORT else DEFAULT_HTTP_PORT
        portText.isEmpty() || !portText.all { it in '0'..'9' } -> return EndpointParse.BadAddress
        else -> portText.toIntOrNull()?.takeIf { it in 1..65535 } ?: return EndpointParse.BadPort
    }

    val endpoint = "${if (secure) "https" else "http"}://$host:$port"
    return EndpointParse.Ok(endpoint, host, cleartext = !secure && !isLoopback(host))
}

private fun Char.isHexDigit(): Boolean = this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

/** RFC 952/1123 主机名或 IPv4 字面量：字母数字、`-`、`_`、`.`；不以点 / 连字符起止。 */
private fun validHostName(host: String): Boolean =
    host.isNotEmpty() &&
        host.length <= 253 &&
        host.all { it.isLetterOrDigit() && it.code < 128 || it == '-' || it == '_' || it == '.' } &&
        !host.startsWith('.') && !host.endsWith('.') && !host.startsWith('-')

private fun isLoopback(host: String): Boolean {
    val h = host.lowercase()
    return h == "localhost" || h == "[::1]" || h.startsWith("127.")
}

/** 访问密钥违规项；顺序与 `validate_access_key` 的检查顺序一致。 */
internal enum class KeyIssue { BadChars, TooShort, TooLong, NeedsMix }

internal const val ACCESS_KEY_MIN_LEN = 8
internal const val ACCESS_KEY_MAX_LEN = 128

/**
 * 访问密钥策略，逐条对齐 `native/agent/src/server_mode.rs::validate_access_key` 与
 * `web/src/lib/token-policy.ts`：可见 ASCII、8–128 位、同时含字母与数字。
 */
internal fun validateAccessKey(key: String): KeyIssue? = when {
    !key.all { it in '!'..'~' } -> KeyIssue.BadChars
    key.length < ACCESS_KEY_MIN_LEN -> KeyIssue.TooShort
    key.length > ACCESS_KEY_MAX_LEN -> KeyIssue.TooLong
    !key.any { it in 'a'..'z' || it in 'A'..'Z' } || !key.any { it in '0'..'9' } -> KeyIssue.NeedsMix
    else -> null
}
