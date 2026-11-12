package com.fluxdown.core.capture

import com.fluxdown.core.format.UrlText
import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.protocol.CategoryRules
import com.fluxdown.core.protocol.CustomCategoryDto
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.protocol.jsonObjectOmitNulls
import com.fluxdown.core.protocol.objOrNull
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.protocol.stringOrNull
import com.fluxdown.core.store.HostState

/**
 * 外部唤起带来的一条下载请求：浏览器「外部下载器」（X / Via 等的 http(s) VIEW）、系统分享、`magnet:` / `ed2k://`
 * 链接、浏览器扩展的 `fluxdown://download` 协议。Cookie / 来源页 /
 * 请求头 / 建议文件名只属于 [url] 这一条链接。
 */
data class ExternalDownload(
    val url: String,
    val fileName: String = "",
    val cookies: String = "",
    val referrer: String = "",
    val headers: Map<String, String> = emptyMap(),
) {
    /**
     * 命中分类（且该分类配置了目录）的保存目录，规则同 agent `capture.rs::with_category_dir`：
     * 外部唤起不指定目录时按文件名 / 链接路径末段归类；无命中 / 未配置目录返回 null。
     */
    fun categorySaveDir(state: HostState): String? =
        CategoryRules.saveDirFor(CustomCategoryDto.fromPreference(state.preferences[CustomCategoryDto.PREFERENCE_KEY]), fileName, url)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
}

/**
 * Intent 载荷 → [ExternalDownload]。只接收原始字符串（Android `Intent` 的读取在 `:app`）；
 * 超长输入按同一组上限截断，
 * 避免卡死界面。
 */
object ExternalIntake {
    const val MAX_URL_LEN = 8192
    const val MAX_NAME_LEN = 512
    const val MAX_COOKIES_LEN = 65536
    const val MAX_REFERRER_LEN = 8192
    const val MAX_HEADERS = 60
    const val MAX_HEADER_KEY_LEN = 512
    const val MAX_HEADER_VALUE_LEN = 8192

    /** `headers` 是 JSON，截断会破坏结构，超限整体丢弃。 */
    const val MAX_HEADERS_JSON_LEN = 131072

    private const val PROTOCOL_PREFIX = "fluxdown://"

    /** 首个可下载链接：magnet / ed2k / http(s) / ftp（空白含 Unicode 空格）。 */
    private val urlPattern = Regex(
        """(magnet:\?[^\s\p{Z}\uFEFF]+|ed2k://[^\s\p{Z}\uFEFF]+|(?:https?|ftp)://[^\s\p{Z}\uFEFF]+)""",
        RegexOption.IGNORE_CASE,
    )

    /** 分享文本（`EXTRA_TEXT` / ClipData）：提取其中首个链接（允许夹带描述文字）；无链接返回 null。 */
    fun fromSharedText(text: String?): ExternalDownload? = normalize(text, "", "", "", emptyMap())

    /**
     * VIEW 的 data：`fluxdown://download?url=…&filename=…&cookies=…&referrer=…&headers=<JSON>` 走协议解析；
     * 其余（http(s) 直链 / magnet / ed2k）原样作为链接，X 浏览器等经 extra 附带的 [userAgent] / [cookie] /
     * [referer] 随之带上；浏览器扩展的 Android Intent 另带 [fileName] 与完整请求头 [headersJson]
     * （[userAgent] 只在请求头里没有 User-Agent 时补上）。
     */
    fun fromView(
        data: String?,
        userAgent: String? = null,
        cookie: String? = null,
        referer: String? = null,
        fileName: String? = null,
        headersJson: String? = null,
    ): ExternalDownload? {
        val raw = data?.trim().orEmpty()
        if (raw.isEmpty()) return null
        if (raw.startsWith(PROTOCOL_PREFIX, ignoreCase = true)) return fromProtocol(raw)
        val headers = LinkedHashMap(decodeHeaders(headersJson))
        val ua = userAgent?.trim().orEmpty()
        if (ua.isNotEmpty() && headers.keys.none { it.equals("User-Agent", ignoreCase = true) } && headers.size < MAX_HEADERS) {
            headers["User-Agent"] = ua.take(MAX_HEADER_VALUE_LEN)
        }
        return normalize(raw, fileName.orEmpty(), cookie.orEmpty(), referer.orEmpty(), headers)
    }

    /** 从文本中提取首个可下载链接；无匹配返回 null。 */
    fun extractUrl(raw: String?): String? {
        val text = raw?.trim().orEmpty()
        if (text.isEmpty()) return null
        return urlPattern.find(text)?.value
    }

