package com.fluxdown.core.protocol

import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus

/*
 * `daemon.rss.*` 订阅编辑所需的 wire 类型（镜像 `native/protocol/src/daemon.rs` 的 `RssSourceDto` /
 * `RssValidate*`，同 iOS `FluxDomain/Protocol/Rss.swift`）。快照里的 `RssSource` 只是 UI 子集；
 * `daemon.rss.updateSource` 需要**完整**订阅，因此编辑前先 `listSources` 取最新副本、只改表单字段再整体写回。
 */

/** 完整订阅（`RssSourceDto`）；未列出的键（含只读运行态）原样保留在 [extra] 中写回。 */
data class RssSourceDetail(
    val sourceId: String = "",
    val url: String,
    val name: String = "",
    val enabled: Boolean = true,
    val autoDownload: Boolean = true,
    val startPaused: Boolean = false,
    /** 空 = 内置主队列。 */
    val queueId: String = "",
    /** 空 = 队列目录 → 全局目录。 */
    val saveDir: String = "",
    /** 0 = 引擎默认 30。 */
    val intervalMinutes: Int = 0,
    val includePattern: String = "",
    val excludePattern: String = "",
    val useRegex: Boolean = false,
    val smartEpisode: Boolean = false,
    /** 字节，0 = 不限。 */
    val sizeMinBytes: Long = 0,
    val sizeMaxBytes: Long = 0,
    val sendReferer: Boolean = true,
    val notifyOnDownload: Boolean = true,
    /** 1..100；0 = 引擎默认 20。 */
    val maxPerFetch: Int = 0,
    val cookies: String = "",
    val userAgent: String = "",
    val proxyUrl: String = "",
    /** 原始对象（providerId / providerConfig / 运行态等），写回时作为底稿。 */
    val extra: Map<String, JsonValue> = emptyMap(),
) {
    val effectiveIntervalMinutes: Int get() = if (intervalMinutes > 0) intervalMinutes else DEFAULT_INTERVAL_MINUTES
    val effectiveMaxPerFetch: Int get() = if (maxPerFetch > 0) maxPerFetch else DEFAULT_MAX_PER_FETCH

    fun toJson(): JsonValue.Obj {
        val out = LinkedHashMap(extra)
        if (!out.containsKey("providerId")) out["providerId"] = JsonValue.Str(BUILTIN_PROVIDER_ID)
        if (!out.containsKey("providerConfig")) out["providerConfig"] = JsonValue.Str("")
        out["sourceId"] = JsonValue.Str(sourceId)
        out["url"] = JsonValue.Str(url)
        out["name"] = JsonValue.Str(name)
        out["enabled"] = JsonValue.of(enabled)
        out["autoDownload"] = JsonValue.of(autoDownload)
        out["startPaused"] = JsonValue.of(startPaused)
        out["queueId"] = JsonValue.Str(queueId)
        out["saveDir"] = JsonValue.Str(saveDir)
        out["intervalMinutes"] = JsonValue.of(intervalMinutes)
        out["includePattern"] = JsonValue.Str(includePattern)
        out["excludePattern"] = JsonValue.Str(excludePattern)
        out["useRegex"] = JsonValue.of(useRegex)
        out["smartEpisode"] = JsonValue.of(smartEpisode)
        out["sizeMinBytes"] = JsonValue.of(sizeMinBytes)
        out["sizeMaxBytes"] = JsonValue.of(sizeMaxBytes)
        out["sendReferer"] = JsonValue.of(sendReferer)
        out["notifyOnDownload"] = JsonValue.of(notifyOnDownload)
        out["maxPerFetch"] = JsonValue.of(maxPerFetch)
        out["cookies"] = JsonValue.Str(cookies)
        out["userAgent"] = JsonValue.Str(userAgent)
        out["proxyUrl"] = JsonValue.Str(proxyUrl)
        return JsonValue.Obj(out)
    }

    companion object {
        const val BUILTIN_PROVIDER_ID = "rss"
        const val DEFAULT_INTERVAL_MINUTES = 30
        const val DEFAULT_MAX_PER_FETCH = 20
        val MAX_PER_FETCH_RANGE = 1..100

        /** 全字段 `#[serde(default)]`（`url` 必填）：缺 `url` → null。 */
        fun fromJson(v: JsonValue?): RssSourceDetail? {
            val url = v.strOrNull("url") ?: return null
            return RssSourceDetail(
                sourceId = v.str("sourceId"),
                url = url,
                name = v.str("name"),
                enabled = v.bool("enabled", true),
                autoDownload = v.bool("autoDownload", true),
                startPaused = v.bool("startPaused", false),
                queueId = v.str("queueId"),
                saveDir = v.str("saveDir"),
                intervalMinutes = v.int("intervalMinutes"),
                includePattern = v.str("includePattern"),
                excludePattern = v.str("excludePattern"),
                useRegex = v.bool("useRegex", false),
                smartEpisode = v.bool("smartEpisode", false),
                sizeMinBytes = v.long("sizeMinBytes"),
                sizeMaxBytes = v.long("sizeMaxBytes"),
                sendReferer = v.bool("sendReferer", true),
                notifyOnDownload = v.bool("notifyOnDownload", true),
                maxPerFetch = v.int("maxPerFetch"),
                cookies = v.str("cookies"),
                userAgent = v.str("userAgent"),
                proxyUrl = v.str("proxyUrl"),
                extra = v.objOrNull.orEmpty(),
            )
        }

        fun listFromJson(v: JsonValue?): List<RssSourceDetail> = v.arrayOrNull.orEmpty().mapNotNull { fromJson(it) }
    }
}

