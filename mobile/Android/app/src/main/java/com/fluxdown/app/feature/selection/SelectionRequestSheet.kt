package com.fluxdown.app.feature.selection

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.State
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.fileCategory
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.CategoryIndex
import com.fluxdown.core.model.SelectionKind
import com.fluxdown.core.model.SelectionOutcome
import com.fluxdown.core.model.SelectionRequest
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.store.HostStore
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxRadioRow
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.data.CountdownRing
import com.fluxdown.fluxui.data.FileTile
import com.fluxdown.fluxui.data.FileTileSize
import com.fluxdown.fluxui.data.fileTileIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxHaptics
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlin.math.ceil

/** 退场动画期间保留最后一次的请求与已处理的请求 id。 */
private class SheetMemory {
    var last: SelectionRequest? = null
    var shownId: String? = null
}

/**
 * X1 BT 文件 / X2 HLS 画质 / X3 插件变体：引擎发起的选择请求。
 *
 * - [request] 为状态里最早的待处理请求（null = 没有）；请求从状态消失即关闭（含其它设备已答复）。
 * - 倒计时按 `deadlineUnixMs` 与当前时间逐帧重算（不累减），到 0 以 `defaultChoice` 答复；
 *   无截止（[SelectionRequest.hasDeadline] 为 false）时不显示倒计时、不自动答复。
 * - X1 / X3 可划走（= [SelectionOutcome.Cancelled]），X2 不可划走。
 */
@Composable
fun SelectionRequestSheet(request: SelectionRequest?) {
    val memory = remember { SheetMemory() }
    if (request != null) memory.last = request
    val shown = request ?: memory.last ?: return

    val overlays = LocalFluxOverlays.current
    val context = LocalContext.current
    val resolver = rememberSelectionResolver()

    // 请求消失而不是我们答复的 → 其它设备已完成选择
    val currentId = request?.requestId
    LaunchedEffect(currentId) {
        val prev = memory.shownId
        if (prev != null && prev != currentId && !resolver.wasOurs(prev)) {
            overlays.toast(context.str(R.string.mobileSelectionResolvedElsewhere), FluxToastKind.Info, FluxIcons.CircleCheck)
        }
        memory.shownId = currentId
    }

    key(shown.requestId) {
        val host = hostState()
        val task by remember { derivedStateOf { host.value.task(shown.taskId) } }
        val countdown = rememberCountdown(shown, active = request != null)

        // 到 0：按默认项答复（无截止的请求只等用户）
        if (shown.hasDeadline) {
            LaunchedEffect(shown.requestId) {
                snapshotFlow { countdown.seconds }.first { it <= 0 }
                resolver.resolve(
                    shown,
                    shown.defaultChoice,
                    context.str(R.string.mobileSelectionAutoApplied),
                    userInitiated = false,
                )
            }
        }

        val ui = SelectionUi(shown, countdown, resolver, taskOf = { task }, visible = request != null)
        when (val kind = shown.kind) {
            is SelectionKind.Bt -> BtSelectionSheet(ui, kind)
            is SelectionKind.Hls -> HlsSelectionSheet(ui, kind)
            is SelectionKind.Variant -> VariantSelectionSheet(ui, kind)
            is SelectionKind.Plugin -> PluginSelectionSheet(ui, kind)
            // fileExists 不走单请求 Sheet：由 FileConflictSheet 聚合呈现（shell 只把其它请求传进来）
            is SelectionKind.FileExists -> Unit
        }
    }
}

/** 三种 Sheet 共用的上下文。 */
@Stable
internal class SelectionUi(
    val request: SelectionRequest,
    val countdown: Countdown,
    val resolver: SelectionResolver,
    val taskOf: () -> Task?,
    val visible: Boolean,
)

/** 选择请求的答复器（单请求 Sheet 与聚合 fileExists 对话框各持一份，互不影响去重与“本机答复”登记）。 */
@Composable
internal fun rememberSelectionResolver(): SelectionResolver {
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val haptics = FluxTheme.haptics
    val context = LocalContext.current
    return remember(container, overlays, haptics, actions) {
        SelectionResolver(container.appScope, { container.session }, container.store, overlays, haptics, context.applicationContext, actions::errorText)
    }
}

// ── 答复 ──────────────────────────────────────────────────────────────────

