package com.fluxdown.app.feature.downloads

import com.fluxdown.app.R
import com.fluxdown.app.data.GroupBy
import com.fluxdown.app.data.SortKey
import com.fluxdown.app.data.ViewPrefs
import com.fluxdown.app.feature.newtask.UrlEntry
import com.fluxdown.app.feature.newtask.inferName
import com.fluxdown.app.ui.TaskVisualState
import com.fluxdown.app.ui.visualState
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.Category
import com.fluxdown.core.model.CategoryIndex
import com.fluxdown.core.model.Queue
import com.fluxdown.core.model.Segment
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.model.TaskRuntime
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.AgentSessionDto
import com.fluxdown.core.protocol.HostCapability
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.RemoteTaskDto
import com.fluxdown.core.protocol.RemoteTaskRules
import com.fluxdown.core.protocol.has
import com.fluxdown.core.store.HostState
import com.fluxdown.core.store.TaskTable
import com.fluxdown.fluxui.data.FlowSegmentUi
import com.fluxdown.fluxui.data.FlowStripState
import java.time.Instant
import java.time.ZoneId
import java.util.Objects
import kotlin.math.max

/*
 * 下载列表的纯派生逻辑：筛选 → 计数 → 排序 → 分区 / 分组 → 扁平条目。
 * 全程在后台调度器（单协程顺序）执行；[DeriveCache] 只在该协程内访问。
 */

private const val REORDER_MIN_MS = 2_000L
private const val REORDER_IDLE_MS = 1_500L

internal class DeriveInput(
    val state: HostState,
    val prefs: ViewPrefs,
    val filter: DownloadsFilter,
    val collapsed: Set<String>,
)

internal class DeriveResult(val list: DownloadsList, val facets: Facets, val retryAtMs: Long)

internal class DeriveCache {
    class Entry(val item: TaskItem, val runtime: TaskRuntime?)

    val items = HashMap<String, Entry>()
    var categoriesRef: List<Category>? = null
    var index: CategoryIndex = CategoryIndex(Category.BUILTIN)
    var sortSig = 0
    var lastReorderMs = 0L
    var lastPos: Map<String, Int> = emptyMap()

    // 远程任务只在分区 / 会话 / 名册 / 分类变化时重建（派生按 10 Hz 运行）
    var remoteRaw: String? = null
    var sessionRaw: String? = null
    var remoteCloud: Any? = null
    var remote: List<RemoteItem> = emptyList()
}

/**
 * 其他设备上执行的远程任务（同 iOS `DownloadsViewModel.remoteTasks`）：主机声明 `agent.remoteTasks` 能力时，
 * 取 `agent.remoteTasks` 分区并去掉目标为本机的镜像（本地已有真实任务）。本机 id 取会话设备，其次名册 `isCurrent`。
 */
private fun remoteItems(s: HostState, cache: DeriveCache): List<RemoteItem> {
    if (!s.has(HostCapability.agentRemoteTasks)) return emptyList()
    val raw = s.sections[HostSection.agentRemoteTasks]
    val session = s.sections[HostSection.agentSession]
    if (raw == cache.remoteRaw && session == cache.sessionRaw && s.cloudDevices === cache.remoteCloud) return cache.remote
    val current = AgentSessionDto.fromJson(Json.parseOrNull(session))?.device?.deviceId?.takeIf { it.isNotEmpty() }
        ?: s.cloudDevices.firstOrNull { it.isCurrent }?.deviceId
    cache.remoteRaw = raw
    cache.sessionRaw = session
    cache.remoteCloud = s.cloudDevices
    cache.remote = RemoteTaskRules.visible(RemoteTaskDto.listFromJson(Json.parseOrNull(raw)), current).map { t ->
        val name = t.fileName.ifEmpty { inferName(UrlEntry(t.url)) }
        RemoteItem(
            task = t,
            name = name,
            category = cache.index.categoryOf(name, t.url),
            site = siteOfUrl(t.url),
            createdSec = t.createdAtSeconds,
        )
    }
    return cache.remote
}