/** `daemon.rss.validate` 参数（只读、不落库；慢方法）。 */
data class RssValidateRequest(val url: String, val cookies: String = "", val userAgent: String = "", val proxyUrl: String = "") {
    fun toJson(): JsonValue = jsonObject("url" to url, "cookies" to cookies, "userAgent" to userAgent, "proxyUrl" to proxyUrl)
}

/** `daemon.rss.validate` 结果：[error] 非空即验证失败（诊断载荷，不是传输错误）。 */
data class RssValidateResponse(val feedTitle: String, val itemCount: Int, val error: String) {
    companion object {
        fun fromJson(v: JsonValue?) = RssValidateResponse(v.str("feedTitle"), v.list("items").size, v.str("error"))
    }
}

/**
 * `200M` / `2G` / `1.5 GB` / `1024`（1024 进制，可带小数与 `B` 后缀）↔ 字节数，同 iOS `RssSizeLiteral` /
 * Web `filter.ts::parseSize/formatSize`。过滤规则本身由主机引擎求值。
 */
object RssSizeLiteral {
    private val scales = mapOf('k' to 1024.0, 'm' to 1024.0 * 1024, 'g' to 1024.0 * 1024 * 1024, 't' to 1024.0 * 1024 * 1024 * 1024)
    private val decimal = Regex("[0-9]+(\\.[0-9]+)?")

    /** 空串 / 无法解析 / 越界 → null。 */
    fun parse(input: String): Long? {
        var text = input.trim().lowercase()
        if (text.isEmpty()) return null
        if (text.last() == 'b') text = text.dropLast(1)
        var multiplier = 1.0
        text.lastOrNull()?.let { last -> scales[last]?.let { multiplier = it; text = text.dropLast(1) } }
        val number = text.trim()
        if (!decimal.matches(number)) return null
        val bytes = (number.toDoubleOrNull() ?: return null) * multiplier
        val limit = 9_223_372_036_854_775_808.0
        if (!bytes.isFinite() || bytes < 0 || bytes > limit) return null
        return if (bytes >= limit) Long.MAX_VALUE else bytes.toLong()
    }

    /** 字节数 → 字面量（≤ 0 → 空串 = 不限）；与 [parse] 往返。 */
    fun format(bytes: Long): String {
        if (bytes <= 0) return ""
        for ((scale, suffix) in listOf((1L shl 40) to "T", (1L shl 30) to "G", (1L shl 20) to "M", (1L shl 10) to "K")) {
            if (bytes % scale == 0L) return "${bytes / scale}$suffix"
        }
        return bytes.toString()
    }

    /** 编辑器输入：空 = 不限（0）；非法为 null。 */
    fun field(text: String): Long? = if (text.isBlank()) 0 else parse(text)
}

/** 每轮上限输入：1..100 的整数，否则 null。 */
fun parseRssMaxPerFetch(text: String): Int? {
    val t = text.trim()
    if (t.isEmpty() || !t.all { it in '0'..'9' }) return null
    return t.toIntOrNull()?.takeIf { it in RssSourceDetail.MAX_PER_FETCH_RANGE }
}

// ───────────────────────────── 条目（R2） ─────────────────────────────

