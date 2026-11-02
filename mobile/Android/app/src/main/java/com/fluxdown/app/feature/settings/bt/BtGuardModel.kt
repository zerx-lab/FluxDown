package com.fluxdown.app.feature.settings.bt

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.BT_GUARD_PAGE_SIZE
import com.fluxdown.core.protocol.BtGuardAction
import com.fluxdown.core.protocol.BtGuardDuration
import com.fluxdown.core.protocol.BtGuardEntries
import com.fluxdown.core.protocol.BtGuardEntriesPage
import com.fluxdown.core.protocol.BtGuardEntriesQuery
import com.fluxdown.core.protocol.BtGuardEntry
import com.fluxdown.core.protocol.BtGuardEvent
import com.fluxdown.core.protocol.BtGuardEventsPage
import com.fluxdown.core.protocol.BtGuardEventsQuery
import com.fluxdown.core.protocol.BtGuardExport
import com.fluxdown.core.protocol.BtGuardFilterSource
import com.fluxdown.core.protocol.BtGuardLayer
import com.fluxdown.core.protocol.BtGuardParams
import com.fluxdown.core.protocol.BtGuardState
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.protocol.int
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch

/**
 * BT 防吸血页的数据层：状态总览、封禁条目分页、事件日志分页。读取失败只置失败标记（页面显示重试），
 * 操作类方法把 [HostException] 抛给调用方（由 UI 弹 toast）。
 *
 * 每类列表带代次号：筛选 / 刷新连续触发时，旧请求的结果与收尾一律丢弃。
 * `btGuardChanged` 通知到来时调 [onChanged] 整体刷新（同一时刻只跑一轮，期间再来的通知合并为一次追加刷新）。
 */