private class Group(val key: String, val label: GroupLabel, val items: List<DownloadItem>, val extra: String? = null)

private class StatusBucket(val key: String, val res: Int, val bucket: Int)

internal fun derive(input: DeriveInput, cache: DeriveCache, nowMs: Long, interactionMs: Long): DeriveResult {
    val s = input.state
    val f = input.filter
    val p = input.prefs

    // 任务表分页载入中：`state.tasks` 不完整（可能为空），不派生行 / 计数 / 空态，也不动行模型缓存（载入后复用）。
    (s.taskTable as? TaskTable.Loading)?.let { loading ->
        return DeriveResult(DownloadsList(emptyList(), emptyList(), 0, loaded = false, loading = loading), Facets.Initial, 0L)
    }

    if (cache.categoriesRef !== s.categories) {
        cache.categoriesRef = s.categories
        cache.index = CategoryIndex(s.categories)
        cache.items.clear()
        cache.remoteRaw = null
        cache.remoteCloud = null
    }
    val index = cache.index
    val queuesById = HashMap<String, Queue>(s.queues.size * 2)
    for (q in s.queues) queuesById[normQueue(q.queueId)] = q

    // 1. 行模型（未变化的复用同一实例）
    val all = ArrayList<TaskItem>(s.tasks.size)
    for (t in s.tasks) all += itemFor(t, s, cache, queuesById)
    if (cache.items.size > all.size) {
        val ids = HashSet<String>(all.size * 2)
        for (it in all) ids += it.id
        cache.items.keys.retainAll(ids)
    }
    val remoteAll = remoteItems(s, cache)

    // 2. 范围（队列 / 搜索）→ 文件夹计数。远程任务：队列是本机概念，限定队列时不出现；搜索按名称 / URL
    val query = f.query.trim().lowercase()
    val scoped = ArrayList<DownloadItem>(all.size + remoteAll.size)
    for (it in all) {
        if (f.queueId != null && normQueue(it.task.queueId) != f.queueId) continue
        if (query.isNotEmpty() && !matchesQuery(it, query)) continue
        scoped += it
    }
    if (f.queueId == null) {
        for (r in remoteAll) {
            if (query.isNotEmpty() && !r.name.lowercase().contains(query) && !r.task.url.lowercase().contains(query)) continue
            scoped += r
        }
    }
    val folderCounts = IntArray(StatusFolder.entries.size)
    for (it in scoped) for (fo in StatusFolder.entries) if (fo.accepts(it)) folderCounts[fo.ordinal]++

    // 3. 文件夹内 → 分类计数（含远程）→ 分类 / 「远程任务」筛选
    val inFolder = scoped.filter { f.folder.accepts(it) }
    val catCounts = HashMap<String, Int>()
    var remoteCount = 0
    for (it in inFolder) {
        if (it is RemoteItem) remoteCount++
        it.category?.let { c -> catCounts[c.id] = (catCounts[c.id] ?: 0) + 1 }
    }
    val selectedCat = if (f.remoteOnly) null else f.categoryId?.takeIf { id -> index.ordered.any { it.id == id && !it.isAll } }
    val pills = ArrayList<CategoryPill>()
    for (c in index.ordered) {
        if (c.isAll || !c.visible) continue
        val n = catCounts[c.id] ?: 0
        if (n > 0 || c.id == selectedCat) pills += CategoryPill(c, n)
    }
    val rows = when {
        f.remoteOnly -> inFolder.filter { it is RemoteItem }
        selectedCat != null -> inFolder.filter { it.category?.id == selectedCat }
        else -> inFolder
    }

    // 4. 排序：本地与远程同一个比较器（进度 / 速度键节流重排）
    var retryAt = 0L
    var ordered = rows.sortedWith(comparatorFor(p))
    val dynamic = p.sortKey == SortKey.Progress || p.sortKey == SortKey.Speed
    val sig = Objects.hash(f, p.sortKey, p.ascending, p.groupBy)
    if (dynamic && cache.sortSig == sig && cache.lastPos.isNotEmpty()) {
        val sinceReorder = nowMs - cache.lastReorderMs
        val sinceTouch = nowMs - interactionMs
        if (sinceReorder < REORDER_MIN_MS || sinceTouch < REORDER_IDLE_MS) {
            val pos = cache.lastPos
            ordered = ordered.sortedBy { pos[it.id] ?: Int.MAX_VALUE }
            retryAt = max(cache.lastReorderMs + REORDER_MIN_MS, interactionMs + REORDER_IDLE_MS)
        } else {
            cache.lastReorderMs = nowMs
        }
    } else {
        cache.lastReorderMs = nowMs
    }
    cache.sortSig = sig
    cache.lastPos = HashMap<String, Int>(ordered.size * 2).also { m -> ordered.forEachIndexed { i, it -> m[it.id] = i } }

    // 5. 分区 / 分组 → 扁平条目（远程行与本地行穿插；远程行以 RemoteItem.id 进多选范围）
    val entries = ArrayList<ListEntry>(ordered.size + 8)
    val visible = ArrayList<String>(ordered.size)
    if (p.groupBy == GroupBy.None) {
        sectioned(ordered, f.folder, entries, visible)
    } else {
        for (g in groupsFor(ordered, p.groupBy, s, index, nowMs)) {
            val closed = g.key in input.collapsed
            entries += GroupHeaderEntry(g.key, g.label, g.items.size, g.extra, closed)
            if (!closed) {
                g.items.forEachIndexed { i, it ->
                    entries += rowEntry(it, RowZone.Card, first = i == 0, last = i == g.items.lastIndex, visible)
                }
            }
        }
    }

    // 6. 队列分面
    val queueCounts = HashMap<String, Int>()
    for (it in all) {
        val k = normQueue(it.task.queueId)
        queueCounts[k] = (queueCounts[k] ?: 0) + 1
    }
    val queueFacets = s.queues.sortedBy { it.position }.map { QueueFacet(it, queueCounts[normQueue(it.queueId)] ?: 0) }

    return DeriveResult(
        list = DownloadsList(entries, visible, s.tasks.size + remoteAll.size, loaded = true, remote = remoteAll),
        facets = Facets(folderCounts.asList(), pills, queueFacets, ordered.size, remoteCount),
        retryAtMs = retryAt,
    )
}