/** `RssItemDto.status`：0 新 / 1 已下载 / 2 已忽略 / 3 规则未命中 / 4 重复剧集 / 5 首轮历史条目；未知值归 [Unknown]。 */
enum class RssItemStatus(val wire: Int) {
    New(0), Downloaded(1), Ignored(2), Filtered(3), DuplicateEpisode(4), Seeded(5), Unknown(-1);

    companion object {
        fun fromWire(value: Int): RssItemStatus = entries.firstOrNull { it != Unknown && it.wire == value } ?: Unknown
    }
}

/**
 * 条目上的稳定原因码（引擎只产出码，文案由客户端本地化）。
 * [SeedSkipped]、[None] 与 [Unknown] 没有对应文案（[hasText] = false）。
 */
enum class RssReason(val wire: String) {
    NotIncluded("not_included"), Excluded("excluded"), TooSmall("too_small"), TooLarge("too_large"),
    DupEpisode("dup_episode"), TorrentFetchFailed("torrent_fetch_failed"),
    SeedSkipped("seed_skipped"), None(""), Unknown("");

    val hasText: Boolean get() = this != SeedSkipped && this != None && this != Unknown

    companion object {
        fun fromWire(value: String): RssReason = entries.firstOrNull { it != Unknown && it.wire == value } ?: Unknown
    }
}

/** 订阅流中的一个条目（`RssItemDto`；全字段缺省安全，`guid` 必填）。 */
data class RssItemDto(
    val sourceId: String = "",
    /** 去重主键。 */
    val guid: String,
    val title: String = "",
    val link: String = "",
    /** enclosure 直链（空 = 回退 [link]）。 */
    val enclosureUrl: String = "",
    /** enclosure 声明大小（字节，0 = 未知）。 */
    val enclosureLength: Long = 0,
    /** 发布时间（Unix 秒，0 = 未知）。 */
    val pubDate: Long = 0,
    val fetchedAt: Long = 0,
    val status: Int = 0,
    /** [status] = 已下载时回链的任务 ID。 */
    val taskId: String = "",
    /** 智能剧集归一键（空 = 未识别）。 */
    val episodeKey: String = "",
    val reason: String = "",
    val imageCount: Int = 0,
) {
    val state: RssItemStatus get() = RssItemStatus.fromWire(status)
    val reasonCode: RssReason get() = RssReason.fromWire(reason)

    /** 打开 / 复制用的链接：enclosure 优先，空则回退 [link]。 */
    val effectiveLink: String get() = enclosureUrl.ifEmpty { link }

    companion object {
        fun fromJson(v: JsonValue?): RssItemDto? {
            val guid = v.strOrNull("guid") ?: return null
            return RssItemDto(
                sourceId = v.str("sourceId"),
                guid = guid,
                title = v.str("title"),
                link = v.str("link"),
                enclosureUrl = v.str("enclosureUrl"),
                enclosureLength = v.long("enclosureLength"),
                pubDate = v.long("pubDate"),
                fetchedAt = v.long("fetchedAt"),
                status = v.int("status"),
                taskId = v.str("taskId"),
                episodeKey = v.str("episodeKey"),
                reason = v.str("reason"),
                imageCount = v.int("imageCount"),
            )
        }

        fun listFromJson(v: JsonValue?): List<RssItemDto> = v.arrayOrNull.orEmpty().mapNotNull { fromJson(it) }
    }
}

/** `daemon.rss.itemAction` 的动作：`download`（绕过规则强制下载，任何状态都允许重新下载）/ `ignore` / `readAll`。 */
enum class RssItemAction(val wire: String) { Download("download"), Ignore("ignore"), ReadAll("readAll") }

/** `daemon.rss.itemAction` 参数；`readAll` 忽略 guid（省略键）。 */
fun rssItemActionParams(sourceId: String, action: RssItemAction, guid: String? = null): JsonValue =
    jsonObjectOmitNulls("sourceId" to sourceId, "guid" to guid.takeIf { action != RssItemAction.ReadAll }, "action" to action.wire)

/** 一次性通知 `rssItemsChanged`：某订阅的条目流快照（新 → 旧）；[notifyTitles] = 本轮自动建任务的条目标题。 */
data class RssItemsChangedNotice(val sourceId: String, val items: List<RssItemDto>, val notifyTitles: List<String>) {
    companion object {
        fun fromJson(v: JsonValue?): RssItemsChangedNotice? {
            val sourceId = v.strOrNull("sourceId") ?: return null
            return RssItemsChangedNotice(sourceId, RssItemDto.listFromJson(v["items"]), v.strings("notifyTitles"))
        }
    }
}

