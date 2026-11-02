package com.fluxdown.core.protocol

import java.util.Locale

/*
 * BT 防吸血（`daemon.bt.guard.*`）DTO 与纯逻辑。镜像 `native/protocol/src/daemon.rs` 的 BtGuard* 段与
 * Web `web/src/lib/rpc/protocol/` 下的 TS 定义（契约 bt-guard §4 / §8）。解码宽松（缺省取零值），编码字段名 = wire camelCase。
 *
 * 仅 UI 可调用的方法见 [BtGuardMethods]；`daemon.bt.guard.cloud.apply` / `evidence.drain` 只属 agent 内部，
 * 这里刻意不提供常量，原生桥 `HostSession.call` 也会拒绝。
 */

/** UI 可调用的防吸血方法（不含 agent 内部的 cloud.apply / evidence.drain）。 */
object BtGuardMethods {
    val all: List<String> = listOf(
        HostMethod.daemonBtGuardState,
        HostMethod.daemonBtGuardEntriesList,
        HostMethod.daemonBtGuardEntriesAdd,
        HostMethod.daemonBtGuardEntriesRemove,
        HostMethod.daemonBtGuardEventsList,
        HostMethod.daemonBtGuardExport,
        HostMethod.daemonBtGuardFilterSourcesRefresh,
    )
}

// ───────────────────────────── DTO ─────────────────────────────

data class BtGuardStats(
    val todayObserved: Long = 0,
    val todayEnforced: Long = 0,
    val todayBlockedConnections: Long = 0,
    val todayRefusedBytes: Long = 0,
) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardStats(
            todayObserved = v.long("todayObserved"),
            todayEnforced = v.long("todayEnforced"),
            todayBlockedConnections = v.long("todayBlockedConnections"),
            todayRefusedBytes = v.long("todayRefusedBytes"),
        )
    }
}

data class BtGuardCloudStatus(
    /** 已应用文档序号；0 = 无。 */
    val sequence: Long = 0,
    /** Unix 秒；0 = 无。 */
    val updatedAt: Long = 0,
    /** Unix 秒；0 = 无。 */
    val expiresAt: Long = 0,
    val entries: Long = 0,
    val clientRules: Long = 0,
    val stale: Boolean = false,
    val lastError: String? = null,
    val rejectedReason: String? = null,
    val attribution: List<String> = emptyList(),
) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardCloudStatus(
            sequence = v.long("sequence"),
            updatedAt = v.long("updatedAt"),
            expiresAt = v.long("expiresAt"),
            entries = v.long("entries"),
            clientRules = v.long("clientRules"),
            stale = v.bool("stale"),
            lastError = v.strOrNull("lastError")?.takeIf { it.isNotBlank() },
            rejectedReason = v.strOrNull("rejectedReason")?.takeIf { it.isNotBlank() },
            attribution = v.strings("attribution"),
        )
    }
}

data class BtGuardFilterSource(
    val url: String = "",
    val ok: Boolean = false,
    val entries: Long = 0,
    /** Unix 秒；0 = 从未成功。 */
    val updatedAt: Long = 0,
    val error: String? = null,
) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardFilterSource(
            url = v.str("url"),
            ok = v.bool("ok"),
            entries = v.long("entries"),
            updatedAt = v.long("updatedAt"),
            error = v.strOrNull("error")?.takeIf { it.isNotBlank() },
        )

        fun listFrom(v: JsonValue?, key: String): List<BtGuardFilterSource> = v.list(key).map { fromJson(it) }
    }
}

data class BtGuardCounts(
    val allow: Long = 0,
    val manual: Long = 0,
    val auto: Long = 0,
    val cloud: Long = 0,
    val file: Long = 0,
) {
    val total: Long get() = allow + manual + auto + cloud + file

    fun of(layer: BtGuardLayer): Long = when (layer) {
        BtGuardLayer.Allow -> allow
        BtGuardLayer.Manual -> manual
        BtGuardLayer.Auto -> auto
        BtGuardLayer.Cloud -> cloud
        BtGuardLayer.File -> file
    }

    companion object {
        fun fromJson(v: JsonValue?) = BtGuardCounts(
            allow = v.long("allow"),
            manual = v.long("manual"),
            auto = v.long("auto"),
            cloud = v.long("cloud"),
            file = v.long("file"),
        )
    }
}