    /**
     * `headers` 参数（JSON 对象）→ 请求头：非对象 / 损坏为空；非字符串值与超长键跳过，值截断，
     * 至多 [MAX_HEADERS] 项（保持出现顺序）。
     */
    fun decodeHeaders(json: String?): Map<String, String> {
        if (json.isNullOrEmpty() || json.length > MAX_HEADERS_JSON_LEN) return emptyMap()
        val fields = Json.parseOrNull(json).objOrNull ?: return emptyMap()
        val out = LinkedHashMap<String, String>()
        for ((key, value) in fields) {
            if (out.size >= MAX_HEADERS) break
            val text = value.stringOrNull ?: continue
            if (key.length > MAX_HEADER_KEY_LEN) continue
            out[key] = text.take(MAX_HEADER_VALUE_LEN)
        }
        return out
    }

    /** `fluxdown://download?…`：主机必须是 `download`，`url` 必填。 */
    private fun fromProtocol(raw: String): ExternalDownload? {
        val rest = raw.substring(PROTOCOL_PREFIX.length)
        val authorityEnd = rest.indexOfFirst { it == '/' || it == '?' || it == '#' }.let { if (it < 0) rest.length else it }
        val host = rest.substring(0, authorityEnd).substringAfterLast('@').substringBefore(':')
        if (!host.equals("download", ignoreCase = true)) return null
        val query = rest.substring(authorityEnd).substringBefore('#').substringAfter('?', "")
        val params = UrlText.queryParameters(query)
        val url = params["url"]?.trim().orEmpty()
        if (url.isEmpty()) return null
        return normalize(
            text = url,
            fileName = params["filename"].orEmpty(),
            cookies = params["cookies"].orEmpty(),
            referrer = params["referrer"].orEmpty(),
            headers = decodeHeaders(params["headers"]),
        )
    }

    private fun normalize(
        text: String?,
        fileName: String,
        cookies: String,
        referrer: String,
        headers: Map<String, String>,
    ): ExternalDownload? {
        val url = extractUrl(text) ?: return null
        return ExternalDownload(
            url = url.take(MAX_URL_LEN),
            fileName = fileName.trim().take(MAX_NAME_LEN),
            cookies = cookies.take(MAX_COOKIES_LEN),
            referrer = referrer.trim().take(MAX_REFERRER_LEN),
            headers = headers,
        )
    }
}

/**
 * 「免打扰下载」（偏好 `download.silent_download`）下外部请求直接建任务的参数，规则同 agent
 * `capture.rs::ExternalPolicy`：保存目录 = 跟随上次保存位置 > 主机默认目录；免打扰自动分类开启（偏好
 * `download.silent_auto_category`，缺省开）时再把它作为 `offeredSaveDir` 交给 Rust 按分类规则路由，
 * 关闭时只用该目录。队列 / 分段取主机默认；[unattended] = 设备偏好
 * `download.silent_skip_selection`（跳过 BT 文件 / 画质 / 变体二次选择）。
 */
data class SilentCapture(val request: CreateTaskRequest) {
    val unattended: Boolean get() = request.unattended

    companion object {
        const val SILENT_PREF = "download.silent_download"
        const val SKIP_SELECTION_PREF = "download.silent_skip_selection"
        const val AUTO_CATEGORY_PREF = "download.silent_auto_category"
        private const val REMEMBER_LAST_SAVE_DIR_PREF = "download.remember_last_save_dir"
        private const val LAST_SAVE_DIR_PREF = "download.last_save_dir"

        /** 免打扰关闭，或算不出任何保存目录（应退回确认弹层）时返回 null。 */
        fun plan(download: ExternalDownload, state: HostState): SilentCapture? {
            val prefs = state.preferences
            if (!prefs.bool(SILENT_PREF, false)) return null
            val remembered = if (prefs.bool(REMEMBER_LAST_SAVE_DIR_PREF, false)) prefs.string(LAST_SAVE_DIR_PREF, "").trim() else ""
            val saveDir = remembered.ifEmpty { state.config["default_save_dir"].orEmpty().trim() }
            if (saveDir.isEmpty()) return null
            return SilentCapture(
                CreateTaskRequest(
                    url = download.url,
                    fileName = download.fileName,
                    saveDir = saveDir,
                    offeredSaveDir = saveDir.takeIf { prefs.bool(AUTO_CATEGORY_PREF, true) },
                    unattended = prefs.bool(SKIP_SELECTION_PREF, false),
                    segments = state.config["default_segments"]?.trim()?.toIntOrNull() ?: 0,
                    queueId = state.defaultQueueId(),
                    cookies = download.cookies,
                    referrer = download.referrer,
                    headers = download.headers,
                ),
            )
        }
    }
}