/** 条目关联任务的实时状态（来自 `HostState.tasks`，按 `taskId` 联动）。 */
data class RssLinkedTask(val status: TaskStatus, val fileMissing: Boolean = false, val fraction: Double = 0.0) {
    constructor(task: Task) : this(
        task.status,
        task.fileMissing,
        if (task.totalBytes > 0) (task.downloadedBytes.toDouble() / task.totalBytes.toDouble()).coerceIn(0.0, 1.0) else 0.0,
    )
}

/** 条目状态 chip 的种类（决定文案 / 图标；形状 + 颜色双通道）。 */
enum class RssChipKind {
    TaskMissing, Pending, Downloading, Paused, Incomplete, Completed, Error, Preparing, TaskCreated,
    Ignored, Filtered, Duplicate, History, New,
}

enum class RssChipTone { Neutral, Accent, Success, Warning, Failure }

/** [progress] 非空 = 下载中（显示进度环）。 */
data class RssItemChip(val kind: RssChipKind, val tone: RssChipTone, val progress: Double? = null)

/** 条目流的纯逻辑（同 iOS `RssFormat` 条目部分）。 */
object RssItems {
    /** 状态 chip：已下载条目以关联任务的真实状态为准，任务已删除 = [RssChipKind.TaskMissing]。 */
    fun chip(item: RssItemDto, task: RssLinkedTask?): RssItemChip = when (item.state) {
        RssItemStatus.Downloaded -> when {
            item.taskId.isEmpty() || task == null -> RssItemChip(RssChipKind.TaskMissing, RssChipTone.Failure)
            else -> when (task.status) {
                TaskStatus.Pending -> RssItemChip(RssChipKind.Pending, RssChipTone.Neutral)
                TaskStatus.Downloading -> RssItemChip(RssChipKind.Downloading, RssChipTone.Accent, task.fraction)
                TaskStatus.Paused -> RssItemChip(RssChipKind.Paused, RssChipTone.Neutral)
                TaskStatus.Completed ->
                    if (task.fileMissing) RssItemChip(RssChipKind.Incomplete, RssChipTone.Warning)
                    else RssItemChip(RssChipKind.Completed, RssChipTone.Success)
                TaskStatus.Failed -> RssItemChip(RssChipKind.Error, RssChipTone.Failure)
                TaskStatus.Preparing -> RssItemChip(RssChipKind.Preparing, RssChipTone.Accent)
                TaskStatus.Unknown -> RssItemChip(RssChipKind.TaskCreated, RssChipTone.Neutral)
            }
        }
        RssItemStatus.Ignored -> RssItemChip(RssChipKind.Ignored, RssChipTone.Neutral)
        RssItemStatus.Filtered -> RssItemChip(RssChipKind.Filtered, RssChipTone.Neutral)
        RssItemStatus.DuplicateEpisode -> RssItemChip(RssChipKind.Duplicate, RssChipTone.Neutral)
        RssItemStatus.Seeded -> RssItemChip(RssChipKind.History, RssChipTone.Neutral)
        RssItemStatus.New, RssItemStatus.Unknown -> RssItemChip(RssChipKind.New, RssChipTone.Accent)
    }

    /**
     * 可见条目：标题包含（不区分大小写）→ 无发布时间的沉底 → 按发布时间排序 → 稳定回退到原顺序
     * （GPUI `visible_indices` / Web `visibleIndices` / iOS `RssFormat.visible`）。
     */
    fun visible(items: List<RssItemDto>, query: String, oldestFirst: Boolean): List<RssItemDto> {
        val needle = query.trim().lowercase()
        val matched = if (needle.isEmpty()) items else items.filter { it.title.lowercase().contains(needle) }
        // sortedWith 稳定：发布时间相同保持原顺序
        return matched.sortedWith { a, b ->
            val aMissing = a.pubDate <= 0
            val bMissing = b.pubDate <= 0
            when {
                aMissing != bMissing -> if (aMissing) 1 else -1
                aMissing || a.pubDate == b.pubDate -> 0
                oldestFirst -> a.pubDate.compareTo(b.pubDate)
                else -> b.pubDate.compareTo(a.pubDate)
            }
        }
    }
}