private fun StatusFolder.accepts(it: DownloadItem): Boolean = when (it) {
    is TaskItem -> accepts(it.task)
    is RemoteItem -> accepts(it.task.status)
}

private fun rowEntry(it: DownloadItem, zone: RowZone, first: Boolean, last: Boolean, visible: MutableList<String>): ListEntry =
    when (it) {
        is TaskItem -> {
            visible += it.id
            RowEntry(it, zone, first, last)
        }
        is RemoteItem -> {
            visible += it.id
            RemoteRowEntry(it, zone, first, last)
        }
    }

// ── 行模型 ────────────────────────────────────────────────────────────────

private fun itemFor(t: Task, s: HostState, cache: DeriveCache, queuesById: Map<String, Queue>): TaskItem {
    val speed = s.speeds[t.taskId]
    val sd = speed?.down ?: 0L
    val su = speed?.up ?: 0L
    val rt = s.runtime[t.taskId]
    val qo = s.queueOrder.orderOf(t.taskId)
    val boosted = s.priorityTaskId == t.taskId
    val queue = queuesById[normQueue(t.queueId)]

    val prev = cache.items[t.taskId]
    // 分类只取决于文件名与来源地址（网址规则）：二者未变就沿用上一行的分类，不在 10 Hz 派生里重算规则。
    val prevTask = prev?.item?.task
    val cat = if (prev != null && prevTask != null && prevTask.fileName == t.fileName && prevTask.url == t.url && prevTask.originUrl == t.originUrl) {
        prev.item.category
    } else {
        cache.index.categoryOf(t.fileName, t.shareUrl)
    }
    if (prev != null) {
        val it = prev.item
        if (it.task === t && prev.runtime === rt && it.speedDown == sd && it.speedUp == su &&
            it.queueOrder == qo && it.boosted == boosted && it.category === cat && it.queue === queue
        ) return it
    }

    val visual = t.visualState(qo != null)
    val fragmentProgress = rt?.hlsFragments?.fraction
    val (flow, flowState) = flowFor(t, visual, rt, fragmentProgress)
    val item = TaskItem(
        task = t,
        speedDown = sd,
        speedUp = su,
        queueOrder = qo,
        boosted = boosted,
        category = cat,
        queue = queue,
        site = siteOf(t),
        visual = visual,
        flow = flow,
        flowState = flowState,
        progress = fragmentProgress ?: t.progress,
        hlsFragments = rt?.hlsFragments?.takeIf { fragmentProgress != null },
        eta = if (visual == TaskVisualState.Downloading && fragmentProgress == null) {
            Format.etaSeconds(t.downloadedBytes, t.totalBytes, sd)
        } else null,
    )
    cache.items[t.taskId] = DeriveCache.Entry(item, rt)
    return item
}