/** `daemon.bt.guard.state` 结果。 */
data class BtGuardState(
    val enabled: Boolean = false,
    /** BT 会话存活（防吸血仅在会话内生效）。 */
    val sessionActive: Boolean = false,
    val stats: BtGuardStats = BtGuardStats(),
    val cloud: BtGuardCloudStatus = BtGuardCloudStatus(),
    val filterSources: List<BtGuardFilterSource> = emptyList(),
    val counts: BtGuardCounts = BtGuardCounts(),
) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardState(
            enabled = v.bool("enabled"),
            sessionActive = v.bool("sessionActive"),
            stats = BtGuardStats.fromJson(v["stats"]),
            cloud = BtGuardCloudStatus.fromJson(v["cloud"]),
            filterSources = BtGuardFilterSource.listFrom(v, "filterSources"),
            counts = BtGuardCounts.fromJson(v["counts"]),
        )
    }
}

/** 过滤条目所在层（优先级 allow > manual > auto / cloud / file）。 */
enum class BtGuardLayer(val wire: String) {
    Allow("allow"),
    Manual("manual"),
    Auto("auto"),
    Cloud("cloud"),
    File("file");

    companion object {
        fun fromWire(wire: String?): BtGuardLayer? = entries.firstOrNull { it.wire == wire }
    }
}

/** 条目动作。 */
enum class BtGuardAction(val wire: String) {
    Allow("allow"),
    Ban("ban"),
    Choke("choke"),
    Throttle("throttle");

    companion object {
        fun fromWire(wire: String?): BtGuardAction? = entries.firstOrNull { it.wire == wire }
    }
}

/** 一条过滤条目。 */
data class BtGuardEntry(
    val id: Long,
    /** 未知层按原文保留（前向兼容），[layerKind] 为 null。 */
    val layer: String,
    val cidr: String,
    val action: String,
    val rule: String = "",
    val reason: String = "",
    val evidence: JsonValue? = null,
    val source: String = "",
    val createdAt: Long = 0,
    /** Unix 秒；null = 永久。 */
    val expiresAt: Long? = null,
    /** 距到期剩余秒数；null = 永久。 */
    val remainingSecs: Long? = null,
) {
    val layerKind: BtGuardLayer? get() = BtGuardLayer.fromWire(layer)
    val actionKind: BtGuardAction? get() = BtGuardAction.fromWire(action)

    companion object {
        fun fromJson(v: JsonValue?) = BtGuardEntry(
            id = v.long("id"),
            layer = v.str("layer"),
            cidr = v.str("cidr"),
            action = v.str("action"),
            rule = v.str("rule"),
            reason = v.str("reason"),
            evidence = v["evidence"]?.takeIf { !it.isNull },
            source = v.str("source"),
            createdAt = v.long("createdAt"),
            expiresAt = v.longOrNull("expiresAt"),
            remainingSecs = v.longOrNull("remainingSecs"),
        )
    }
}

/** `daemon.bt.guard.entries.list` 结果。 */
data class BtGuardEntriesPage(val total: Long = 0, val items: List<BtGuardEntry> = emptyList()) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardEntriesPage(
            total = v.long("total"),
            items = v.list("items").map { BtGuardEntry.fromJson(it) },
        )
    }
}

/** 事件日志一行。 */
data class BtGuardEvent(
    val id: Long,
    val at: Long,
    /** `observe` / `enforce` / `ban` / `unban` / `probe` 等；未知值原样保留。 */
    val kind: String,
    val ipGroup: String,
    val infoHash: String = "",
    val rule: String = "",
    /** 规则当时的模式（`observe` / `enforce`）。 */
    val mode: String = "",
    val action: String = "",
    val detail: String = "",
) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardEvent(
            id = v.long("id"),
            at = v.long("at"),
            kind = v.str("kind"),
            ipGroup = v.str("ipGroup"),
            infoHash = v.str("infoHash"),
            rule = v.str("rule"),
            mode = v.str("mode"),
            action = v.str("action"),
            detail = v.str("detail"),
        )
    }
}

