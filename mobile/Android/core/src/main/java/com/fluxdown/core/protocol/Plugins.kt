package com.fluxdown.core.protocol

import com.fluxdown.core.host.PluginDiagnostic
import java.net.URI
import java.net.URISyntaxException
import java.util.Base64
import java.util.Locale

// 插件（`daemon.plugin.*`）DTO 与纯逻辑：镜像 `native/protocol/src/daemon.rs` 的插件段
// （同 iOS `Plugins.swift` / `web/src/lib/rpc/protocol/plugin.ts`），校验 / 市场 / 登录挑战规则对齐
// `web/src/pages/settings/sections/extensions/logic.ts` 与 GPUI `crates/extensions`。
// 解码一律宽松（缺省字段 = serde `#[serde(default)]`），编码字段名 = wire camelCase。

// ───────────────────────────── DTO ─────────────────────────────

/** select 控件选项。 */
data class PluginSettingOption(val value: String, val label: String) {
    companion object {
        fun fromJson(v: JsonValue?): PluginSettingOption? {
            if (v.objOrNull == null) return null
            return PluginSettingOption(value = v.str("value"), label = v.str("label"))
        }
    }
}

/** 声明式设置项（`SettingFieldDto`）。 */
data class PluginSettingField(
    val key: String,
    val title: String = "",
    val description: String = "",
    /** wire 字段名 `type`：`string` / `number` / `boolean`。 */
    val settingType: String = "string",
    /** `text` / `password` / `textarea` / `select` / `toggle` / `number` / `folder`。 */
    val widget: String = "text",
    val options: List<PluginSettingOption> = emptyList(),
    /** wire 字段名 `default`。 */
    val defaultValue: String? = null,
    val required: Boolean = false,
    val min: Double? = null,
    val max: Double? = null,
    val pattern: String? = null,
    /** 非空时 UI 在字段旁渲染复制按钮（仅复制文本，绝不执行）。 */
    val helperScript: String? = null,
    val helperLabel: String? = null,
) {
    val displayTitle: String get() = title.ifEmpty { key }

    companion object {
        fun fromJson(v: JsonValue?): PluginSettingField? {
            val key = v.strOrNull("key") ?: return null
            return PluginSettingField(
                key = key,
                title = v.str("title"),
                description = v.str("description"),
                settingType = v.str("type", "string"),
                widget = v.str("widget", "text"),
                options = v.list("options").mapNotNull { PluginSettingOption.fromJson(it) },
                defaultValue = v.strOrNull("default"),
                required = v.bool("required"),
                min = v["min"].doubleOrNull,
                max = v["max"].doubleOrNull,
                pattern = v.strOrNull("pattern"),
                helperScript = v.strOrNull("helperScript"),
                helperLabel = v.strOrNull("helperLabel"),
            )
        }
    }
}

data class PluginResolverUrlConflict(val pattern: String, val otherIdentity: String, val otherPattern: String)