/** 来源站点：originUrl ‖ url 的 host（去 www. 与端口）；BT / eD2K 哨兵无 host → 空串。 */
internal fun siteOf(t: Task): String = siteOfUrl(t.originUrl.ifEmpty { t.url })

internal fun siteOfUrl(raw: String): String {
    if (raw.startsWith("magnet:") || raw.startsWith("torrent-file://") || raw.startsWith("ed2k://")) return ""
    return raw.substringAfter("://", "")
        .substringBefore('/').substringBefore('?').substringAfterLast('@').substringBefore(':')
        .removePrefix("www.")
}

private fun matchesQuery(it: TaskItem, q: String): Boolean {
    val t = it.task
    return t.fileName.lowercase().contains(q) ||
        t.url.lowercase().contains(q) ||
        t.originUrl.lowercase().contains(q) ||
        it.site.lowercase().contains(q)
}

/** 流带：HLS 点播按已写入分片数；其他协议按真实字节区间或总字节进度。 */
private fun flowFor(
    t: Task,
    visual: TaskVisualState,
    rt: TaskRuntime?,
    fragmentProgress: Float?,
): Pair<List<FlowSegmentUi>?, FlowStripState> {
    val state = when (visual) {
        TaskVisualState.Downloading -> FlowStripState.Downloading
        TaskVisualState.Queued -> FlowStripState.Queued
        TaskVisualState.Pending -> FlowStripState.Pending
        TaskVisualState.Preparing, TaskVisualState.Verifying -> FlowStripState.Preparing
        TaskVisualState.Paused -> FlowStripState.Paused
        TaskVisualState.Failed -> FlowStripState.Failed
        TaskVisualState.Seeding -> FlowStripState.Seeding
        TaskVisualState.Missing -> FlowStripState.Missing
        TaskVisualState.Completed -> FlowStripState.Completed
    }
    val show = when (visual) {
        TaskVisualState.Completed, TaskVisualState.Missing -> false
        TaskVisualState.Preparing, TaskVisualState.Verifying, TaskVisualState.Downloading, TaskVisualState.Seeding -> true
        else -> fragmentProgress != null || t.totalBytes > 0 || t.downloadedBytes > 0
    }
    if (!show) return null to state
    if (visual == TaskVisualState.Seeding) return listOf(FlowSegmentUi(1f, 1f)) to state
    if (fragmentProgress != null) return listOf(FlowSegmentUi(1f, fragmentProgress)) to state
    val segs = rt?.segments
    if (!segs.isNullOrEmpty() && t.totalBytes > 0 &&
        (visual == TaskVisualState.Downloading || visual == TaskVisualState.Paused || visual == TaskVisualState.Failed)
    ) {
        val live = visual == TaskVisualState.Downloading
        return segs.map { it.toUi(live) } to state
    }
    return listOf(FlowSegmentUi(1f, t.progress ?: 0f)) to state
}