/** 经 [HostSession.resolveSelection] 答复；去重、只读拦截与错误 toast 集中在这里。 */
@Stable
internal class SelectionResolver(
    private val scope: CoroutineScope,
    private val session: () -> HostSession,
    private val store: HostStore,
    private val overlays: FluxOverlayState,
    private val haptics: FluxHaptics,
    private val context: Context,
    private val errorText: (HostException) -> String,
) {
    private val inFlight = HashSet<String>()
    private val ours = HashSet<String>()

    fun wasOurs(requestId: String): Boolean = requestId in ours

    /**
     * @param successToast 答复成功后的提示；null 不提示
     * @param userInitiated 用户操作才受只读拦截；倒计时自动答复照常尝试
     */
    fun resolve(request: SelectionRequest, outcome: SelectionOutcome, successToast: String?, userInitiated: Boolean) =
        resolveAll(listOf(request to outcome), successToast, userInitiated)

    /**
     * 逐条答复一批请求（聚合 fileExists 对话框的批量按钮）；整批只触发一次成功 / 失败反馈。
     * 请求已被其它设备答复或已到期（Conflict / NotFound）不算失败；其它错误保留该请求（仍在待选列表里）并提示，用户可重试。
     */
    fun resolveAll(items: List<Pair<SelectionRequest, SelectionOutcome>>, successToast: String?, userInitiated: Boolean) {
        if (userInitiated && store.state.value.isReadOnly) {
            haptics.reject()
            overlays.toast(context.str(R.string.localServiceDisconnected), FluxToastKind.Error, FluxIcons.WifiOff)
            return
        }
        val fresh = items.filter { inFlight.add(it.first.requestId) }
        if (fresh.isEmpty()) return
        for ((request, _) in fresh) ours += request.requestId // 先登记：SelectionResolved 可能先于本协程恢复到达
        scope.launch {
            var done = 0
            var failure: HostException? = null
            for ((request, outcome) in fresh) {
                val id = request.requestId
                try {
                    session().resolveSelection(id, outcome)
                    done++
                } catch (e: HostException) {
                    when {
                        e.code == HostErrorCode.Conflict || e.code == HostErrorCode.NotFound -> ours -= id
                        // 引擎已按默认项处理而状态还没刷新：不算失败
                        store.state.value.selections.any { it.requestId == id } -> {
                            ours -= id
                            failure = e
                        }
                    }
                } finally {
                    inFlight.remove(id)
                }
            }
            if (done > 0) {
                haptics.confirm()
                successToast?.let { overlays.toast(it, FluxToastKind.Success, FluxIcons.CircleCheck) }
            }
            failure?.let {
                haptics.reject()
                overlays.toast(errorText(it), FluxToastKind.Error)
            }
        }
    }

    /** 划走 / 取消：任务保持暂停。 */
    fun cancel(request: SelectionRequest) {
        resolve(request, SelectionOutcome.Cancelled, context.str(R.string.mobileSelectionCancelled), userInitiated = true)
    }

    /** 已改动选择后划走：先确认。 */
    fun confirmDiscard(request: SelectionRequest) {
        overlays.showDialog(
            FluxDialogSpec(
                title = context.str(R.string.mobileSelectionDiscardTitle),
                message = context.str(R.string.mobileSelectionDiscardMessage),
                icon = FluxIcons.TriangleAlert,
                buttons = listOf(
                    FluxDialogButton(context.str(R.string.mobileKeepEditing)),
                    FluxDialogButton(context.str(R.string.mobileDiscard), FluxDialogButtonStyle.Destructive) { cancel(request) },
                ),
            ),
        )
    }

    fun rejectDismiss() = haptics.reject()
}

// ── 倒计时 ────────────────────────────────────────────────────────────────

@Stable
internal class Countdown(
    private val deadlineMs: Long,
    private val totalMs: Long,
    private val now: State<Long>,
    secondsState: State<Int>,
) {
    val seconds: Int by secondsState
    val warn: Boolean get() = seconds in 0..5

    /** 有截止时刻；无截止时页脚不显示倒计时。 */
    val enabled: Boolean get() = deadlineMs != SelectionRequest.NO_DEADLINE

    /** 环的剩余比例（1 = 满环）；在绘制阶段读取，逐帧重算。 */
    fun fraction(): Float = ((deadlineMs - now.value).toFloat() / totalMs).coerceIn(0f, 1f)
}