/** 已安装插件视图（`PluginDto`）。 */
data class PluginDto(
    val identity: String,
    val name: String,
    val version: String,
    val description: String = "",
    val homepage: String = "",
    val enabled: Boolean = true,
    val devMode: Boolean = false,
    /** Empty for public or local packages; "private" for the private store. */
    val source: String = "",
    /** `None` / `Manual` / `CircuitBreaker`。 */
    val disabledReason: String = "None",
    val settings: List<PluginSettingField> = emptyList(),
    /** 当前设置值（key → 字符串）。 */
    val settingsValues: Map<String, String> = emptyMap(),
    /** manifest 声明的能力权限（如 `["ffmpeg"]`）。 */
    val permissions: List<String> = emptyList(),
    /** 是否声明平台登录入口。 */
    val authSupported: Boolean = false,
    val subscriptionProviderIds: List<String> = emptyList(),
    /** `Loaded` / `Failed`；与 [enabled] 独立。 */
    val loadStatus: String = "Loaded",
    /** Legacy wire fallback; clients render localized [loadDetail] instead. */
    val loadError: String = "",
    val loadDetail: PluginDiagnostic? = null,
    val resolverUrlConflicts: List<PluginResolverUrlConflict> = emptyList(),
) {
    val loadFailed: Boolean get() = loadStatus == "Failed"

    companion object {
        fun fromJson(v: JsonValue?): PluginDto? {
            val identity = v.strOrNull("identity") ?: return null
            return PluginDto(
                identity = identity,
                name = v.str("name"),
                version = v.str("version"),
                description = v.str("description"),
                homepage = v.str("homepage"),
                enabled = v.bool("enabled"),
                devMode = v.bool("devMode"),
                source = v.str("source"),
                disabledReason = v.str("disabledReason", "None"),
                settings = v.list("settings").mapNotNull { PluginSettingField.fromJson(it) },
                settingsValues = v.stringMap("settingsValues"),
                permissions = v.strings("permissions"),
                authSupported = v.bool("authSupported"),
                subscriptionProviderIds = v.strings("subscriptionProviderIds"),
                loadStatus = v.str("loadStatus", "Loaded"),
                loadError = v.str("loadError"),
                loadDetail = v["loadDetail"].let { detail ->
                    detail.strOrNull("kind")?.let { kind ->
                        PluginDiagnostic(
                            kind = kind,
                            path = detail.strOrNull("path"),
                            line = detail.longOrNull("line")?.takeIf { it in 0L..UInt.MAX_VALUE.toLong() }?.toUInt(),
                            column = detail.longOrNull("column")?.takeIf { it in 0L..UInt.MAX_VALUE.toLong() }?.toUInt(),
                        )
                    }
                },
                resolverUrlConflicts = v.list("resolverUrlConflicts").mapNotNull { conflict ->
                    PluginResolverUrlConflict(
                        conflict.strOrNull("pattern") ?: return@mapNotNull null,
                        conflict.strOrNull("otherIdentity") ?: return@mapNotNull null,
                        conflict.strOrNull("otherPattern") ?: return@mapNotNull null,
                    )
                },
            )
        }

        /** `daemon.plugins` 分区 / `daemon.plugin.list` 结果（`Vec<PluginDto>`）。 */
        fun listFromJson(v: JsonValue?): List<PluginDto> = v.arrayOrNull.orEmpty().mapNotNull { fromJson(it) }
    }
}

/** 站点默认插件（`PluginSiteDefaultDto`）；[automatic] = 无人值守下载自动选定、尚未被用户确认。 */
data class PluginSiteDefault(
    val site: String,
    val identity: String,
    val candidates: List<String> = emptyList(),
    val automatic: Boolean = false,
) {
    companion object {
        fun fromJson(v: JsonValue?): PluginSiteDefault? {
            val site = v.strOrNull("site") ?: return null
            val identity = v.strOrNull("identity") ?: return null
            return PluginSiteDefault(site, identity, v.strings("candidates"), v.bool("automatic"))
        }

        /** `daemon.plugin.siteDefaults` 结果（`Vec<PluginSiteDefaultDto>`）。 */
        fun listFromJson(v: JsonValue?): List<PluginSiteDefault> = v.arrayOrNull.orEmpty().mapNotNull { fromJson(it) }
    }
}

/** `daemon.plugin.auth` 请求（`PluginAuthRequest`）；省略字段由主机按 `#[serde(default)]` 补空串，这里一律显式发送。 */
data class PluginAuthRequest(
    val identity: String,
    /** `begin` / `poll` / `cancel` / `logout` / `status`。 */
    val action: String,
    val site: String = "",
    val authRef: String = "",
    val sessionId: String = "",
    val input: String = "",
) {
    fun toJson(): JsonValue = jsonObject(
        "identity" to identity,
        "action" to action,
        "site" to site,
        "authRef" to authRef,
        "sessionId" to sessionId,
        "input" to input,
    )
}

/** `daemon.plugin.auth` 响应（`PluginAuthResponse`）。 */
data class PluginAuthResponse(
    /** `pending` / `success` / `error`。 */
    val status: String,
    val sessionId: String = "",
    /** 二维码文本、data URL 或其他挑战内容。 */
    val challenge: String? = null,
    val challengeType: String? = null,
    val message: String = "",
    val authRef: String? = null,
) {
    companion object {
        fun fromJson(v: JsonValue?): PluginAuthResponse? {
            val status = v.strOrNull("status") ?: return null
            return PluginAuthResponse(
                status = status,
                sessionId = v.str("sessionId"),
                challenge = v.strOrNull("challenge"),
                challengeType = v.strOrNull("challengeType"),
                message = v.str("message"),
                authRef = v.strOrNull("authRef"),
            )
        }
    }
}

