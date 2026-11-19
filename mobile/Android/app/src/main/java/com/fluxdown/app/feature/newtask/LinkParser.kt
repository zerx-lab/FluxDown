package com.fluxdown.app.feature.newtask

import android.net.Uri
import android.provider.DocumentsContract
import com.fluxdown.core.model.TaskProtocol
import java.net.URI
import java.net.URLDecoder

/**
 * 「新建下载」的纯模型：链接解析与预设。规则与桌面端 `crates/downloads/src/model/new_download.rs`
 * 逐条对齐（aria2 风格：URL + 缩进的 `out=` / `checksum=` 选项行）。
 */

/** 一条解析出的下载条目。 */
internal data class UrlEntry(val url: String, val fileName: String = "", val checksum: String = "")

/**
 * 解析多行文本为下载条目。
 * - 以空格 / Tab 开头的行是选项行，附着到上一条（`out=` / `checksum=`）；
 * - `#` 开头为注释，空行跳过；
 * - 含 `magnet:?` / `ed2k://` 时从该位置截取到行尾；
 * - 其余行：[loose] = true（TXT 导入）取行内首个 `http(s)/ftp` 链接并去尾部标点，否则要求链接位于行首。
 */
internal fun parseEntries(text: String, loose: Boolean = false): List<UrlEntry> {
    val entries = ArrayList<UrlEntry>()
    var current: UrlEntry? = null
    for (line in text.split('\n')) {
        if (line.startsWith(' ') || line.startsWith('\t')) {
            val entry = current ?: continue
            val trimmed = line.trim()
            current = when {
                trimmed.startsWith("out=") -> entry.copy(fileName = trimmed.removePrefix("out="))
                trimmed.startsWith("checksum=") -> entry.copy(checksum = trimmed.removePrefix("checksum="))
                else -> entry
            }
            continue
        }
        val trimmed = line.trim()
        if (trimmed.isEmpty() || trimmed.startsWith('#')) continue
        current?.let { entries += it }
        val magnet = trimmed.indexOf("magnet:?", ignoreCase = true)
        val ed2k = trimmed.indexOf("ed2k://", ignoreCase = true)
        current = when {
            magnet >= 0 -> UrlEntry(trimmed.substring(magnet))
            ed2k >= 0 -> UrlEntry(trimmed.substring(ed2k))
            loose -> findHttpUrl(trimmed)?.trimEnd { it in ".,;:!?()[]{}" }?.takeIf { it.isNotEmpty() }?.let { UrlEntry(it) }
            else -> leadingHttpUrl(trimmed)?.let { UrlEntry(it) }
        }
    }
    current?.let { entries += it }
    return entries
}

private val Schemes = arrayOf("https://", "http://", "ftp://")

/** `^(https?|ftp)://\S+`（忽略大小写）。 */
private fun leadingHttpUrl(text: String): String? {
    val scheme = Schemes.firstOrNull { text.startsWith(it, ignoreCase = true) } ?: return null
    val rest = text.substring(scheme.length)
    val body = rest.indexOfFirst { it.isWhitespace() }.let { if (it < 0) rest.length else it }
    return if (body > 0) text.substring(0, scheme.length + body) else null
}

private fun findHttpUrl(line: String): String? {
    for (i in line.indices) {
        val scheme = Schemes.firstOrNull { line.regionMatches(i, it, 0, it.length, ignoreCase = true) } ?: continue
        val start = i + scheme.length
        var end = start
        while (end < line.length && !line[end].isWhitespace()) end++
        if (end > start) return line.substring(i, end)
    }
    return null
}

/** 按 URL 去重（保留首条）。 */
internal fun List<UrlEntry>.dedupe(): List<UrlEntry> {
    val seen = HashSet<String>()
    return filter { seen.add(it.url) }
}

/** 条目 → aria2 风格文本（含选项行）。 */
internal fun UrlEntry.toText(): String = buildString {
    append(url)
    if (fileName.isNotEmpty()) append("\n  out=").append(fileName)
    if (checksum.isNotEmpty()) append("\n  checksum=").append(checksum)
}