data class BtGuardEventsPage(val total: Long = 0, val items: List<BtGuardEvent> = emptyList()) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardEventsPage(
            total = v.long("total"),
            items = v.list("items").map { BtGuardEvent.fromJson(it) },
        )
    }
}

/** `daemon.bt.guard.export` 结果。 */
data class BtGuardExport(val text: String = "", val lines: Long = 0) {
    companion object {
        fun fromJson(v: JsonValue?) = BtGuardExport(text = v.str("text"), lines = v.long("lines"))
    }
}

// ───────────────────────────── 请求参数 ─────────────────────────────

/** 单页条数（daemon 上限 500，默认 100；移动端按 50 一页）。 */
const val BT_GUARD_PAGE_SIZE = 50

/** `daemon.bt.guard.entries.list` 参数。空白 [query] 不发送。 */
data class BtGuardEntriesQuery(
    val layer: BtGuardLayer? = null,
    val rule: String? = null,
    val query: String = "",
    val offset: Int = 0,
    val limit: Int = BT_GUARD_PAGE_SIZE,
) {
    fun toJson(): JsonValue = jsonObjectOmitNulls(
        "layer" to layer?.wire,
        "rule" to rule?.takeIf { it.isNotEmpty() },
        "query" to query.trim().takeIf { it.isNotEmpty() },
        "offset" to offset,
        "limit" to limit.coerceIn(1, 500),
    )

    fun nextPage(loaded: Int): BtGuardEntriesQuery = copy(offset = loaded)
}

/** `daemon.bt.guard.events.list` 参数。 */
data class BtGuardEventsQuery(
    val rule: String? = null,
    val kind: String? = null,
    val offset: Int = 0,
    val limit: Int = BT_GUARD_PAGE_SIZE,
) {
    fun toJson(): JsonValue = jsonObjectOmitNulls(
        "rule" to rule?.takeIf { it.isNotEmpty() },
        "kind" to kind?.takeIf { it.isNotEmpty() },
        "offset" to offset,
        "limit" to limit.coerceIn(1, 500),
    )
}

/** 手动封禁 / 加白的有效期选项（1 小时 / 1 天 / 7 天 / 永久）。 */
enum class BtGuardDuration(val seconds: Long?) {
    Hour(3_600),
    Day(86_400),
    Week(604_800),
    Permanent(null),
}

object BtGuardParams {
    /** `daemon.bt.guard.entries.add`：手动封禁 / 限速 / choke（`allow` 动作请用 [allowParams]）。 */
    fun addParams(cidr: String, action: BtGuardAction, duration: BtGuardDuration, reason: String = ""): JsonValue =
        jsonObjectOmitNulls(
            "cidr" to cidr.trim(),
            "action" to action.wire,
            "durationSecs" to duration.seconds,
            "reason" to reason.trim(),
        )

    /** 加白（永久，白名单层）。 */
    fun allowParams(cidr: String, reason: String = ""): JsonValue =
        addParams(cidr, BtGuardAction.Allow, BtGuardDuration.Permanent, reason)

    /** `daemon.bt.guard.entries.remove`。 */
    fun removeParams(ids: Collection<Long>): JsonValue = jsonObject("ids" to ids.toList())

    /** `daemon.bt.guard.export`（可按层导出，null = 全部）。 */
    fun exportParams(layer: BtGuardLayer?): JsonValue = jsonObjectOmitNulls("layer" to layer?.wire)
}

// ───────────────────────────── 纯逻辑 ─────────────────────────────

/** 规则 id 与其模式配置键。 */
object BtGuardRules {
    /** 条目 / 事件 / 证据共用的规则 id（`BT_GUARD_RULES`）。 */
    val ids: List<String> = listOf("excessive", "rerequest", "rewind", "lag", "fingerprint", "cloud")