/**
 * 剩余秒数 = ceil((deadline − 当前时间) / 1000)，每帧用当前时间重算而不是累减；
 * [active] 为 false（请求已消失，退场动画中）时停止刷新。
 */
@Composable
internal fun rememberCountdown(request: SelectionRequest, active: Boolean): Countdown =
    rememberCountdown(request.requestId, request.deadlineUnixMs, active)

/** 以 [key]（身份）与截止时间 [deadlineUnixMs] 驱动的倒计时：[key] 变化即重新起算。 */
@Composable
internal fun rememberCountdown(key: Any, deadlineUnixMs: Long, active: Boolean): Countdown {
    val reduce = FluxTheme.motion.reduce
    val now = remember(key) { mutableLongStateOf(System.currentTimeMillis()) }
    val total = remember(key) { (deadlineUnixMs - System.currentTimeMillis()).coerceAtLeast(1_000L) }
    val seconds = remember(key) {
        derivedStateOf { ceil((deadlineUnixMs - now.longValue) / 1000.0).toInt().coerceAtLeast(0) }
    }
    LaunchedEffect(key, active, reduce) {
        if (!active || deadlineUnixMs == SelectionRequest.NO_DEADLINE) return@LaunchedEffect
        while (true) {
            val t = System.currentTimeMillis()
            now.longValue = t
            if (t >= deadlineUnixMs) break
            if (reduce) delay(1000L - t % 1000L) else withFrameMillis { }
        }
    }
    return remember(key) { Countdown(deadlineUnixMs, total, now, seconds) }
}

// ── 共用 UI ───────────────────────────────────────────────────────────────

/** 任务行：FileTile sm + 等宽文件名 + 协议标签。 */
@Composable
internal fun SelectionTaskRow(task: Task?, modifier: Modifier = Modifier) {
    if (task == null) return
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val host = hostState()
    val categories by remember { derivedStateOf { host.value.categories } }
    val cat = remember(categories, task.fileName, task.shareUrl) { CategoryIndex(categories).categoryOf(task.fileName, task.shareUrl).fileCategory() }
    Row(
        modifier
            .fillMaxWidth()
            .padding(horizontal = 6.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        FileTile(fileTileIcon(cat, task.protocol == TaskProtocol.Bt), c.category(cat), size = FileTileSize.Sm)
        FluxText(task.fileName, style = type.mono, color = c.ink, maxLines = 1, modifier = Modifier.weight(1f))
        FluxTag(selectionTag(task))
    }
}

private fun selectionTag(task: Task): String = when (task.protocol) {
    TaskProtocol.Http -> "HTTP"
    TaskProtocol.Ftp -> "FTP"
    TaskProtocol.Bt -> "BT"
    TaskProtocol.Ed2k -> "ED2K"
    TaskProtocol.Hls -> "HLS"
    TaskProtocol.Plugin -> "PLUGIN"
}

/** 倒计时环 + 文案（[labelRes] 带 `{seconds}` 占位符）。 */
@Composable
internal fun CountdownLine(countdown: Countdown, labelRes: Int = R.string.selectionAutoDefaultIn) {
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val seconds = countdown.seconds
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        CountdownRing(
            remainingFraction = countdown::fraction,
            seconds = seconds,
            warn = countdown.warn,
            size = 40.dp,
            liveDescription = str(labelRes, "seconds" to seconds),
        )
        FluxText(
            str(labelRes, "seconds" to seconds),
            style = type.sm,
            color = if (countdown.warn) c.amberText else c.inkMuted,
            modifier = Modifier.weight(1f),
        )
    }
}

/** 页脚：左侧倒计时环 + 文案；其下是操作按钮行。 */
@Composable
internal fun SelectionFooter(countdown: Countdown, actions: @Composable RowScope.() -> Unit) {
    FluxSheetFooter {
        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (countdown.enabled) CountdownLine(countdown)
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                actions()
            }
        }
    }
}

// ── X2 HLS 画质 ──────────────────────────────────────────────────────────