/** 安装成功结果（`InstalledPlugin`）；[missingComponents] 为所需但未安装的基础组件（提醒式，不阻断）。 */
data class InstalledPlugin(val identity: String, val missingComponents: List<String> = emptyList()) {
    companion object {
        fun fromJson(v: JsonValue?): InstalledPlugin? {
            val identity = v.strOrNull("identity") ?: return null
            return InstalledPlugin(identity, v.strings("missingComponents"))
        }
    }
}

/** 市场索引条目（`MarketEntryDto`）。 */
data class MarketEntry(
    val pluginId: String,
    val version: String,
    val sequence: Long = 0,
    val contentHash: String = "",
    val minAppVersion: String = "",
    val name: String = "",
    val description: String = "",
    val author: String = "",
    val homepage: String = "",
    val mirrors: List<String> = emptyList(),
    val publishTime: String = "",
    /** `none` = 可安装；`deprecated` / `vulnerable` / `malicious` = 已被发布者撤回。 */
    val yanked: String = "none",
    val tags: List<String> = emptyList(),
    val permissions: List<String> = emptyList(),
) {
    val id: String get() = "$pluginId@$version"
    val displayName: String get() = name.ifEmpty { pluginId }

    /** 引擎只安装 `yanked == "none"` 的条目。 */
    val installable: Boolean get() = yanked == "none"

    companion object {
        fun fromJson(v: JsonValue?): MarketEntry? {
            val pluginId = v.strOrNull("pluginId") ?: return null
            val version = v.strOrNull("version") ?: return null
            return MarketEntry(
                pluginId = pluginId,
                version = version,
                sequence = v.long("sequence"),
                contentHash = v.str("contentHash"),
                minAppVersion = v.str("minAppVersion"),
                name = v.str("name"),
                description = v.str("description"),
                author = v.str("author"),
                homepage = v.str("homepage"),
                mirrors = v.strings("mirrors"),
                publishTime = v.str("publishTime"),
                yanked = v.str("yanked", ""),
                tags = v.strings("tags"),
                permissions = v.strings("permissions"),
            )
        }

        fun listFromJson(v: JsonValue?): List<MarketEntry> = v.arrayOrNull.orEmpty().mapNotNull { fromJson(it) }
    }
}

/** `WsServerMsg::PluginAutoDisabled`（[HostNotice.pluginAutoDisabled] 的载荷）。 */
data class PluginAutoDisabledNotice(val identity: String, val reason: String = "") {
    companion object {
        fun fromJson(v: JsonValue?): PluginAutoDisabledNotice? {
            val identity = v.strOrNull("identity") ?: return null
            return PluginAutoDisabledNotice(identity, v.str("reason"))
        }
    }
}

// ───────────────────────────── 参数（wire 形状） ─────────────────────────────

/** `daemon.plugin.{uninstall,reloadDev}`。 */
fun pluginIdentityParams(identity: String): JsonValue = jsonObject("identity" to identity)

fun pluginSetEnabledParams(identity: String, enabled: Boolean): JsonValue =
    jsonObject("identity" to identity, "enabled" to enabled)

/** `daemon.plugin.updateSettings`：`entries` = 设置键 → 字符串值。 */
fun pluginUpdateSettingsParams(identity: String, entries: Map<String, String>): JsonValue =
    jsonObject("identity" to identity, "entries" to entries)

/** `daemon.plugin.install`：`blobId` 是经 `/api/web/blobs/plugins` 上传得到的一次性引用。 */
fun pluginInstallParams(blobId: String): JsonValue = jsonObject("blobId" to blobId)

/** `daemon.plugin.marketInstall`：[version] 为用户确认权限时看到的版本（最新可装版本不一致时主机拒绝 `marketVersionChanged`）；null 不发送。 */
fun pluginMarketInstallParams(pluginId: String, version: String?): JsonValue =
    jsonObjectOmitNulls("pluginId" to pluginId, "version" to version)

/** `daemon.plugin.ignoreRetry`（与 TaskIdParams 同形）。 */
fun pluginIgnoreRetryParams(taskId: String): JsonValue = jsonObject("taskId" to taskId)

/** `daemon.plugin.setSiteDefault`：[identity] = null 删除条目（下次下载重新询问）。 */
fun pluginSetSiteDefaultParams(site: String, identity: String?): JsonValue =
    jsonObject("site" to site, "identity" to identity)