    /** 可在设置里单独配置模式的检测规则（不含 `cloud`，其模式是 `bt_guard_cloud_mode`）。 */
    val detectionIds: List<String> = ids - "cloud"

    /** 条目的 `rule` 取值：检测规则 + 云层 + 手动 / 过滤文件条目（封禁名单的规则筛选用）。 */
    val entryRuleIds: List<String> = ids + listOf("manual", "file")

    /** 事件日志 `kind`（engine `bt_guard` 实际写入的值，筛选下拉按此顺序）。 */
    val eventKinds: List<String> = listOf(
        "observed", "enforced", "probe", "probe_cleared", "entry_added", "entry_removed",
        "cloud_applied", "cloud_rejected", "filter_applied",
    )

    fun configKey(rule: String): String? = when (rule) {
        "cloud" -> "bt_guard_cloud_mode"
        in detectionIds -> "bt_guard_rule_$rule"
        else -> null
    }

    /** 模式 wire 值；未知值按默认 `observe` 处理。 */
    fun normalizeMode(wire: String?): String = wire?.trim()?.takeIf { it in SettingsCatalog.btGuardModes } ?: "observe"
}

/** 条目的剩余时长（展示分档；由 app 层本地化）。 */
sealed interface BtGuardRemaining {
    data object Permanent : BtGuardRemaining

    data object Expired : BtGuardRemaining

    data class Minutes(val n: Long) : BtGuardRemaining

    data class Hours(val n: Long) : BtGuardRemaining

    data class Days(val n: Long) : BtGuardRemaining

    companion object {
        /**
         * 优先使用服务端折算的 `remainingSecs`；缺失但有 `expiresAt` 时按 [nowSecs] 推算。
         * 不足 1 分钟按 1 分钟、向上取整到整分 / 时 / 天（不足一天按小时，不足一小时按分钟）。
         */
        fun of(entry: BtGuardEntry, nowSecs: Long): BtGuardRemaining {
            val secs = entry.remainingSecs ?: entry.expiresAt?.let { it - nowSecs } ?: return Permanent
            return of(secs)
        }

        fun of(secs: Long): BtGuardRemaining = when {
            secs <= 0 -> Expired
            secs < 3_600 -> Minutes(ceilDiv(secs, 60))
            secs < 86_400 -> Hours(ceilDiv(secs, 3_600))
            else -> Days(ceilDiv(secs, 86_400))
        }

        private fun ceilDiv(a: Long, b: Long): Long = (a + b - 1) / b
    }
}

/** 条目列表的纯逻辑（可移除 / 加白判定、分页合并、批量选择）。 */
object BtGuardEntries {
    /** 云层条目只读（daemon 拒绝删除，UI 引导「加白」）。未知层同样不可删，避免误删。 */
    fun canRemove(entry: BtGuardEntry): Boolean = entry.layerKind != null && entry.layerKind != BtGuardLayer.Cloud

    /** 白名单层条目本身已是放行，不再提供「加白」。 */
    fun canAllow(entry: BtGuardEntry): Boolean = entry.layerKind != BtGuardLayer.Allow

    /** 批量解除：只保留可移除条目的 id（保持选择顺序）。 */
    fun removableIds(selected: Collection<BtGuardEntry>): List<Long> = selected.filter(::canRemove).map { it.id }

    /** 是否还有下一页。 */
    fun hasMore(loaded: Int, total: Long): Boolean = loaded < total

    /** 追加一页：按 id 去重（翻页期间条目变化可能重叠），保持服务端顺序。 */
    fun merge(existing: List<BtGuardEntry>, page: List<BtGuardEntry>): List<BtGuardEntry> {
        val seen = existing.mapTo(HashSet()) { it.id }
        return existing + page.filter { seen.add(it.id) }
    }

    /** 移除成功后本地剔除（服务端会再发 `btGuardChanged` 通知触发整页刷新）。 */
    fun without(existing: List<BtGuardEntry>, removed: Set<Long>): List<BtGuardEntry> =
        existing.filter { it.id !in removed }