/** 把条目追加到现有文本末尾（按 URL 去重，已有文本逐字保留）。返回新文本与实际追加数。 */
internal fun appendEntries(existing: String, entries: List<UrlEntry>): Pair<String, Int> {
    val seen = parseEntries(existing).mapTo(HashSet()) { it.url }
    var text = existing.trimEnd()
    var added = 0
    for (e in entries) {
        if (!seen.add(e.url)) continue
        if (text.isNotEmpty()) text += "\n"
        text += e.toText()
        added++
    }
    return text to added
}

internal fun protocolOf(url: String): TaskProtocol = when {
    url.startsWith("magnet:", ignoreCase = true) || url.startsWith("torrent-file://") -> TaskProtocol.Bt
    url.startsWith("ed2k://", ignoreCase = true) -> TaskProtocol.Ed2k
    url.startsWith("ftp://", ignoreCase = true) || url.startsWith("ftps://", ignoreCase = true) -> TaskProtocol.Ftp
    url.substringBefore('?').endsWith(".m3u8", ignoreCase = true) -> TaskProtocol.Hls
    else -> TaskProtocol.Http
}

private fun decode(s: String): String = runCatching { URLDecoder.decode(s.replace("+", "%2B"), "UTF-8") }.getOrDefault(s)

internal fun UrlEntry.batchExportSize(): Long? {
    if (!url.startsWith("ed2k://|file|", ignoreCase = true)) return null
    val fields = url.split('|', limit = 6)
    if (fields.size < 5 || fields[2].isEmpty()) return null
    val hash = fields[4]
    if (hash.length != 32 || hash.any { it !in '0'..'9' && it !in 'a'..'f' && it !in 'A'..'F' }) return null
    val size = fields[3].removePrefix("+")
    if (size.any { it !in '0'..'9' }) return null
    return size.toLongOrNull()?.takeIf {
        it <= 0x40_0000_0000L && (it != 0L || hash.equals("31d6cfe0d16ae931b73c59d7e0c089c0", ignoreCase = true))
    }
}

internal fun UrlEntry.batchExportName(): String {
    if (fileName.isNotEmpty()) return fileName
    if (url.startsWith("magnet:?", ignoreCase = true)) return magnetName(url).orEmpty()
    if (url.startsWith("ed2k://", ignoreCase = true)) return ed2kName(url).orEmpty()
    if (!url.startsWith("http://", ignoreCase = true) &&
        !url.startsWith("https://", ignoreCase = true) &&
        !url.startsWith("ftp://", ignoreCase = true)
    ) return ""
    val schemeEnd = url.indexOf("://")
    // 字符串切分而非 java.net.URI：URL 含空格 / `|` / `{}` 时 URI 会抛异常，与 GPUI / Web / iOS 不一致。
    val rest = url.substring(schemeEnd + 3).substringBefore('#').substringBefore('?')
    val slash = rest.indexOf('/')
    if (slash < 0) return ""
    return decode(rest.substring(slash).substringAfterLast('/'))
}

/** 预览用的推断文件名：`out=` 优先；磁力取 `dn=`，eD2K 取 `|file|名称|`，其余取路径末段（取不到用主机名）。 */
internal fun inferName(entry: UrlEntry): String {
    if (entry.fileName.isNotBlank()) return entry.fileName
    val url = entry.url
    return when (protocolOf(url)) {
        TaskProtocol.Bt -> magnetName(url) ?: btihOf(url)?.let { "magnet:$it" } ?: url.take(40)
        TaskProtocol.Ed2k -> ed2kName(url) ?: url.take(40)
        else -> {
            val uri = runCatching { URI(url.substringBefore('#')) }.getOrNull()
            val seg = uri?.rawPath?.trimEnd('/')?.substringAfterLast('/')?.takeIf { it.isNotEmpty() }
            seg?.let(::decode) ?: uri?.host ?: url.take(40)
        }
    }
}

private fun magnetName(url: String): String? =
    url.substringAfter('?', "").substringBefore('#').splitToSequence('&')
        .firstOrNull { it.startsWith("dn=") }?.removePrefix("dn=")?.let(::decode)?.takeIf { it.isNotBlank() }

private fun ed2kName(url: String): String? =
    url.takeIf { it.startsWith("ed2k://|file|", ignoreCase = true) }
        ?.split('|', limit = 4)?.getOrNull(2)?.let(::decode)?.takeIf { it.isNotBlank() }