/** X2：不可划走、无关闭钮、返回键被忽略（REJECT）。选项按带宽降序，首项为最高画质。 */
@Composable
private fun HlsSelectionSheet(ui: SelectionUi, kind: SelectionKind.Hls) {
    val request = ui.request
    val sorted = remember(kind) { kind.options.sortedByDescending { it.bandwidth } }
    val defaultIndex = (request.defaultChoice as? SelectionOutcome.Hls)?.index
    var selected by remember { mutableStateOf(sorted.firstOrNull { it.index == defaultIndex }?.index ?: sorted.firstOrNull()?.index) }
    val context = LocalContext.current

    FluxSheet(
        visible = ui.visible,
        onDismissRequest = ui.resolver::rejectDismiss,
        detent = FluxSheetDetent.Wrap,
        dismissible = false,
        title = str(R.string.hlsQualityTitle),
        header = { FluxSheetHeader(title = str(R.string.hlsQualityTitle), subtitle = str(R.string.hlsQualityDesc)) },
        footer = {
            SelectionFooter(ui.countdown) {
                FluxButton(
                    str(R.string.confirm),
                    {
                        val pick = sorted.firstOrNull { it.index == selected } ?: return@FluxButton
                        ui.resolver.resolve(
                            request,
                            SelectionOutcome.Hls(pick.index),
                            context.str(R.string.mobileSelectionPicked, "label" to hlsLabel(context, pick.height, pick.bandwidth)),
                            userInitiated = true,
                        )
                    },
                    variant = ButtonVariant.Primary,
                    fullWidth = true,
                    enabled = selected != null,
                )
            }
        },
    ) {
        SelectionTaskRow(ui.taskOf())
        val best = str(R.string.mobileQualityBest)
        GlassSection(Modifier.padding(top = 8.dp)) {
            sorted.forEachIndexed { i, o ->
                row {
                    FluxRadioRow(
                        title = hlsLabelText(o.height, o.bandwidth),
                        selected = o.index == selected,
                        onSelect = { selected = o.index },
                        subtitle = hlsSubtitle(o.width, o.height, o.bandwidth),
                        value = if (i == 0) best else null,
                    )
                }
            }
        }
    }
}

@Composable
private fun hlsLabelText(height: Long, bandwidth: Long): String =
    if (height > 0) "${height}p" else str(R.string.mobileKbps, "n" to bandwidth / 1000)

private fun hlsLabel(context: Context, height: Long, bandwidth: Long): String =
    if (height > 0) "${height}p" else context.str(R.string.mobileKbps, "n" to bandwidth / 1000)

@Composable
private fun hlsSubtitle(width: Long, height: Long, bandwidth: Long): String? {
    val parts = ArrayList<String>(2)
    if (width > 0 && height > 0) parts += "${width}×$height"
    if (bandwidth > 0) parts += str(R.string.mobileKbps, "n" to bandwidth / 1000)
    return parts.takeIf { it.isNotEmpty() }?.joinToString(" · ")
}

// ── X3 插件变体 ──────────────────────────────────────────────────────────

/** X3：按插件给出的顺序，默认第一项；可划走（= 取消）。 */
@Composable
private fun VariantSelectionSheet(ui: SelectionUi, kind: SelectionKind.Variant) {
    val request = ui.request
    val defaultIndex = (request.defaultChoice as? SelectionOutcome.Variant)?.index
    var selected by remember { mutableStateOf(kind.options.firstOrNull { it.index == defaultIndex }?.index ?: kind.options.firstOrNull()?.index) }
    val context = LocalContext.current

    FluxSheet(
        visible = ui.visible,
        onDismissRequest = { ui.resolver.cancel(request) },
        detent = FluxSheetDetent.Wrap,
        title = str(R.string.resolveVariantTitle),
        header = {
            FluxSheetHeader(
                title = str(R.string.resolveVariantTitle),
                subtitle = str(R.string.resolveVariantDesc),
                onClose = { ui.resolver.cancel(request) },
            )
        },
        footer = {
            SelectionFooter(ui.countdown) {
                FluxButton(str(R.string.cancel), { ui.resolver.cancel(request) }, modifier = Modifier.weight(1f))
                FluxButton(
                    str(R.string.confirm),
                    {
                        val pick = kind.options.firstOrNull { it.index == selected } ?: return@FluxButton
                        ui.resolver.resolve(
                            request,
                            SelectionOutcome.Variant(pick.index),
                            context.str(R.string.mobileSelectionPicked, "label" to pick.label),
                            userInitiated = true,
                        )
                    },
                    variant = ButtonVariant.Primary,
                    modifier = Modifier.weight(1f),
                    enabled = selected != null,
                )
            }
        },
    ) {
        SelectionTaskRow(ui.taskOf())
        GlassSection(Modifier.padding(top = 8.dp)) {
            kind.options.forEach { o ->
                row {
                    val sub = listOfNotNull(
                        o.container.takeIf { it.isNotEmpty() },
                        if (o.width > 0 && o.height > 0) "${o.width}×${o.height}" else null,
                    ).joinToString(" · ").ifEmpty { null }
                    FluxRadioRow(
                        title = o.label,
                        selected = o.index == selected,
                        onSelect = { selected = o.index },
                        subtitle = sub,
                        value = if (o.totalBytes > 0) Format.bytes(o.totalBytes).toString() else null,
                    )
                }
            }
        }
    }
}