    /** 每个层的筛选芯片：全部 + 各层（带数量）。 */
    fun layerChips(counts: BtGuardCounts): List<Pair<BtGuardLayer?, Long>> =
        listOf<Pair<BtGuardLayer?, Long>>(null to counts.total) + BtGuardLayer.entries.map { it to counts.of(it) }
}

/** 证据摘要字段（由 app 层本地化标签）。 */
enum class BtGuardEvidenceField(val wire: String) {
    Detail("detail"),
    TorrentSize("torrentSize"),
    DeliveredBytes("deliveredBytes"),
    DupBytes("dupBytes"),
    Ratio("ratioMilli"),
    Rerequests("rerequests"),
    Rewind("rewindPermille"),
    Strikes("strikes"),
    Category("category"),
    Url("url"),
    Entries("entries"),
}

/** 一条证据事实：数值类字段放 [number]，文本类放 [text]。 */
data class BtGuardEvidenceFact(val field: BtGuardEvidenceField, val number: Long? = null, val text: String? = null)

object BtGuardEvidence {
    /** 把条目 / 事件的证据 JSON 解析成有序事实列表；零值与缺失的数值字段（`deliveredBytes` 除外）跳过。 */
    fun facts(evidence: JsonValue?): List<BtGuardEvidenceFact> {
        if (evidence.objOrNull == null) return emptyList()
        return BtGuardEvidenceField.entries.mapNotNull { field ->
            when (field) {
                BtGuardEvidenceField.Detail, BtGuardEvidenceField.Category, BtGuardEvidenceField.Url ->
                    evidence.strOrNull(field.wire)?.takeIf { it.isNotBlank() }?.let { BtGuardEvidenceFact(field, text = it) }
                BtGuardEvidenceField.DeliveredBytes ->
                    evidence.longOrNull(field.wire)?.let { BtGuardEvidenceFact(field, number = it) }
                else -> evidence.longOrNull(field.wire)?.takeIf { it > 0 }?.let { BtGuardEvidenceFact(field, number = it) }
            }
        }
    }

    /** 事件日志的 `detail` 是 JSON 文本（与条目证据同形），无法解析时返回空。 */
    fun factsFromText(detail: String): List<BtGuardEvidenceFact> = facts(Json.parseOrNull(detail))

    /** `ratioMilli`（千分比）→ 倍数文本，如 3700 → `3.7`。 */
    fun ratioText(milli: Long): String = String.format(Locale.ROOT, "%.1f", milli / 1000.0)

    /** `rewindPermille`（千分比）→ 百分比文本，如 125 → `12.5`。 */
    fun permilleText(permille: Long): String = String.format(Locale.ROOT, "%.1f", permille / 10.0)
}

/** 云名单状态展示分档。 */
enum class BtGuardCloudPhase {
    /** 云名单开关关闭。 */
    Disabled,

    /** 尚未拿到任何文档。 */
    Waiting,

    /** 文档过期，云层停用。 */
    Stale,

    /** 正常生效。 */
    Active,
}

object BtGuardCloud {
    fun phase(cloudEnabled: Boolean, status: BtGuardCloudStatus): BtGuardCloudPhase = when {
        !cloudEnabled -> BtGuardCloudPhase.Disabled
        status.sequence == 0L -> BtGuardCloudPhase.Waiting
        status.stale -> BtGuardCloudPhase.Stale
        else -> BtGuardCloudPhase.Active
    }
}

/** 证据上报开关的前置条件（契约 §0：独立开关、默认关、显式同意、仅登录用户可上报）。 */
object BtGuardReport {
    sealed interface Toggle {
        /** 可以直接关闭 / 已经满足条件的开启。 */
        data object Allowed : Toggle

        /** 开启需先让用户确认同意文案。 */
        data object NeedsConsent : Toggle

        /** 开启需要登录。 */
        data object NeedsLogin : Toggle
    }

    /** 关闭永远允许；开启时先要求登录，再要求同意。 */
    fun toggle(turnOn: Boolean, loggedIn: Boolean): Toggle = when {
        !turnOn -> Toggle.Allowed
        !loggedIn -> Toggle.NeedsLogin
        else -> Toggle.NeedsConsent
    }