// ───────────────────────────── 设置校验 ─────────────────────────────

/** 前置校验失败原因；文案映射留给 UI 层。 */
sealed interface PluginFieldError {
    data object Required : PluginFieldError
    data object Number : PluginFieldError
    data class Min(val min: String) : PluginFieldError
    data class Max(val max: String) : PluginFieldError
    data object Select : PluginFieldError

    /** 主机校验失败（如 `pattern` 不匹配）；文案已本地化。 */
    data class Server(val message: String) : PluginFieldError
}

/** 插件设置表单规则（GPUI `plugin_settings.rs` / Web `logic.ts` 一一对应）。 */
object PluginSettings {
    /** 单条前置校验：required → number / min / max → select 成员。`pattern` 由主机校验。 */
    fun validate(field: PluginSettingField, raw: String): PluginFieldError? {
        val value = raw.trim()
        if (field.required && value.isEmpty()) return PluginFieldError.Required
        if (value.isEmpty()) return null
        if (field.settingType == "number") {
            val number = parseDecimal(value)
            if (number == null || !number.isFinite()) return PluginFieldError.Number
            field.min?.let { if (number < it) return PluginFieldError.Min(formatNumber(it)) }
            field.max?.let { if (number > it) return PluginFieldError.Max(formatNumber(it)) }
        }
        if (field.widget == "select" && field.options.isNotEmpty() && field.options.none { it.value == value }) {
            return PluginFieldError.Select
        }
        return null
    }

    /** 校验整张表单，返回 key → 错误。 */
    fun validateAll(fields: List<PluginSettingField>, values: Map<String, String>): Map<String, PluginFieldError> {
        val errors = LinkedHashMap<String, PluginFieldError>()
        for (field in fields) {
            validate(field, values[field.key].orEmpty())?.let { errors[field.key] = it }
        }
        return errors
    }

    /**
     * 实际发给引擎的值（GPUI `submit_value`）：引擎对数字按原串解析、对 select 要求成员、对 pattern 做
     * 正则匹配，因此数字去掉首尾空白，可选且留空的这三类字段不上报（引擎会拒绝空串）。
     */
    fun submitValue(field: PluginSettingField, raw: String): String? {
        val trimmed = raw.trim()
        if (field.settingType == "number") return trimmed.ifEmpty { null }
        val rejectsEmpty = field.pattern != null ||
            (field.widget == "select" && field.options.none { it.value.isEmpty() })
        if (trimmed.isEmpty() && !field.required && rejectsEmpty) return null
        return raw
    }

    /** `daemon.plugin.updateSettings` 的 `entries`。 */
    fun submitEntries(fields: List<PluginSettingField>, values: Map<String, String>): Map<String, String> {
        val entries = LinkedHashMap<String, String>()
        for (field in fields) {
            submitValue(field, values[field.key].orEmpty())?.let { entries[field.key] = it }
        }
        return entries
    }

    /** 初始值：已保存 > manifest 默认值 > toggle 的 `false`。 */
    fun initialValue(field: PluginSettingField, saved: Map<String, String>): String {
        saved[field.key]?.let { return it }
        field.defaultValue?.takeIf { it.isNotEmpty() }?.let { return it }
        return if (field.widget == "toggle") "false" else ""
    }

    fun initialValues(fields: List<PluginSettingField>, saved: Map<String, String>): Map<String, String> {
        val out = LinkedHashMap<String, String>()
        for (field in fields) out[field.key] = initialValue(field, saved)
        return out
    }

    /** 数字占位提示：`1 – 10` / `≥ 1` / `≤ 10`。 */
    fun rangeHint(field: PluginSettingField): String? {
        val min = field.min
        val max = field.max
        return when {
            min != null && max != null -> "${formatNumber(min)} – ${formatNumber(max)}"
            min != null -> "≥ ${formatNumber(min)}"
            max != null -> "≤ ${formatNumber(max)}"
            else -> null
        }
    }

    /** JS `String(number)` 的等价输出：整数不带小数点。 */
    fun formatNumber(value: Double): String = JsonValue.formatDouble(value)