// ── 插件选择 ─────────────────────────────────────────────────────────────

/** 多个插件解析器匹配同一 URL：默认项预选，可勾选「始终使用」记住站点默认；可划走（= 取消，下次启动重新询问）。 */
@Composable
private fun PluginSelectionSheet(ui: SelectionUi, kind: SelectionKind.Plugin) {
    val request = ui.request
    val c = FluxTheme.colors
    val type = FluxTheme.type
    val defaultIdentity = (request.defaultChoice as? SelectionOutcome.Plugin)?.identity
    var selected by remember {
        mutableStateOf(kind.candidates.firstOrNull { it.identity == defaultIdentity }?.identity ?: kind.candidates.firstOrNull()?.identity)
    }
    var rememberChoice by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val title = str(R.string.pluginChoiceTitle)

    FluxSheet(
        visible = ui.visible,
        onDismissRequest = { ui.resolver.cancel(request) },
        detent = FluxSheetDetent.Wrap,
        title = title,
        header = {
            FluxSheetHeader(
                title = title,
                subtitle = str(R.string.pluginChoiceDesc, "count" to kind.candidates.size, "site" to kind.site),
                onClose = { ui.resolver.cancel(request) },
            )
        },
        footer = {
            SelectionFooter(ui.countdown) {
                FluxButton(str(R.string.cancel), { ui.resolver.cancel(request) }, modifier = Modifier.weight(1f))
                FluxButton(
                    str(R.string.pluginChoiceConfirm),
                    {
                        val pick = kind.candidates.firstOrNull { it.identity == selected } ?: return@FluxButton
                        ui.resolver.resolve(
                            request,
                            SelectionOutcome.Plugin(pick.identity, rememberChoice),
                            context.str(R.string.mobileSelectionPicked, "label" to pick.name.ifEmpty { pick.identity }),
                            userInitiated = true,
                        )
                    },
                    variant = ButtonVariant.Primary,
                    modifier = Modifier.weight(1f),
                    enabled = selected != null,
                )
            }
        },
    ) {
        SelectionTaskRow(ui.taskOf())
        FluxText(
            kind.url,
            Modifier.fillMaxWidth().padding(horizontal = 6.dp, vertical = 4.dp),
            style = type.mono,
            color = c.inkMuted,
            maxLines = 1,
        )
        GlassSection(Modifier.padding(top = 8.dp)) {
            kind.candidates.forEach { p ->
                row {
                    val meta = listOf(p.version, p.identity).filter { it.isNotEmpty() }.joinToString(" · ")
                    val sub = listOf(meta, p.description).filter { it.isNotEmpty() }.joinToString("\n").ifEmpty { null }
                    FluxRadioRow(
                        title = p.name.ifEmpty { p.identity },
                        selected = p.identity == selected,
                        onSelect = { selected = p.identity },
                        subtitle = sub,
                        value = if (p.identity == defaultIdentity) str(R.string.pluginChoiceDefaultBadge) else null,
                    )
                }
            }
        }
        GlassSection(Modifier.padding(top = 8.dp)) {
            row {
                FluxSwitchRow(
                    title = str(R.string.pluginChoiceRemember, "site" to kind.site),
                    checked = rememberChoice,
                    onCheckedChange = { rememberChoice = it },
                    subtitle = str(R.string.pluginChoiceRememberHint),
                )
            }
        }
    }
}
