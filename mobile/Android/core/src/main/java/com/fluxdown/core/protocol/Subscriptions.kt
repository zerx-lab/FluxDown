package com.fluxdown.core.protocol

/*
 * BT Tracker / eD2K 服务器 / Kad 节点订阅（`daemon.bt.trackerSubscription.refresh` /
 * `daemon.ed2k.serverSubscription.refresh` / `daemon.ed2k.nodesSubscription.refresh`）。
 * 镜像 `native/protocol/src/daemon.rs`（TrackerSubRefreshResponse / Ed2kServerSubRefreshResponse /
 * Ed2kNodesSubRefreshResponse）；三个方法都不带参数。
 * 列表型配置键的存储格式镜像 GPUI `sections/subscription.rs::ListFormat` 与 Web `listFormat.ts`。
 */

/** 订阅刷新结果的公共视图（BT / eD2K 两种响应共用同一套展示逻辑）。 */
data class SubscriptionRefreshOutcome(
    /** 至少一个订阅源拉取成功。 */
    val success: Boolean,
    /** 去重合并后的唯一条目数（Tracker / 服务器）。 */
    val count: Long,
    val okSources: Long,
    val totalSources: Long,
    /** 缓存更新时间（Unix 秒；本次未成功时沿用旧值）。 */
    val updatedAt: Long,
    /** 全部源失败时的错误摘要（成功时为空）。 */
    val error: String,
)

/** `daemon.bt.trackerSubscription.refresh` 结果。缺省宽松。 */
data class TrackerSubRefreshResponse(
    val success: Boolean = false,
    val trackerCount: Long = 0,
    val okSources: Long = 0,
    val totalSources: Long = 0,
    val updatedAt: Long = 0,
    val error: String = "",
) {
    val outcome: SubscriptionRefreshOutcome
        get() = SubscriptionRefreshOutcome(success, trackerCount, okSources, totalSources, updatedAt, error)

    companion object {
        fun fromJson(v: JsonValue): TrackerSubRefreshResponse = TrackerSubRefreshResponse(
            success = v.bool("success"),
            trackerCount = v.long("trackerCount"),
            okSources = v.long("okSources"),
            totalSources = v.long("totalSources"),
            updatedAt = v.long("updatedAt"),
            error = v.str("error"),
        )
    }
}

/** `daemon.ed2k.serverSubscription.refresh` 结果。缺省宽松。 */
data class Ed2kServerSubRefreshResponse(
    val success: Boolean = false,
    val serverCount: Long = 0,
    val okSources: Long = 0,
    val totalSources: Long = 0,
    val updatedAt: Long = 0,
    val error: String = "",
) {
    val outcome: SubscriptionRefreshOutcome
        get() = SubscriptionRefreshOutcome(success, serverCount, okSources, totalSources, updatedAt, error)

    companion object {
        fun fromJson(v: JsonValue): Ed2kServerSubRefreshResponse = Ed2kServerSubRefreshResponse(
            success = v.bool("success"),
            serverCount = v.long("serverCount"),
            okSources = v.long("okSources"),
            totalSources = v.long("totalSources"),
            updatedAt = v.long("updatedAt"),
            error = v.str("error"),
        )
    }
}

/** `daemon.ed2k.nodesSubscription.refresh` 结果。缺省宽松。 */
data class Ed2kNodesSubRefreshResponse(
    val success: Boolean = false,
    val nodeCount: Long = 0,
    val okSources: Long = 0,
    val totalSources: Long = 0,
    val updatedAt: Long = 0,
    val error: String = "",
) {
    val outcome: SubscriptionRefreshOutcome
        get() = SubscriptionRefreshOutcome(success, nodeCount, okSources, totalSources, updatedAt, error)

    companion object {
        fun fromJson(v: JsonValue): Ed2kNodesSubRefreshResponse = Ed2kNodesSubRefreshResponse(
            success = v.bool("success"),
            nodeCount = v.long("nodeCount"),
            okSources = v.long("okSources"),
            totalSources = v.long("totalSources"),
            updatedAt = v.long("updatedAt"),
            error = v.str("error"),
        )
    }
}

/** 列表型配置键的存储格式；编辑区一律每行一个条目。 */
enum class SubscriptionListFormat {
    /** 按行存储（Tracker、订阅地址；保留 `#` 注释等自由文本）。 */
    Lines,

    /** 逗号分隔的 `host:port`（`ed2k_server_list` / `ed2k_server_sub_cache`）；读取时同时容忍换行 / 空白分隔的旧值。 */
    Comma;

    /** 存储值中的非空条目（首尾去空白）。 */
    fun entries(stored: String): List<String> {
        val parts = when (this) {
            Lines -> stored.split('\n', '\r')
            Comma -> stored.split(',') .flatMap { piece -> piece.split(Regex("\\s")) }
        }
        return parts.map { it.trim { c -> c.isWhitespace() } }.filter { it.isNotEmpty() }
    }

    /** 存储值中的条目数。 */
    fun count(stored: String): Int = entries(stored).size

    /** 存储值 → 编辑区文本（每行一个）。 */
    fun toEditor(stored: String): String = when (this) {
        Lines -> stored
        Comma -> entries(stored).joinToString("\n")
    }

    /** 编辑区文本 → 存储值：`Lines` 仅去首尾空白；`Comma` 按条目去空白并忽略大小写去重（保留首次出现的写法）。 */
    fun toStored(text: String): String = when (this) {
        Lines -> text.trim { it.isWhitespace() }
        Comma -> {
            val seen = HashSet<String>()
            entries(text).filter { seen.add(it.lowercase()) }.joinToString(",")
        }
    }
}