    /** 十进制浮点（与 Rust `f64::from_str` 对常规输入的接受范围一致，不含 hex / inf / NaN）。 */
    internal fun parseDecimal(text: String): Double? {
        var index = 0
        val normalized = StringBuilder()
        if (index < text.length && (text[index] == '+' || text[index] == '-')) {
            normalized.append(text[index])
            index++
        }
        var intDigits = 0
        var fracDigits = 0
        val mantissa = StringBuilder()
        while (index < text.length && text[index] in '0'..'9') {
            mantissa.append(text[index])
            index++
            intDigits++
        }
        var sawDot = false
        if (index < text.length && text[index] == '.') {
            sawDot = true
            mantissa.append('.')
            index++
            while (index < text.length && text[index] in '0'..'9') {
                mantissa.append(text[index])
                index++
                fracDigits++
            }
        }
        if (intDigits + fracDigits == 0) return null
        if (intDigits == 0) mantissa.insert(0, '0')
        if (sawDot && fracDigits == 0) mantissa.append('0')
        normalized.append(mantissa)
        if (index < text.length) {
            if (text[index] != 'e' && text[index] != 'E') return null
            index++
            normalized.append('e')
            if (index < text.length && (text[index] == '+' || text[index] == '-')) {
                normalized.append(text[index])
                index++
            }
            var expDigits = 0
            while (index < text.length && text[index] in '0'..'9') {
                normalized.append(text[index])
                index++
                expDigits++
            }
            if (expDigits == 0 || index != text.length) return null
        }
        return normalized.toString().toDoubleOrNull()
    }
}

// ───────────────────────────── 市场 ─────────────────────────────

enum class MarketAction { Install, Update, Installed, Unavailable }

object PluginMarket {
    /** 市场列表每次展开的条数。 */
    const val PAGE_SIZE = 50

    /** 关键字过滤：名称 / id / 描述 / 作者 / 标签任一命中（大小写不敏感）。 */
    fun filter(entries: List<MarketEntry>, query: String): List<MarketEntry> {
        val needle = query.trim().lowercase(Locale.ROOT)
        if (needle.isEmpty()) return entries
        fun hit(value: String) = value.lowercase(Locale.ROOT).contains(needle)
        return entries.filter { e ->
            hit(e.name) || hit(e.pluginId) || hit(e.description) || hit(e.author) || e.tags.any(::hit)
        }
    }

    /**
     * 每个插件一条（`crates/extensions/src/market.rs::latest_per_plugin`）：最新可安装版本；
     * 全部撤回（下架）的插件不出现。顺序为各插件在索引中首次出现的位置。
     */
    fun latestPerPlugin(entries: List<MarketEntry>): List<MarketEntry> {
        val best = LinkedHashMap<String, MarketEntry>()
        for (entry in entries) {
            val current = best[entry.pluginId]
            if (current == null) {
                best[entry.pluginId] = entry
                continue
            }
            val a = entry.installable
            val b = current.installable
            val better = if (a != b) a else entry.sequence > current.sequence
            if (better) best[entry.pluginId] = entry
        }
        return best.values.filter { it.installable }
    }

    /** `MAJOR.MINOR.PATCH`（可带前导 `v`，忽略预发布 / 构建后缀）；无法解析为 null。 */
    internal fun parseSemver(version: String): IntArray? {
        var text = version.trim()
        if (text.startsWith("v")) text = text.substring(1)
        val core = text.takeWhile { it != '-' && it != '+' }
        val parts = core.split('.')
        if (parts.size != 3) return null
        val numbers = IntArray(3)
        for ((i, part) in parts.withIndex()) {
            if (part.isEmpty() || !part.all { it in '0'..'9' }) return null
            numbers[i] = part.toIntOrNull() ?: return null
        }
        return numbers
    }

    /** [candidate] 严格新于 [current]；任一侧无法解析视为不可比较。 */
    fun versionNewer(candidate: String, current: String): Boolean {
        val a = parseSemver(candidate) ?: return false
        val b = parseSemver(current) ?: return false
        for (i in 0 until 3) if (a[i] != b[i]) return a[i] > b[i]
        return false
    }

    /** 市场条目相对本机安装状态的动作；开发模式插件不被市场覆盖。 */
    fun action(entry: MarketEntry, installed: PluginDto?): MarketAction {
        if (installed == null) return if (entry.installable) MarketAction.Install else MarketAction.Unavailable
        if (!installed.devMode && entry.installable && versionNewer(entry.version, installed.version)) return MarketAction.Update
        return MarketAction.Installed
    }