private fun Segment.toUi(live: Boolean): FlowSegmentUi {
    val len = (endByte - startByte + 1).coerceAtLeast(1L)
    return FlowSegmentUi(
        fraction = len.toFloat(),
        filled = (downloadedBytes.toDouble() / len).toFloat().coerceIn(0f, 1f),
        active = live && active == true,
    )
}

// ── 排序（本地与远程同一个比较器，键见 [DownloadItem]）──────────────────────

private fun comparatorFor(p: ViewPrefs): Comparator<DownloadItem> {
    val byId = Comparator<DownloadItem> { a, b -> a.id.compareTo(b.id) }
    if (p.sortKey == SortKey.Smart) {
        return Comparator<DownloadItem> { a, b ->
            val ta = a.sortTier
            val tb = b.sortTier
            if (ta != tb) {
                ta - tb
            } else {
                val c = a.createdSec.compareTo(b.createdSec)
                if (ta <= 2) c else -c
            }
        }.then(byId)
    }
    val key: Comparator<DownloadItem> = when (p.sortKey) {
        SortKey.Created -> Comparator { a, b -> a.createdSec.compareTo(b.createdSec) }
        SortKey.Name -> Comparator { a, b -> String.CASE_INSENSITIVE_ORDER.compare(a.sortName, b.sortName) }
        SortKey.Size -> Comparator { a, b -> a.sizeBytes.compareTo(b.sizeBytes) }
        SortKey.Progress -> Comparator { a, b -> (a.sortProgress ?: -1f).compareTo(b.sortProgress ?: -1f) }
        SortKey.Speed -> Comparator { a, b -> a.sortSpeed.compareTo(b.sortSpeed) }
        else -> Comparator { a, b -> a.sortTier - b.sortTier }
    }
    val directed = if (p.ascending) key else key.reversed()
    return directed
        .then(Comparator { a, b -> b.createdSec.compareTo(a.createdSec) })
        .then(byId)
}

// ── 分区 / 分组 ───────────────────────────────────────────────────────────

private fun sectioned(rows: List<DownloadItem>, folder: StatusFolder, out: MutableList<ListEntry>, visible: MutableList<String>) {
    val inflight = rows.filter { it.inflight }
    val history = rows.filter { !it.inflight }
    if (inflight.isNotEmpty()) {
        // 汇总速度只算本机任务：远程行的速度是另一台设备的吞吐
        out += FlowHeaderEntry(inflight.size, inflight.sumOf { (it as? TaskItem)?.speedDown ?: 0L })
        inflight.forEachIndexed { i, it -> out += rowEntry(it, RowZone.Flow, first = false, last = i == inflight.lastIndex, visible) }
    }
    if (history.isNotEmpty()) {
        val named = folder.takeIf { it == StatusFolder.Completed || it == StatusFolder.Seeding || it == StatusFolder.Failed || it == StatusFolder.Paused }
        out += HistoryHeaderEntry(history.size, named)
        history.forEachIndexed { i, it -> out += rowEntry(it, RowZone.History, first = i == 0, last = i == history.lastIndex, visible) }
    }
}