@Stable
internal class BtGuardModel(
    private val session: HostSession,
    private val scope: CoroutineScope,
) {
    // ───────────── 状态总览 ─────────────

    var state by mutableStateOf<BtGuardState?>(null)
        private set
    var stateFailed by mutableStateOf(false)
        private set
    var stateLoading by mutableStateOf(false)
        private set
    var filterRefreshing by mutableStateOf(false)
        private set
    private var stateGen = 0

    fun loadState() {
        val gen = ++stateGen
        stateLoading = true
        scope.launch {
            try {
                val next = BtGuardState.fromJson(session.callJson(HostMethod.daemonBtGuardState))
                if (gen == stateGen) {
                    state = next
                    stateFailed = false
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: HostException) {
                if (gen == stateGen) stateFailed = true
            } finally {
                if (gen == stateGen) stateLoading = false
            }
        }
    }

    /** `daemon.bt.guard.filterSources.refresh`：立即拉取过滤文件订阅，结果写回状态。 */
    suspend fun refreshFilterSources() {
        if (filterRefreshing) return
        filterRefreshing = true
        try {
            val sources = BtGuardFilterSource.listFrom(session.callJson(HostMethod.daemonBtGuardFilterSourcesRefresh), "sources")
            state = state?.copy(filterSources = sources)
            loadState()
        } finally {
            filterRefreshing = false
        }
    }

    // ───────────── 封禁条目 ─────────────

    var layer by mutableStateOf<BtGuardLayer?>(null)
    var rule by mutableStateOf<String?>(null)
    var query by mutableStateOf("")

    var entries by mutableStateOf<List<BtGuardEntry>>(emptyList())
        private set
    var entriesTotal by mutableStateOf(0L)
        private set
    var entriesLoading by mutableStateOf(false)
        private set
    var entriesFailed by mutableStateOf(false)
        private set

    /** 已选条目 id（批量解除）。 */
    var selected by mutableStateOf<Set<Long>>(emptySet())
        private set

    private var entriesGen = 0
    private var entriesJob: Job? = null

    val hasMoreEntries: Boolean get() = BtGuardEntries.hasMore(entries.size, entriesTotal)

    fun reloadEntries() {
        val gen = ++entriesGen
        entriesJob?.cancel()
        entriesLoading = true
        val request = BtGuardEntriesQuery(layer, rule, query, offset = 0)
        entriesJob = scope.launch {
            try {
                val page = BtGuardEntriesPage.fromJson(session.callJson(HostMethod.daemonBtGuardEntriesList, request.toJson()))
                if (gen == entriesGen) {
                    entries = page.items
                    entriesTotal = page.total
                    entriesFailed = false
                    val alive = page.items.mapTo(HashSet()) { it.id }
                    selected = selected.filterTo(HashSet()) { it in alive }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: HostException) {
                if (gen == entriesGen) entriesFailed = true
            } finally {
                if (gen == entriesGen) entriesLoading = false
            }
        }
    }

    fun loadMoreEntries() {
        if (entriesLoading || !hasMoreEntries) return
        val gen = ++entriesGen
        entriesLoading = true
        val next = BtGuardEntriesQuery(layer, rule, query, offset = entries.size)
        entriesJob = scope.launch {
            try {
                val page = BtGuardEntriesPage.fromJson(session.callJson(HostMethod.daemonBtGuardEntriesList, next.toJson()))
                if (gen == entriesGen) {
                    entries = BtGuardEntries.merge(entries, page.items)
                    entriesTotal = page.total
                    entriesFailed = false
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: HostException) {
                if (gen == entriesGen) entriesFailed = true
            } finally {
                if (gen == entriesGen) entriesLoading = false
            }
        }
    }

    fun toggleSelected(id: Long) {
        selected = if (id in selected) selected - id else selected + id
    }

    fun clearSelection() {
        selected = emptySet()
    }

    /** 当前已选且允许解除的条目（云层条目不可删）。 */
    fun selectedRemovable(): List<BtGuardEntry> =
        entries.filter { it.id in selected && BtGuardEntries.canRemove(it) }

    /** `daemon.bt.guard.entries.remove`：返回实际移除数；成功后本地剔除并刷新总览。 */
    suspend fun remove(ids: List<Long>): Int {
        if (ids.isEmpty()) return 0
        val removed = session.callJson(HostMethod.daemonBtGuardEntriesRemove, BtGuardParams.removeParams(ids)).int("removed")
        val gone = ids.toSet()
        entries = BtGuardEntries.without(entries, gone)
        entriesTotal = (entriesTotal - removed).coerceAtLeast(0)
        selected = selected - gone
        loadState()
        return removed
    }

    /** `daemon.bt.guard.entries.add`：成功后刷新列表与总览（手动 / 白名单层条目总在列表最前）。 */
    suspend fun add(cidr: String, action: BtGuardAction, duration: BtGuardDuration, reason: String): BtGuardEntry {
        val entry = BtGuardEntry.fromJson(
            session.callJson(HostMethod.daemonBtGuardEntriesAdd, BtGuardParams.addParams(cidr, action, duration, reason)),
        )
        reloadEntries()
        loadState()
        return entry
    }

    /** 加白（永久，白名单层）。 */
    suspend fun allow(cidr: String): BtGuardEntry {
        val entry = BtGuardEntry.fromJson(session.callJson(HostMethod.daemonBtGuardEntriesAdd, BtGuardParams.allowParams(cidr)))
        reloadEntries()
        loadState()
        return entry
    }

    suspend fun export(layer: BtGuardLayer?): BtGuardExport =
        BtGuardExport.fromJson(session.callJson(HostMethod.daemonBtGuardExport, BtGuardParams.exportParams(layer)))

    // ───────────── 事件日志 ─────────────

    var eventKind by mutableStateOf<String?>(null)
    var eventRule by mutableStateOf<String?>(null)

    var events by mutableStateOf<List<BtGuardEvent>>(emptyList())
        private set
    var eventsTotal by mutableStateOf(0L)
        private set
    var eventsLoading by mutableStateOf(false)
        private set
    var eventsFailed by mutableStateOf(false)
        private set

    private var eventsGen = 0
    private var eventsJob: Job? = null

    val hasMoreEvents: Boolean get() = BtGuardEntries.hasMore(events.size, eventsTotal)

    fun reloadEvents() {
        val gen = ++eventsGen
        eventsJob?.cancel()
        eventsLoading = true
        val query = BtGuardEventsQuery(eventRule, eventKind, offset = 0)
        eventsJob = scope.launch {
            try {
                val page = BtGuardEventsPage.fromJson(session.callJson(HostMethod.daemonBtGuardEventsList, query.toJson()))
                if (gen == eventsGen) {
                    events = page.items
                    eventsTotal = page.total
                    eventsFailed = false
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: HostException) {
                if (gen == eventsGen) eventsFailed = true
            } finally {
                if (gen == eventsGen) eventsLoading = false
            }
        }
    }

    fun loadMoreEvents() {
        if (eventsLoading || !hasMoreEvents) return
        val gen = ++eventsGen
        eventsLoading = true
        val next = BtGuardEventsQuery(eventRule, eventKind, offset = events.size)
        eventsJob = scope.launch {
            try {
                val page = BtGuardEventsPage.fromJson(session.callJson(HostMethod.daemonBtGuardEventsList, next.toJson()))
                if (gen == eventsGen) {
                    val seen = events.mapTo(HashSet()) { it.id }
                    events = events + page.items.filter { seen.add(it.id) }
                    eventsTotal = page.total
                    eventsFailed = false
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: HostException) {
                if (gen == eventsGen) eventsFailed = true
            } finally {
                if (gen == eventsGen) eventsLoading = false
            }
        }
    }

    // ───────────── 变化通知 ─────────────

    private var refreshing = false
    private var dirty = false
    private var entriesWanted = false
    private var eventsWanted = false

    /** 页面当前可见的子视图：只有可见的列表才随通知刷新（直接赋值，切走即停止）。 */
    fun markVisible(entries: Boolean, events: Boolean) {
        entriesWanted = entries
        eventsWanted = events
    }

    /** `btGuardChanged` 通知：刷新总览与已加载过的列表。 */
    fun onChanged() {
        if (refreshing) {
            dirty = true
            return
        }
        refreshing = true
        scope.launch {
            try {
                do {
                    dirty = false
                    loadState()
                    if (entriesWanted) reloadEntries()
                    if (eventsWanted) reloadEvents()
                } while (dirty)
            } finally {
                refreshing = false
            }
        }
    }

    companion object {
        const val PAGE_SIZE = BT_GUARD_PAGE_SIZE
    }
}