    /** 需要用户确认的权限：新装取全部权限，更新只取已安装版本没有的新增权限。 */
    fun permissionsToConfirm(entry: MarketEntry, installed: PluginDto?): List<String> {
        val granted = installed?.permissions.orEmpty()
        return entry.permissions.filter { it !in granted }
    }

    /** 已安装版本在市场中的撤回标记；未撤回 / 市场无此版本为 null。 */
    fun installedVersionYanked(entries: List<MarketEntry>, plugin: PluginDto): String? {
        if (plugin.devMode) return null
        val hit = entries.firstOrNull { it.pluginId == plugin.identity && it.version == plugin.version } ?: return null
        return hit.yanked.takeIf { it.isNotEmpty() && it != "none" }
    }

    /** 撤回标记 → i18n 键名；空 / 未知值不展示。 */
    fun yankedLabelKey(yanked: String): String? = when (yanked) {
        "deprecated" -> "marketYankedDeprecated"
        "vulnerable" -> "marketYankedVulnerable"
        "malicious" -> "marketYankedMalicious"
        else -> null
    }

    /** 权限 → i18n 键名（名称、说明）；未知权限为 null（调用侧回退原名 + 未知说明）。 */
    fun permissionKeys(permission: String): Pair<String, String>? = when (permission) {
        "ffmpeg" -> "pluginPermFfmpegName" to "pluginPermFfmpegDesc"
        "ytdlp" -> "pluginPermYtdlpName" to "pluginPermYtdlpDesc"
        "auth" -> "pluginPermAuthName" to "pluginPermAuthDesc"
        else -> null
    }
}

// ───────────────────────────── 登录挑战 ─────────────────────────────

/** 插件平台登录对话的本地状态。 */
data class PluginAuthState(
    val status: String = "",
    val sessionId: String = "",
    val authRef: String = "",
    val challenge: String? = null,
    val challengeType: String? = null,
    val message: String? = null,
) {
    /** 已登录：status 在不同 action 下语义不同（logout 的 success 是「注销成功」），故额外要求 authRef 非空。 */
    val loggedIn: Boolean get() = status == "success" && authRef.isNotEmpty()
    val sessionPending: Boolean get() = status == "pending" && sessionId.isNotEmpty()
    val isQrChallenge: Boolean get() = PluginAuth.isQrcode(challengeType)
}

object PluginAuth {
    /** 二维码轮询间隔（毫秒）。 */
    const val POLL_INTERVAL_MS = 2_000L

    /** 挑战文本超过该长度（按码点）即截断显示（复制始终给完整原文）。 */
    const val CHALLENGE_TEXT_LIMIT = 512

    /** `data:image/...;base64,` 挑战超过该长度直接放弃图片渲染，退化为文本 + 复制。 */
    const val MAX_CHALLENGE_DATA_URL_LENGTH = 256 * 1024

    /** QR Model 2 即使最低纠错级别也最多容纳 7089 个数字；更大的不可信载荷不交给编码器。 */
    const val MAX_QR_TEXT_LENGTH = 7089

    private val imageMimes = setOf(
        "image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/svg+xml",
    )

    fun isQrcode(type: String?): Boolean = type?.lowercase(Locale.ROOT) == "qrcode"

    /** pending 的 poll 可省略挑战（沿用旧值）；终态不沿用旧挑战；logout 始终清空本地登录态。 */
    fun apply(previous: PluginAuthState, response: PluginAuthResponse, wasLogout: Boolean): PluginAuthState {
        val pending = response.status == "pending"
        val next = PluginAuthState(
            status = response.status,
            sessionId = response.sessionId,
            authRef = response.authRef.orEmpty(),
            challenge = if (pending) response.challenge ?: previous.challenge else response.challenge,
            challengeType = if (pending) response.challengeType ?: previous.challengeType else response.challengeType,
            message = response.message.ifEmpty { null },
        )
        if (!wasLogout) return next
        return next.copy(authRef = "", sessionId = "", challenge = null, challengeType = null)
    }