private fun groupsFor(rows: List<DownloadItem>, by: GroupBy, s: HostState, index: CategoryIndex, nowMs: Long): List<Group> = when (by) {
    GroupBy.None -> emptyList()
    GroupBy.Status -> {
        val order = listOf(
            StatusBucket("status:1", R.string.statusDownloading, 0),
            StatusBucket("status:0", R.string.statusPending, 1),
            StatusBucket("status:4", R.string.statusError, 2),
            StatusBucket("status:2", R.string.statusPaused, 3),
            StatusBucket("status:3", R.string.statusCompleted, 4),
        )
        order.mapNotNull { b ->
            val items = rows.filter { it.statusBucket == b.bucket }
            if (items.isEmpty()) null else Group(b.key, GroupLabel(res = b.res), items)
        }
    }
    GroupBy.Date -> {
        val zone = ZoneId.systemDefault()
        val today = Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate()
        val bounds = longArrayOf(
            today.atStartOfDay(zone).toEpochSecond(),
            today.minusDays(1).atStartOfDay(zone).toEpochSecond(),
            today.minusDays(7).atStartOfDay(zone).toEpochSecond(),
            today.minusDays(30).atStartOfDay(zone).toEpochSecond(),
        )
        val labels = intArrayOf(R.string.today, R.string.yesterday, R.string.thisWeek, R.string.thisMonth, R.string.older)
        val buckets = Array(5) { ArrayList<DownloadItem>() }
        for (it in rows) {
            val sec = it.createdSec
            val b = when {
                sec >= bounds[0] -> 0
                sec >= bounds[1] -> 1
                sec >= bounds[2] -> 2
                sec >= bounds[3] -> 3
                else -> 4
            }
            buckets[b] += it
        }
        buckets.mapIndexedNotNull { i, items ->
            if (items.isEmpty()) null else Group("date:$i", GroupLabel(res = labels[i]), items)
        }
    }
    GroupBy.Type -> {
        val byCat = rows.groupBy { it.category?.id }
        val out = ArrayList<Group>()
        for (c in index.ordered) {
            if (c.isAll) continue
            val items = byCat[c.id] ?: continue
            out += Group("type:${c.id}", GroupLabel(category = c), items)
        }
        byCat[null]?.let { out += Group("type:none", GroupLabel(res = R.string.categoryOther), it) }
        out
    }
    GroupBy.Queue -> {
        // 队列是本机概念：远程行单独一组放最后
        val byQueue = rows.filterIsInstance<TaskItem>().groupBy { normQueue(it.task.queueId) }
        val out = ArrayList<Group>()
        val used = HashSet<String>()
        for (q in s.queues.sortedBy { it.position }) {
            val k = normQueue(q.queueId)
            val items = byQueue[k] ?: continue
            used += k
            out += Group("queue:$k", GroupLabel(queue = q), items)
        }
        for ((k, items) in byQueue) if (k !in used) out += Group("queue:$k", GroupLabel(text = k), items)
        val remote = rows.filter { it is RemoteItem }
        if (remote.isNotEmpty()) out += Group("queue:remote", GroupLabel(res = R.string.remoteTasksGroup), remote)
        out
    }
    GroupBy.Site -> {
        val bySite = rows.groupBy { it.site }
        val named = bySite.filterKeys { it.isNotEmpty() }.toSortedMap(String.CASE_INSENSITIVE_ORDER)
        val out = ArrayList<Group>()
        for ((site, items) in named) out += Group("site:$site", GroupLabel(text = site), items)
        bySite[""]?.let { hostless ->
            // 无 host：磁力 / 种子归「BT · 磁力」，其余（eD2K 等）归「—」（同 iOS）
            val (bt, other) = hostless.partition { it.isBt() }
            if (bt.isNotEmpty()) out += Group("site:", GroupLabel(res = R.string.viewSiteBt), bt)
            if (other.isNotEmpty()) out += Group("site:-", GroupLabel(text = "—"), other)
        }
        out
    }
    GroupBy.Group -> {
        // 任务组是本机概念：远程行归「未分组」
        val byGroup = rows.filterIsInstance<TaskItem>().groupBy { it.task.groupId }
        val out = ArrayList<Group>()
        for (g in s.groups.sortedBy { it.name.lowercase() }) {
            val items = byGroup[g.groupId] ?: continue
            val done = items.count { it.task.status == TaskStatus.Completed }
            out += Group("group:${g.groupId}", GroupLabel(text = g.name), items, extra = "$done/${items.size}")
        }
        val known = s.groups.mapTo(HashSet()) { it.groupId }
        val loose = rows.filter { it !is TaskItem || it.task.groupId.isEmpty() || it.task.groupId !in known }
        if (loose.isNotEmpty()) out += Group("group:none", GroupLabel(res = R.string.ungroupedTasks), loose)
        out
    }
}

private fun DownloadItem.isBt(): Boolean = when (this) {
    is TaskItem -> task.protocol == TaskProtocol.Bt
    is RemoteItem -> task.url.startsWith("magnet:") || task.url.startsWith("torrent-file://")
}