    /** 已开启但已退出登录：证据停止上报，页面给出提示。 */
    fun dormant(enabled: Boolean, loggedIn: Boolean): Boolean = enabled && !loggedIn
}

/** 手动添加条目的 IP / CIDR 校验（`1.2.3.4`、`1.2.3.0/24`、`2001:db8::`、`2001:db8::/64`）。 */
object BtGuardCidr {
    sealed interface Result {
        /** [normalized] = 去空白、小写后的文本（前缀保持用户输入）。 */
        data class Ok(val normalized: String, val v6: Boolean, val prefix: Int?) : Result

        data object Empty : Result

        data object Invalid : Result
    }

    fun parse(input: String): Result {
        val text = input.trim().lowercase(Locale.ROOT)
        if (text.isEmpty()) return Result.Empty
        val slash = text.indexOf('/')
        val host = if (slash >= 0) text.substring(0, slash) else text
        val prefixText = if (slash >= 0) text.substring(slash + 1) else null
        val v6 = ':' in host
        val valid = if (v6) isIpv6(host) else isIpv4(host)
        if (!valid) return Result.Invalid
        var prefix: Int? = null
        if (prefixText != null) {
            if (prefixText.isEmpty() || prefixText.length > 3 || !prefixText.all { it in '0'..'9' }) return Result.Invalid
            val n = prefixText.toInt()
            if (n > if (v6) 128 else 32) return Result.Invalid
            prefix = n
        }
        return Result.Ok(text, v6, prefix)
    }

    fun isValid(input: String): Boolean = parse(input) is Result.Ok

    private fun isIpv4(s: String): Boolean {
        val parts = s.split('.')
        if (parts.size != 4) return false
        return parts.all { p ->
            p.isNotEmpty() && p.length <= 3 && p.all { it in '0'..'9' } && p.toInt() <= 255 && (p.length == 1 || p[0] != '0')
        }
    }

    private fun isIpv6(s: String): Boolean {
        if (s.isEmpty() || s.count { it == ':' } < 2) return false
        val doubleAt = s.indexOf("::")
        if (doubleAt >= 0 && s.indexOf("::", doubleAt + 1) >= 0) return false
        if (":::" in s) return false
        // 末尾可带内嵌 IPv4（占两组）
        var groupsBudget = 8
        var body = s
        val lastColon = s.lastIndexOf(':')
        val tail = s.substring(lastColon + 1)
        if ('.' in tail) {
            if (!isIpv4(tail)) return false
            groupsBudget -= 2
            body = s.substring(0, lastColon + 1).let { if (it.endsWith("::")) it else it.dropLast(1) }
            if (body.isEmpty()) return false
        }
        if (doubleAt >= 0) {
            val head = body.substringBefore("::")
            val rest = body.substringAfter("::", "")
            val left = if (head.isEmpty()) emptyList() else head.split(':')
            val right = if (rest.isEmpty()) emptyList() else rest.split(':')
            if (!(left + right).all(::isGroup)) return false
            return left.size + right.size < groupsBudget
        }
        val groups = body.split(':')
        return groups.size == groupsBudget && groups.all(::isGroup)
    }

    private fun isGroup(g: String): Boolean = g.length in 1..4 && g.all { it in '0'..'9' || it in 'a'..'f' }
}

/** 展示用文本裁剪（原因 / 详情可能很长）：按字符边界截断，不拆代理对，超长补省略号。 */
object BtGuardText {
    fun clip(text: String, max: Int): String {
        if (max <= 0 || text.length <= max) return text
        var end = max
        if (text[end - 1].isHighSurrogate() && end < text.length && text[end].isLowSurrogate()) end--
        return text.substring(0, end).trimEnd() + "…"
    }

    /** 条目行副标题：层 · 动作 · 规则 · 原因（空项跳过，原因最多 [reasonMax] 个字符）。 */
    fun subtitle(parts: List<String>, reason: String, reasonMax: Int = 80): String =
        (parts + clip(reason.trim(), reasonMax)).filter { it.isNotBlank() }.joinToString(" · ")
}
