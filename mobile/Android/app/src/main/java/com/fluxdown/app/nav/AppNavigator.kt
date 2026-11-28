package com.fluxdown.app.nav

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateSet
import androidx.compose.runtime.staticCompositionLocalOf
import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.model.HostRef

/** 顶层目的地（浮动导航坞 / Rail）。 */
enum class AppTab { Downloads, Rss, Devices, Settings }

/** 推入页。medium / expanded 档下 [TaskDetail] 进入右侧详情栏（paneable）。 */
sealed interface Route {
    data class TaskDetail(val taskId: String) : Route
    /** R2：某订阅的条目流（订阅页点按订阅行进入）。 */
    data class RssItems(val sourceId: String) : Route
    /** [arg]：子页参数（如插件详情的插件标识）；分类根页为空。 */
    data class Settings(val page: SettingsPage, val arg: String = "") : Route
}

/**
 * 设置子页（顺序同 iOS `SettingsRoute` / PC `build_pages`，移动端无法使用的桌面专属分类不在此列）。
 * [PluginMarket] / [PluginDetail] 是「扩展」的推入子页（[PluginDetail] 的 `arg` = 插件标识）。
 */
enum class SettingsPage {
    Account, General, Appearance, Notify, Download, Bt, Ed2k, Network,
    Extensions, PluginMarket, PluginDetail, Webhook, Api, Diagnostics, About,
}

/** 浮动 Sheet（同时最多一个；X1–X3 选择请求由 shell 依据主机状态单独呈现）。 */
sealed interface SheetRoute {
    /**
     * N1：[prefill] = 预填链接（粘贴 / 扫码）；[external] = 外部唤起（浏览器外部下载器 / 分享 / 协议链接）的
     * 请求，带 Cookie / 来源页 / 请求头 / 建议文件名；[targetId] = 「下载到」预选的 [com.fluxdown.core.protocol.DispatchTarget.id]
     * （设备页「下发」；null = 当前主机）。同一外部批次分目标依次打开，各组保留自己的下发失败原因。
     */
    data class NewDownload(
        val prefill: String = "",
        val external: List<ExternalDownload> = emptyList(),
        val targetId: String? = null,
        val captureFailure: String? = null,
    ) : SheetRoute
    /** R3：新建（[sourceId] = null，[prefillUrl] 预填地址）/ 编辑订阅。 */
    data class RssEditor(val sourceId: String? = null, val prefillUrl: String = "") : SheetRoute
    /** D1v：视图（分组 / 排序 / 密度 / 卡片字段）。 */
    data object ViewOptions : SheetRoute
    /** 主机切换器。 */
    data object HostSwitch : SheetRoute
    /** V2：添加远程主机（局域网配对 / 访问密钥）；[repair] = 重新配对这台已保存、被拒的主机（预填地址，成功后更新同一主机）。 */
    data class AddHost(val repair: HostRef.Remote? = null) : SheetRoute
    /** V4：添加设备（账户自动 / 直连配对）。 */
    data object AddDevice : SheetRoute
    /** D9：全局活动面板。 */
    data object Activity : SheetRoute
    /** D5：移动到队列。 */
    data class MoveToQueue(val taskIds: List<String>) : SheetRoute
}

/**
 * 应用导航状态（单一事实源）。返回优先级：命令搜索 → Sheet → 多选 → 推入页 → 回到“下载”。
 * 浮层菜单 / 对话框的返回由 FluxOverlayHost 自行拦截（更高 z 序）。
 */
@Stable
class AppNavigator {
    var tab by mutableStateOf(AppTab.Downloads)
        private set

    /** 显式导航优先于尚未结束的根页拖动，包括再次点选当前标签。 */
    var revision by mutableStateOf(0L)
        private set
    val stack = mutableStateListOf<Route>()
    var sheet by mutableStateOf<SheetRoute?>(null)
        private set
    var searchOpen by mutableStateOf(false)
        private set

    /** D2 多选：已选任务 id；[selecting] 为 true 时坞变形为选择坞。 */
    val selection: SnapshotStateSet<String> = SnapshotStateSet()
    var selecting by mutableStateOf(false)
        private set

    /**
     * 「稍后决定」：被用户划走 / 关闭的那批 fileExists 请求 id（不向主机答复，请求照常等到超时）。
     * 新请求到达、回到前台或点任务行「待确认」角标都会重新弹出。
     */
    var deferredFileConflicts by mutableStateOf<Set<String>>(emptySet())
        private set

    fun deferFileConflicts(ids: Set<String>) {
        deferredFileConflicts = ids
    }

    fun reopenFileConflicts() {
        if (deferredFileConflicts.isNotEmpty()) deferredFileConflicts = emptySet()
    }

    val top: Route? get() = stack.lastOrNull()

    fun selectTab(t: AppTab) {
        revision++
        if (tab != t) {
            tab = t
            stack.clear()
            exitSelection()
        }
    }

    /** 仅允许从同一导航快照开始的根页手势在落定后提交。 */
    internal fun selectTabFromSwipe(t: AppTab, expectedRevision: Long): Boolean {
        if (revision != expectedRevision || stack.isNotEmpty() || sheet != null || searchOpen || selecting) return false
        if (tab != t) selectTab(t)
        return true
    }

    fun push(r: Route) {
        val top = stack.lastOrNull()
        when {
            top == r -> Unit
            // 列表旁的详情栏 / 详情页内跳转：替换而不是堆叠，返回直接回到列表
            top is Route.TaskDetail && r is Route.TaskDetail -> {
                revision++
                stack[stack.lastIndex] = r
            }
            else -> {
                revision++
                stack.add(r)
            }
        }
    }

    fun pop(): Boolean {
        if (stack.isEmpty()) return false
        stack.removeAt(stack.lastIndex)
        revision++
        return true
    }

    fun openSheet(s: SheetRoute) {
        revision++
        sheet = s
    }

    fun closeSheet() {
        if (sheet != null) revision++
        sheet = null
    }

    fun openSearch() {
        if (!searchOpen) revision++
        searchOpen = true
    }

    fun closeSearch() {
        if (searchOpen) revision++
        searchOpen = false
    }

    fun enterSelection(firstId: String? = null) {
        revision++
        selecting = true
        if (firstId != null) selection.add(firstId)
    }

    fun toggleSelected(id: String) {
        if (!selection.remove(id)) selection.add(id)
    }

    fun exitSelection() {
        if (selecting) revision++
        selecting = false
        selection.clear()
    }

    /** @return true = 已消费返回。 */
    fun back(): Boolean = when {
        searchOpen -> { closeSearch(); true }
        sheet != null -> { closeSheet(); true }
        selecting -> { exitSelection(); true }
        pop() -> true
        tab != AppTab.Downloads -> { selectTab(AppTab.Downloads); true }
        else -> false
    }
}

val LocalNavigator = staticCompositionLocalOf<AppNavigator> { error("AppNavigator missing") }