private fun btihOf(url: String): String? =
    Regex("btih:([A-Za-z0-9]+)", RegexOption.IGNORE_CASE).find(url)?.groupValues?.get(1)?.take(12)

/** 站点 / 来源描述：http(s) / ftp 取 host；磁力 / eD2K 为协议名。 */
internal fun hostOrNull(url: String): String? =
    if (protocolOf(url) == TaskProtocol.Bt || protocolOf(url) == TaskProtocol.Ed2k) null
    else runCatching { URI(url).host }.getOrNull()?.removePrefix("www.")

// ── 预设 ────────────────────────────────────────────────────────────────

internal val ThreadPresets = listOf(4, 8, 16, 32, 64)
internal const val MaxThreads = 256

/** 预设 UA（key → UA）。版本基准与 GPUI 桌面端 / Web 一致。 */
internal val UaPresets: List<Pair<String, String>> = listOf(
    "chrome" to "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
    "firefox" to "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0",
    "edge" to "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70",
    "safari" to "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15",
)
internal const val UaDefault = "default"
internal const val UaCustom = "custom"

internal fun detectUaPreset(ua: String): String =
    if (ua.isEmpty()) UaDefault else UaPresets.firstOrNull { it.second == ua }?.first ?: UaCustom

internal fun uaPresetValue(key: String): String = UaPresets.firstOrNull { it.first == key }?.second.orEmpty()

/** 哈希校验算法（wire 字面量，无本地化），默认 `sha-256`。 */
internal val HashAlgorithms = listOf("md5", "sha-1", "sha-256", "sha-512")
internal const val DefaultHashAlgorithm = "sha-256"

internal fun checksumSpec(algorithm: String, hash: String): String {
    val h = hash.trim()
    return if (h.isEmpty()) "" else "$algorithm=$h"
}

/** 任务代理选择；wire 语义同桌面端 `ProxyChoice`。 */
internal enum class ProxyChoice { Follow, Direct, System, GlobalManual, Custom }

private const val ProxyDirect = "direct://"
private const val ProxySystem = "system://"

internal fun ProxyChoice.wire(manualUrl: String, customUrl: String): String = when (this) {
    ProxyChoice.Follow -> ""
    ProxyChoice.Direct -> ProxyDirect
    ProxyChoice.System -> ProxySystem
    ProxyChoice.GlobalManual -> manualUrl
    ProxyChoice.Custom -> customUrl.trim()
}

/** 由主机配置拼全局手动代理 URL：`type://[user[:pass]@]host:port`；host 空或端口非法 = 未配置（空串）。 */
internal fun manualProxyUrl(config: Map<String, String>): String {
    fun cfg(key: String) = config[key]?.trim().orEmpty()
    val host = cfg("proxy_host")
    val port = cfg("proxy_port").toIntOrNull() ?: 0
    if (host.isEmpty() || port <= 0) return ""
    val type = cfg("proxy_type").ifEmpty { "http" }
    val user = config["proxy_username"].orEmpty()
    val password = config["proxy_password"].orEmpty()
    return buildString {
        append(type).append("://")
        if (user.isNotEmpty()) {
            append(encodeComponent(user))
            if (password.isNotEmpty()) append(':').append(encodeComponent(password))
            append('@')
        }
        append(host).append(':').append(port)
    }
}

/** URI 组件编码：除 `A-Za-z0-9-_.!~*'()` 外全部按 UTF-8 百分号编码。 */
private fun encodeComponent(value: String): String {
    val sb = StringBuilder(value.length + 8)
    for (b in value.toByteArray(Charsets.UTF_8)) {
        val c = b.toInt() and 0xff
        if (c < 128 && (c.toChar().isLetterOrDigit() || c.toChar() in "-_.!~*'()")) {
            sb.append(c.toChar())
        } else {
            sb.append('%').append("0123456789ABCDEF"[c shr 4]).append("0123456789ABCDEF"[c and 15])
        }
    }
    return sb.toString()
}

/** 保存目录合法：留空（用主机默认）或绝对路径（posix `/…`、Windows `X:\…` / `\\…`）。 */
internal fun isValidSaveDir(dir: String): Boolean {
    val d = dir.trim()
    return d.isEmpty() || d.startsWith("/") || d.startsWith("\\\\") || Regex("^[A-Za-z]:[\\\\/]").containsMatchIn(d)
}