    /**
     * 挑战不可信：只有形如 `data:image/<mime>[;..];base64,<payload>`、mime 已知、载荷是合法 base64 且
     * 长度未超限的值才当图片；返回解码后的图片字节，其余为 null（调用侧退化为截断文本 + 复制）。
     */
    fun dataImageBytes(value: String): ByteArray? {
        if (value.length > MAX_CHALLENGE_DATA_URL_LENGTH) return null
        if (!value.lowercase(Locale.ROOT).startsWith("data:")) return null
        val comma = value.indexOf(',')
        if (comma < 0) return null
        val header = value.substring(5, comma)
        val pieces = header.split(';').map { it.lowercase(Locale.ROOT) }
        val mime = pieces.first()
        if (mime !in imageMimes || "base64" !in pieces.drop(1)) return null
        val payload = value.substring(comma + 1).filter { !it.isWhitespace() }
        if (payload.isEmpty() || payload.length % 4 == 1) return null
        if (!payload.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '+' || it == '/' || it == '=' }) return null
        val padded = if (payload.length % 4 == 0) payload else payload + "=".repeat(4 - payload.length % 4)
        return try {
            Base64.getDecoder().decode(padded)
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /** 挑战文本超过 [CHALLENGE_TEXT_LIMIT] 个码点时截断并追加省略号。 */
    fun truncate(value: String): String {
        if (value.codePointCount(0, value.length) <= CHALLENGE_TEXT_LIMIT) return value
        return value.substring(0, value.offsetByCodePoints(0, CHALLENGE_TEXT_LIMIT)) + "…"
    }

    /** 仅 http(s) 链接可点击（挑战 / 主页均不可信，拒绝 `javascript:` 等）；返回原串。 */
    fun safeHttpUrl(value: String): String? {
        val uri = try {
            URI(value)
        } catch (_: URISyntaxException) {
            return null
        }
        val scheme = uri.scheme?.lowercase(Locale.ROOT)
        if (scheme != "http" && scheme != "https") return null
        if (uri.host.isNullOrEmpty()) return null
        return value
    }
}

// ───────────────────────────── 详情 ─────────────────────────────

/** 详情页数据：已安装插件与市场条目映射到同一形状。 */
data class PluginDetail(
    val name: String,
    val version: String,
    val identity: String,
    val description: String,
    val homepage: String,
    val author: String,
    val tags: List<String>,
    val publishTime: String,
    val minAppVersion: String,
    val settingsCount: Int,
    val permissions: List<String>,
    val yanked: String,
) {
    companion object {
        fun of(plugin: PluginDto) = PluginDetail(
            name = plugin.name,
            version = plugin.version,
            identity = plugin.identity,
            description = plugin.description,
            homepage = plugin.homepage,
            author = "",
            tags = emptyList(),
            publishTime = "",
            minAppVersion = "",
            settingsCount = plugin.settings.size,
            permissions = plugin.permissions,
            yanked = "",
        )

        fun of(entry: MarketEntry) = PluginDetail(
            name = entry.displayName,
            version = entry.version,
            identity = entry.pluginId,
            description = entry.description,
            homepage = entry.homepage,
            author = entry.author,
            tags = entry.tags,
            publishTime = entry.publishTime,
            minAppVersion = entry.minAppVersion,
            settingsCount = 0,
            permissions = entry.permissions,
            yanked = entry.yanked,
        )
    }
}

// ───────────────────────────── 插件包上传（远端主机文件面） ─────────────────────────────

/** 远端 `--server` 主机的插件包上传：`POST {http 根}/api/web/blobs/plugins`（Bearer 访问密钥，原始字节）→ `blobId`。 */
object PluginPackage {
    /** 单个插件包大小上限（超限报 `pluginPackageTooLarge`）。 */
    const val MAX_BYTES = 10 * 1024 * 1024

    /** 上传端点路径。 */
    const val UPLOAD_PATH = "/api/web/blobs/plugins"

    /** 主机 HTTP 根上传地址：`http(s)://host:port/api/web/blobs/plugins`（`ws(s)` 视同，路径 / 查询丢弃）；无法解析为 null。 */
    fun uploadUrl(endpoint: String): String? {
        val uri = try {
            URI(endpoint.trim())
        } catch (_: URISyntaxException) {
            return null
        }
        val scheme = when (uri.scheme?.lowercase(Locale.ROOT)) {
            "ws", "http" -> "http"
            "wss", "https" -> "https"
            else -> return null
        }
        val host = uri.host?.takeIf { it.isNotEmpty() } ?: return null
        val port = if (uri.port >= 0) ":${uri.port}" else ""
        return "$scheme://$host$port$UPLOAD_PATH"
    }
}
