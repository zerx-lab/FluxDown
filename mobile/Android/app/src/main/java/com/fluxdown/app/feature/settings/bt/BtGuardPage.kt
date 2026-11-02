package com.fluxdown.app.feature.settings.bt

import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.settingChoice
import com.fluxdown.app.feature.settings.settingNumber
import com.fluxdown.app.feature.settings.settingSwitch
import com.fluxdown.app.i18n.str
import com.fluxdown.app.i18n.fill
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.BtGuardAction
import com.fluxdown.core.protocol.BtGuardCloud
import com.fluxdown.core.protocol.BtGuardCloudPhase
import com.fluxdown.core.protocol.BtGuardCounts
import com.fluxdown.core.protocol.BtGuardEntries
import com.fluxdown.core.protocol.BtGuardEntry
import com.fluxdown.core.protocol.BtGuardLayer
import com.fluxdown.core.protocol.BtGuardReport
import com.fluxdown.core.protocol.BtGuardRules
import com.fluxdown.core.protocol.BtGuardText
import com.fluxdown.core.protocol.BtSettingsRow
import com.fluxdown.core.protocol.SettingsForm
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxBadge
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSectionFoot
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.FlowInGate
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.IOException

// LazyColumn 项 key（搜索定位用，与 `itemKeyOf` 共用）
internal const val K_GUARD_VIEW = "guard-view"
internal const val K_GUARD_GENERAL = "guard-general"
internal const val K_GUARD_STATS = "guard-stats"
internal const val K_GUARD_RULES = "guard-rules"
internal const val K_GUARD_THRESHOLDS = "guard-thresholds"
internal const val K_GUARD_ACTIONS = "guard-actions"
internal const val K_GUARD_EXEMPTIONS = "guard-exemptions"
internal const val K_GUARD_CLOUD = "guard-cloud"
internal const val K_GUARD_FILTER = "guard-filter"
internal const val K_GUARD_REPORT = "guard-report"

/** 条目 / 事件每个 LazyColumn 项承载的行数（一个 GlassSection 一块，保持成组外观又不一次组合全部）。 */
private const val CHUNK = 20

/** 防吸血区的子视图（GPUI 的「防吸血 / 封禁名单 / 事件日志」三个页签）。 */
internal enum class GuardView { Settings, Bans, Events }

/** 行 → 所在 LazyColumn 项 key。 */
internal fun guardItemKeyOf(row: BtSettingsRow): String = when (row) {
    BtSettingsRow.GuardEnabled -> K_GUARD_GENERAL
    BtSettingsRow.GuardRuleExcessive, BtSettingsRow.GuardRuleRerequest, BtSettingsRow.GuardRuleRewind,
    BtSettingsRow.GuardRuleLag, BtSettingsRow.GuardRuleFingerprint -> K_GUARD_RULES
    BtSettingsRow.GuardExcessRatio, BtSettingsRow.GuardMinTorrent, BtSettingsRow.GuardRewindTolerance,
    BtSettingsRow.GuardIpv6Prefix -> K_GUARD_THRESHOLDS
    BtSettingsRow.GuardThrottle, BtSettingsRow.GuardBanBase, BtSettingsRow.GuardBanMax,
    BtSettingsRow.GuardDownloadingMax -> K_GUARD_ACTIONS
    BtSettingsRow.GuardSkipPrivate, BtSettingsRow.GuardSkipLan -> K_GUARD_EXEMPTIONS
    BtSettingsRow.GuardCloudEnabled, BtSettingsRow.GuardCloudMode -> K_GUARD_CLOUD
    BtSettingsRow.GuardFilterUrls -> K_GUARD_FILTER
    BtSettingsRow.GuardReportEnabled -> K_GUARD_REPORT
    else -> K_GUARD_GENERAL
}

// ───────────────────────────── 操作 ─────────────────────────────

/** 防吸血页的用户操作（带 toast 反馈）。 */
@Stable
internal class GuardActions(
    private val model: BtGuardModel,
    private val scope: CoroutineScope,
    private val context: Context,
    private val overlays: FluxOverlayState,
    private val errorText: (HostException) -> String,
) {
    var exporting by mutableStateOf(false)
        private set
    var launchCreateDocument: (String) -> Unit = {}
    private var pendingExport: String? = null
    private var pendingLines = 0L

    private fun failed(@androidx.annotation.StringRes id: Int, e: HostException) {
        overlays.toast("${context.str(id)}: ${errorText(e)}", FluxToastKind.Error)
    }

    fun release(ids: List<Long>) {
        if (ids.isEmpty()) return
        scope.launch {
            try {
                val removed = model.remove(ids)
                overlays.toast(context.str(R.string.btGuardReleased, "n" to removed), FluxToastKind.Success)
            } catch (e: CancellationException) {
                throw e
            } catch (e: HostException) {
                failed(R.string.btGuardReleaseFailed, e)
            }
        }
    }

    fun allow(cidr: String) {
        scope.launch {
            try {
                model.allow(cidr)
                overlays.toast(context.str(R.string.btGuardAllowed, "cidr" to cidr), FluxToastKind.Success)
            } catch (e: CancellationException) {
                throw e
            } catch (e: HostException) {
                failed(R.string.btGuardAddFailed, e)
            }
        }
    }

    fun export(layer: BtGuardLayer?) {
        if (exporting) return
        exporting = true
        scope.launch {
            try {
                val result = model.export(layer)
                if (result.text.isBlank()) {
                    overlays.toast(context.str(R.string.btGuardEntriesEmpty), FluxToastKind.Warn)
                } else {
                    pendingExport = result.text
                    pendingLines = result.lines
                    launchCreateDocument("fluxdown-bt-guard.txt")
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: HostException) {
                failed(R.string.btGuardExportFailed, e)
            } finally {
                exporting = false
            }
        }
    }

    /** 文件选择器返回：取消（null）丢弃待写内容，否则在 IO 线程写入。 */
    fun finishExport(uri: Uri?) {
        val text = pendingExport
        val lines = pendingLines
        pendingExport = null
        if (uri == null || text == null) return
        scope.launch {
            try {
                withContext(Dispatchers.IO) {
                    val out = context.contentResolver.openOutputStream(uri, "wt") ?: throw IOException("no output stream")
                    out.use { it.write(text.toByteArray(Charsets.UTF_8)) }
                }
                overlays.toast(context.str(R.string.btGuardExportDone, "n" to lines), FluxToastKind.Success)
            } catch (e: CancellationException) {
                throw e
            } catch (_: IOException) {
                overlays.toast(context.str(R.string.btGuardExportFailed), FluxToastKind.Error)
            } catch (_: SecurityException) {
                overlays.toast(context.str(R.string.btGuardExportFailed), FluxToastKind.Error)
            }
        }
    }
}

@Composable
internal fun rememberGuardActions(model: BtGuardModel, scope: CoroutineScope): GuardActions {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val taskActions = LocalTaskActions.current
    val actions = remember(model, scope, context, overlays, taskActions) {
        GuardActions(model, scope, context, overlays, taskActions::errorText)
    }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        actions.finishExport(uri)
    }
    actions.launchCreateDocument = { name -> launcher.launch(name) }
    return actions
}

// ───────────────────────────── 入口 ─────────────────────────────

/** 防吸血页签的全部列表项；返回下一个可用的 flow 序号。 */
internal fun LazyListScope.btGuardItems(
    start: Int,
    gate: FlowInGate,
    ctx: SettingsCtx,
    model: BtGuardModel,
    actions: GuardActions,
    view: GuardView,
    loggedIn: Boolean,
    onView: (GuardView) -> Unit,
    openSheet: (GuardSheet) -> Unit,
): Int {
    var index = start
    val form = ctx.form
    val enabled = form.bool(BtSettingsRow.GuardEnabled.configKey)
    // 总开关关闭时只保留设置视图的总开关（封禁名单 / 事件日志仍可查看历史，故子视图切换保留）
    flowItem(index++, gate, K_GUARD_VIEW) { GuardViewSwitcher(view, onView) }
    when (view) {
        GuardView.Settings -> index = guardSettingsItems(index, gate, ctx, model, enabled, loggedIn, openSheet)
        GuardView.Bans -> index = guardBanItems(index, gate, ctx, model, actions, openSheet)
        GuardView.Events -> index = guardEventItems(index, gate, ctx, model, openSheet)
    }
    return index
}

@Composable
private fun GuardViewSwitcher(view: GuardView, onView: (GuardView) -> Unit) {
    val options = listOf(
        SegOption(GuardView.Settings, str(R.string.btGuardTab)),
        SegOption(GuardView.Bans, str(R.string.btGuardTabBans)),
        SegOption(GuardView.Events, str(R.string.btGuardTabEvents)),
    )
    FluxSegmented(options = options, selected = view, onSelect = onView)
}

// ───────────────────────────── 设置视图 ─────────────────────────────

private fun LazyListScope.guardSettingsItems(
    start: Int,
    gate: FlowInGate,
    ctx: SettingsCtx,
    model: BtGuardModel,
    enabled: Boolean,
    loggedIn: Boolean,
    openSheet: (GuardSheet) -> Unit,
): Int {
    var index = start
    flowItem(index++, gate, K_GUARD_GENERAL) { GuardGeneralSection(ctx, model) }
    if (!enabled) return index
    flowItem(index++, gate, K_GUARD_STATS) { StatsSection(model) }
    flowItem(index++, gate, K_GUARD_RULES) { RulesSection(ctx) }
    flowItem(index++, gate, K_GUARD_THRESHOLDS) { ThresholdsSection(ctx) }
    flowItem(index++, gate, K_GUARD_ACTIONS) { ActionsSection(ctx) }
    flowItem(index++, gate, K_GUARD_EXEMPTIONS) { ExemptionsSection(ctx) }
    flowItem(index++, gate, K_GUARD_CLOUD) { CloudSection(ctx, model) }
    flowItem(index++, gate, K_GUARD_FILTER) { FilterSection(ctx, model) }
    flowItem(index++, gate, K_GUARD_REPORT) { ReportSection(ctx, loggedIn, openSheet) }
    return index
}

private fun modeLabels(off: String, observe: String, enforce: String) = listOf(
    SelectOption("off", off),
    SelectOption("observe", observe),
    SelectOption("enforce", enforce),
)

@Composable
private fun modeOptions(): List<SelectOption<String>> =
    modeLabels(str(R.string.btGuardModeOff), str(R.string.btGuardModeObserve), str(R.string.btGuardModeEnforce))

@Composable
private fun GuardGeneralSection(ctx: SettingsCtx, model: BtGuardModel) {
    val form = ctx.form
    val state = model.state
    GlassSection {
        if (BtSettingsRow.GuardEnabled.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.GuardEnabled.configKey, R.string.btGuardEnabled, R.string.btGuardEnabledDesc,
                id = BtSettingsRow.GuardEnabled.id,
            )
        }
        row {
            when {
                !form.bool(BtSettingsRow.GuardEnabled.configKey) ->
                    FluxListRow(title = str(R.string.btGuardStatusDisabled), icon = FluxIcons.ShieldAlert, iconTone = Tone.Neutral)
                state == null && model.stateFailed -> FluxActionRow(
                    title = str(R.string.btGuardReload),
                    subtitle = str(R.string.btGuardLoadFailed),
                    icon = FluxIcons.RefreshCw,
                    tone = Tone.Coral,
                    onClick = { model.loadState() },
                )
                state == null -> FluxListRow(title = str(R.string.btGuardLoading), icon = FluxIcons.Shield)
                state.sessionActive ->
                    FluxListRow(title = str(R.string.btGuardSessionOn), icon = FluxIcons.ShieldCheck, iconTone = Tone.Mint)
                else ->
                    FluxListRow(title = str(R.string.btGuardSessionOff), icon = FluxIcons.Shield, iconTone = Tone.Amber)
            }
        }
    }
}

@Composable
private fun StatsSection(model: BtGuardModel) {
    val state = model.state ?: return
    val stats = state.stats
    val counts = state.counts
    GlassSection(
        title = str(R.string.btGuardSectionStats),
        footer = str(
            R.string.btGuardCountsSummary,
            "allow" to counts.allow, "manual" to counts.manual, "auto" to counts.auto,
            "cloud" to counts.cloud, "file" to counts.file,
        ),
    ) {
        row { FluxListRow(title = str(R.string.btGuardStatObserved), value = stats.todayObserved.toString()) }
        row { FluxListRow(title = str(R.string.btGuardStatEnforced), value = stats.todayEnforced.toString()) }
        row { FluxListRow(title = str(R.string.btGuardStatBlocked), value = stats.todayBlockedConnections.toString()) }
        row { FluxListRow(title = str(R.string.btGuardStatRefusedBytes), value = Format.bytes(stats.todayRefusedBytes).toString()) }
    }
}

@Composable
private fun RulesSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btGuardSectionRules), footer = str(R.string.btGuardSectionRulesDesc)) {
        fun rule(row: BtSettingsRow, title: Int, desc: Int) {
            if (row.isVisible(form)) settingChoice(ctx, row.configKey, title, desc, id = row.id) { modeOptions() }
        }
        rule(BtSettingsRow.GuardRuleExcessive, R.string.btGuardRuleExcessive, R.string.btGuardRuleExcessiveDesc)
        rule(BtSettingsRow.GuardRuleRerequest, R.string.btGuardRuleRerequest, R.string.btGuardRuleRerequestDesc)
        rule(BtSettingsRow.GuardRuleRewind, R.string.btGuardRuleRewind, R.string.btGuardRuleRewindDesc)
        rule(BtSettingsRow.GuardRuleLag, R.string.btGuardRuleLag, R.string.btGuardRuleLagDesc)
        rule(BtSettingsRow.GuardRuleFingerprint, R.string.btGuardRuleFingerprint, R.string.btGuardRuleFingerprintDesc)
    }
}

@Composable
private fun ThresholdsSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btGuardSectionThresholds)) {
        if (BtSettingsRow.GuardExcessRatio.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardExcessRatio.configKey, R.string.btGuardExcessRatio, R.string.btGuardExcessRatioDesc,
                range = 110L..1000L, default = 200L, unit = R.string.btGuardUnitPercent, id = BtSettingsRow.GuardExcessRatio.id,
            )
        }
        if (BtSettingsRow.GuardMinTorrent.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardMinTorrent.configKey, R.string.btGuardMinTorrent, R.string.btGuardMinTorrentDesc,
                range = 0L..1_048_576L, default = 64L, unit = R.string.btGuardUnitMib, id = BtSettingsRow.GuardMinTorrent.id,
            )
        }
        if (BtSettingsRow.GuardRewindTolerance.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardRewindTolerance.configKey, R.string.btGuardRewindTolerance, R.string.btGuardRewindToleranceDesc,
                range = 0L..100L, default = 7L, unit = R.string.btGuardUnitPercent, id = BtSettingsRow.GuardRewindTolerance.id,
            )
        }
        if (BtSettingsRow.GuardIpv6Prefix.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardIpv6Prefix.configKey, R.string.btGuardIpv6Prefix, R.string.btGuardIpv6PrefixDesc,
                range = 48L..128L, default = 64L, id = BtSettingsRow.GuardIpv6Prefix.id,
            )
        }
    }
}

@Composable
private fun ActionsSection(ctx: SettingsCtx) {
    val form = ctx.form
    val observe = str(R.string.btGuardDownloadingActionObserve)
    val throttle = str(R.string.btGuardDownloadingActionThrottle)
    val choke = str(R.string.btGuardDownloadingActionChoke)
    val ban = str(R.string.btGuardDownloadingActionBan)
    GlassSection(title = str(R.string.btGuardSectionActions)) {
        if (BtSettingsRow.GuardThrottle.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardThrottle.configKey, R.string.btGuardThrottleKib, R.string.btGuardThrottleKibDesc,
                range = 1L..1_048_576L, default = 32L, unit = R.string.btGuardUnitKibps, id = BtSettingsRow.GuardThrottle.id,
            )
        }
        if (BtSettingsRow.GuardBanBase.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardBanBase.configKey, R.string.btGuardBanBaseHours, R.string.btGuardBanBaseHoursDesc,
                range = 1L..720L, default = 24L, unit = R.string.timeUnitHours, id = BtSettingsRow.GuardBanBase.id,
            )
        }
        if (BtSettingsRow.GuardBanMax.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.GuardBanMax.configKey, R.string.btGuardBanMaxDays, null,
                range = 1L..365L, default = 30L, unit = R.string.timeUnitDays, id = BtSettingsRow.GuardBanMax.id,
            )
        }
        if (BtSettingsRow.GuardDownloadingMax.isVisible(form)) {
            settingChoice(
                ctx, BtSettingsRow.GuardDownloadingMax.configKey, R.string.btGuardDownloadingMaxAction,
                R.string.btGuardDownloadingMaxActionDesc, id = BtSettingsRow.GuardDownloadingMax.id,
            ) {
                listOf(
                    SelectOption("observe", observe),
                    SelectOption("throttle", throttle),
                    SelectOption("choke", choke),
                    SelectOption("ban", ban),
                )
            }
        }
    }
}

@Composable
private fun ExemptionsSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btGuardSectionExemptions)) {
        if (BtSettingsRow.GuardSkipPrivate.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.GuardSkipPrivate.configKey, R.string.btGuardSkipPrivate, R.string.btGuardSkipPrivateDesc,
                id = BtSettingsRow.GuardSkipPrivate.id,
            )
        }
        if (BtSettingsRow.GuardSkipLan.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.GuardSkipLan.configKey, R.string.btGuardSkipLan, R.string.btGuardSkipLanDesc,
                id = BtSettingsRow.GuardSkipLan.id,
            )
        }
    }
}

@Composable
private fun CloudSection(ctx: SettingsCtx, model: BtGuardModel) {
    val form = ctx.form
    val cloudEnabled = form.bool(BtSettingsRow.GuardCloudEnabled.configKey)
    val status = model.state?.cloud
    val phase = status?.let { BtGuardCloud.phase(cloudEnabled, it) }
    GlassSection(title = str(R.string.btGuardSectionCloud)) {
        if (BtSettingsRow.GuardCloudEnabled.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.GuardCloudEnabled.configKey, R.string.btGuardCloudEnabled, R.string.btGuardCloudEnabledDesc,
                id = BtSettingsRow.GuardCloudEnabled.id,
            )
        }
        if (BtSettingsRow.GuardCloudMode.isVisible(form)) {
            settingChoice(
                ctx, BtSettingsRow.GuardCloudMode.configKey, R.string.btGuardCloudMode, null,
                id = BtSettingsRow.GuardCloudMode.id,
            ) { modeOptions() }
        }
        custom(padded = true) {
            if (status == null) {
                FluxText(
                    str(if (model.stateFailed) R.string.btGuardLoadFailed else R.string.btGuardLoading),
                    style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted,
                )
            } else {
                CloudStatusCard(status, phase ?: BtGuardCloudPhase.Waiting)
            }
        }
    }
}

@Composable
private fun CloudStatusCard(status: com.fluxdown.core.protocol.BtGuardCloudStatus, phase: BtGuardCloudPhase) {
    val c = FluxTheme.colors
    val sm = FluxTheme.type.sm
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        when (phase) {
            BtGuardCloudPhase.Disabled -> FluxText(str(R.string.btGuardCloudDisabled), style = sm, color = c.inkMuted)
            BtGuardCloudPhase.Waiting -> FluxText(str(R.string.btGuardCloudNone), style = sm, color = c.inkMuted)
            BtGuardCloudPhase.Stale, BtGuardCloudPhase.Active -> {
                FluxText(str(R.string.btGuardCloudSequence, "n" to status.sequence), style = FluxTheme.type.body, color = c.ink)
                FluxText(
                    str(R.string.btGuardCloudEntries, "entries" to status.entries, "rules" to status.clientRules),
                    style = sm, color = c.inkMuted,
                )
                if (status.updatedAt > 0) {
                    FluxText(str(R.string.btGuardCloudUpdatedAt, "time" to guardTime(status.updatedAt)), style = sm, color = c.inkMuted)
                }
                if (status.expiresAt > 0) {
                    FluxText(str(R.string.btGuardCloudExpiresAt, "time" to guardTime(status.expiresAt)), style = sm, color = c.inkMuted)
                }
                if (phase == BtGuardCloudPhase.Stale) {
                    FluxText(str(R.string.btGuardCloudStale), style = sm, color = c.amberText)
                }
            }
        }
        status.lastError?.let {
            FluxText(
                str(R.string.btGuardCloudLastError, "error" to BtGuardText.clip(it, MAX_ERROR_CHARS)),
                style = sm, color = c.amberText,
            )
        }
        status.rejectedReason?.let {
            FluxText(
                str(R.string.btGuardCloudRejected, "reason" to BtGuardText.clip(it, MAX_ERROR_CHARS)),
                style = sm, color = c.coralText,
            )
        }
        if (status.attribution.isNotEmpty()) {
            FluxText(str(R.string.btGuardCloudAttribution), style = FluxTheme.type.weight(sm, 600), color = c.inkMuted)
            status.attribution.forEach { line ->
                FluxText(BtGuardText.clip(line, MAX_ATTRIBUTION_CHARS), style = sm, color = c.inkFaint)
            }
        }
    }
}

private const val MAX_ERROR_CHARS = 200
private const val MAX_ATTRIBUTION_CHARS = 300

@Composable
private fun FilterSection(ctx: SettingsCtx, model: BtGuardModel) {
    val form = ctx.form
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val overlays = LocalFluxOverlays.current
    val taskActions = LocalTaskActions.current
    val context = LocalContext.current
    val sources = model.state?.filterSources.orEmpty()
    val neverUpdated = str(R.string.btGuardFilterNeverUpdated)
    val emptyText = str(R.string.btGuardFilterEmpty)
    val failedTemplate = str(R.string.btGuardFilterFailed)
    val entriesTemplate = str(R.string.btGuardFilterEntries)
    val updatedTemplate = str(R.string.btGuardFilterUpdatedAt)
    val sourceLines = sources.map { source ->
        val detail = when {
            !source.ok && source.error != null -> failedTemplate.fill("error" to BtGuardText.clip(source.error!!, MAX_ERROR_CHARS))
            source.updatedAt > 0 ->
                entriesTemplate.fill("n" to source.entries) + " · " + updatedTemplate.fill("time" to guardTime(source.updatedAt))
            else -> neverUpdated
        }
        source.url to detail
    }
    GlassSection(title = str(R.string.btGuardSectionFilter)) {
        if (BtSettingsRow.GuardFilterUrls.isVisible(form)) {
            linesRow(
                ctx, BtSettingsRow.GuardFilterUrls.id, BtSettingsRow.GuardFilterUrls.configKey,
                R.string.btGuardFilterUrls, R.string.btGuardFilterUrlsDesc, R.string.btGuardFilterUrlsPlaceholder,
            )
        }
        if (sourceLines.isEmpty()) {
            custom(padded = true) { FluxText(emptyText, style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted) }
        } else {
            sourceLines.forEach { (url, detail) ->
                row {
                    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp)) {
                        // URL 单行中间省略：保留协议头与末段
                        FluxText(url, style = FluxTheme.type.body, color = FluxTheme.colors.ink, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                        FluxText(detail, style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
                    }
                }
            }
        }
        row {
            FluxActionRow(
                title = str(if (model.filterRefreshing) R.string.btGuardFilterRefreshing else R.string.btGuardFilterRefresh),
                icon = FluxIcons.RefreshCw,
                loading = model.filterRefreshing,
                enabled = !ctx.readOnly,
                onClick = {
                    scope.launch {
                        try {
                            model.refreshFilterSources()
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: HostException) {
                            overlays.toast("${context.str(R.string.btGuardLoadFailed)}: ${taskActions.errorText(e)}", FluxToastKind.Error)
                        }
                    }
                },
            )
        }
    }
}

@Composable
private fun ReportSection(ctx: SettingsCtx, loggedIn: Boolean, openSheet: (GuardSheet) -> Unit) {
    val form = ctx.form
    val overlays = LocalFluxOverlays.current
    val key = BtSettingsRow.GuardReportEnabled.configKey
    val enabled = form.bool(key)
    val loginText = str(R.string.btGuardReportLoginRequired)
    val contentsTitle = str(R.string.btGuardReportContentsTitle)
    val contents = str(R.string.btGuardReportContents)
    GlassSection(title = str(R.string.btGuardSectionReport)) {
        if (BtSettingsRow.GuardReportEnabled.isVisible(form)) {
            settingSwitch(
                ctx, key, R.string.btGuardReportEnabled, R.string.btGuardReportEnabledDesc,
                id = BtSettingsRow.GuardReportEnabled.id,
                onChange = { on ->
                    when (BtGuardReport.toggle(turnOn = on, loggedIn = loggedIn)) {
                        BtGuardReport.Toggle.Allowed -> ctx.editor.set(key, SettingsForm.wire(on))
                        BtGuardReport.Toggle.NeedsLogin -> overlays.toast(loginText, FluxToastKind.Warn)
                        BtGuardReport.Toggle.NeedsConsent ->
                            openSheet(GuardSheet.Consent { ctx.editor.set(key, SettingsForm.wire(true)) })
                    }
                },
            )
        }
        if (!loggedIn) {
            custom(padded = true) {
                FluxText(loginText, style = FluxTheme.type.sm, color = FluxTheme.colors.amberText)
            }
        }
        custom(padded = true) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                FluxText(contentsTitle, style = FluxTheme.type.weight(FluxTheme.type.sm, 600), color = FluxTheme.colors.inkMuted)
                FluxText(contents, style = FluxTheme.type.sm, color = FluxTheme.colors.inkFaint)
                if (BtGuardReport.dormant(enabled, loggedIn)) {
                    FluxText(loginText, style = FluxTheme.type.sm, color = FluxTheme.colors.amberText)
                }
            }
        }
    }
}

// ───────────────────────────── 封禁名单 ─────────────────────────────

private fun LazyListScope.guardBanItems(
    start: Int,
    gate: FlowInGate,
    ctx: SettingsCtx,
    model: BtGuardModel,
    actions: GuardActions,
    openSheet: (GuardSheet) -> Unit,
): Int {
    var index = start
    flowItem(index++, gate, "bans-toolbar") { BansToolbar(ctx, model, actions, openSheet) }
    if (model.selected.isNotEmpty()) {
        flowItem(index++, gate, "bans-selection") { SelectionBar(ctx, model, actions) }
    }
    val chunks = model.entries.chunked(CHUNK)
    chunks.forEachIndexed { n, chunk ->
        item(key = "bans-chunk-$n") { EntryChunk(ctx, model, actions, chunk, openSheet) }
    }
    item(key = "bans-footer") { ListFooter(
        loading = model.entriesLoading,
        failed = model.entriesFailed,
        empty = model.entries.isEmpty(),
        filtered = model.layer != null || model.rule != null || model.query.isNotBlank(),
        emptyRes = R.string.btGuardEntriesEmpty,
        filteredRes = R.string.btGuardEntriesFilterEmpty,
        more = model.hasMoreEntries,
        shown = model.entries.size,
        total = model.entriesTotal,
        onMore = model::loadMoreEntries,
        onRetry = model::reloadEntries,
    ) }
    return index
}

@Composable
private fun BansToolbar(ctx: SettingsCtx, model: BtGuardModel, actions: GuardActions, openSheet: (GuardSheet) -> Unit) {
    val counts = model.state?.counts ?: BtGuardCounts()
    val layerOptions = BtGuardEntries.layerChips(counts).map { (layer, n) ->
        SelectOption(layer?.wire.orEmpty(), "${str(layerLabelRes(layer))} ($n)")
    }
    val allRules = str(R.string.btGuardRuleAll)
    val ruleOptions = listOf(SelectOption("", allRules)) +
        BtGuardRules.entryRuleIds.map { SelectOption(it, ruleLabel(it)) }
    val layerTitle = str(R.string.btGuardColLayer)
    val ruleTitle = str(R.string.btGuardColRule)
    GlassSection(footer = str(R.string.btGuardEntriesTotal, "total" to model.entriesTotal)) {
        custom(padded = true) {
            FluxField(
                value = model.query,
                onValueChange = { model.query = it.take(MAX_QUERY_CHARS) },
                placeholder = str(R.string.btGuardSearchPlaceholder),
            )
        }
        row {
            FluxListRow(
                title = layerTitle,
                value = str(layerLabelRes(model.layer)),
                chevron = true,
                onClick = {
                    ctx.openPicker(PickerSpec(layerTitle, layerOptions, model.layer?.wire.orEmpty()) { picked ->
                        model.layer = BtGuardLayer.fromWire(picked)
                    })
                },
            )
        }
        row {
            FluxListRow(
                title = ruleTitle,
                value = model.rule?.let { ruleLabel(it) } ?: allRules,
                chevron = true,
                onClick = {
                    ctx.openPicker(PickerSpec(ruleTitle, ruleOptions, model.rule.orEmpty()) { picked ->
                        model.rule = picked.ifEmpty { null }
                    })
                },
            )
        }
        row {
            FluxActionRow(
                title = str(R.string.btGuardAddEntry),
                icon = FluxIcons.Plus,
                enabled = !ctx.readOnly,
                onClick = { openSheet(GuardSheet.Add()) },
            )
        }
        row {
            FluxActionRow(
                title = str(if (actions.exporting) R.string.btGuardExporting else R.string.btGuardExport),
                icon = FluxIcons.FileDown,
                loading = actions.exporting,
                enabled = !ctx.readOnly,
                onClick = { actions.export(model.layer) },
            )
        }
    }
}

private const val MAX_QUERY_CHARS = 100

@Composable
private fun SelectionBar(ctx: SettingsCtx, model: BtGuardModel, actions: GuardActions) {
    val removable = model.selectedRemovable()
    GlassSection {
        row {
            FluxListRow(
                title = str(R.string.btGuardSelectedCount, "n" to model.selected.size),
                subtitle = if (removable.size < model.selected.size) str(R.string.btGuardCloudEntryHint) else null,
                trailing = { FluxBadge(model.selected.size) },
            )
        }
        row {
            FluxActionRow(
                title = str(R.string.btGuardReleaseSelected),
                icon = FluxIcons.Trash2,
                tone = Tone.Coral,
                enabled = !ctx.readOnly && removable.isNotEmpty(),
                onClick = { actions.release(removable.map { it.id }) },
            )
        }
        row { FluxActionRow(title = str(R.string.cancel), tone = Tone.Accent, onClick = { model.clearSelection() }) }
    }
}

@Composable
private fun EntryChunk(
    ctx: SettingsCtx,
    model: BtGuardModel,
    actions: GuardActions,
    entries: List<BtGuardEntry>,
    openSheet: (GuardSheet) -> Unit,
) {
    val selected = model.selected
    val viewLabel = str(R.string.btGuardPeerViewEvidence)
    val allowLabel = str(R.string.btGuardAllowEntry)
    val releaseLabel = str(R.string.btGuardRelease)
    val cloudHint = str(R.string.btGuardCloudEntryHint)
    // 行内文案需要 @Composable 解析：先算好再进入非组合的 section builder
    val rows = entries.map { entry ->
        val subtitle = BtGuardText.subtitle(
            listOf(layerLabel(entry.layer), actionLabel(entry.action), if (entry.rule.isEmpty()) "" else ruleLabel(entry.rule)),
            entry.reason,
        )
        EntryRowModel(entry, subtitle, remainingText(entry))
    }
    GlassSection {
        rows.forEach { r ->
            row {
                val entry = r.entry
                FluxListRow(
                    title = entry.cidr,
                    subtitle = r.subtitle.ifEmpty { null },
                    value = r.remaining,
                    selected = entry.id in selected,
                    onClick = {
                        if (selected.isNotEmpty()) {
                            model.toggleSelected(entry.id)
                        } else {
                            val options = listOf(
                                SelectOption("detail", viewLabel),
                                SelectOption("allow", allowLabel, enabled = BtGuardEntries.canAllow(entry) && !ctx.readOnly),
                                SelectOption(
                                    "release", releaseLabel,
                                    hint = if (BtGuardEntries.canRemove(entry)) null else cloudHint,
                                    enabled = BtGuardEntries.canRemove(entry) && !ctx.readOnly,
                                ),
                            )
                            ctx.openPicker(PickerSpec(entry.cidr, options, null) { picked ->
                                when (picked) {
                                    "detail" -> openSheet(GuardSheet.EntryDetail(entry))
                                    "allow" -> actions.allow(entry.cidr)
                                    "release" -> actions.release(listOf(entry.id))
                                }
                            })
                        }
                    },
                    onLongClick = { model.toggleSelected(entry.id) },
                )
            }
        }
    }
}

private class EntryRowModel(val entry: BtGuardEntry, val subtitle: String, val remaining: String)

// ───────────────────────────── 事件日志 ─────────────────────────────

private fun LazyListScope.guardEventItems(
    start: Int,
    gate: FlowInGate,
    ctx: SettingsCtx,
    model: BtGuardModel,
    openSheet: (GuardSheet) -> Unit,
): Int {
    var index = start
    flowItem(index++, gate, "events-toolbar") { EventsToolbar(ctx, model) }
    model.events.chunked(CHUNK).forEachIndexed { n, chunk ->
        item(key = "events-chunk-$n") { EventChunk(chunk, openSheet) }
    }
    item(key = "events-footer") { ListFooter(
        loading = model.eventsLoading,
        failed = model.eventsFailed,
        empty = model.events.isEmpty(),
        filtered = model.eventKind != null || model.eventRule != null,
        emptyRes = R.string.btGuardEventsEmpty,
        filteredRes = R.string.btGuardEventsFilterEmpty,
        more = model.hasMoreEvents,
        shown = model.events.size,
        total = model.eventsTotal,
        onMore = model::loadMoreEvents,
        onRetry = model::reloadEvents,
    ) }
    return index
}

@Composable
private fun EventsToolbar(ctx: SettingsCtx, model: BtGuardModel) {
    val allKinds = str(R.string.btGuardEventKindAll)
    val allRules = str(R.string.btGuardRuleAll)
    val kindOptions = listOf(SelectOption("", allKinds)) + com.fluxdown.core.protocol.BtGuardRules.eventKinds.map { SelectOption(it, eventKindLabel(it)) }
    val ruleOptions = listOf(SelectOption("", allRules)) + BtGuardRules.ids.map { SelectOption(it, ruleLabel(it)) }
    val kindTitle = str(R.string.btGuardColKind)
    val ruleTitle = str(R.string.btGuardColRule)
    GlassSection(footer = str(R.string.btGuardEntriesTotal, "total" to model.eventsTotal)) {
        row {
            FluxListRow(
                title = kindTitle,
                value = model.eventKind?.let { eventKindLabel(it) } ?: allKinds,
                chevron = true,
                onClick = {
                    ctx.openPicker(PickerSpec(kindTitle, kindOptions, model.eventKind.orEmpty()) { picked ->
                        model.eventKind = picked.ifEmpty { null }
                    })
                },
            )
        }
        row {
            FluxListRow(
                title = ruleTitle,
                value = model.eventRule?.let { ruleLabel(it) } ?: allRules,
                chevron = true,
                onClick = {
                    ctx.openPicker(PickerSpec(ruleTitle, ruleOptions, model.eventRule.orEmpty()) { picked ->
                        model.eventRule = picked.ifEmpty { null }
                    })
                },
            )
        }
    }
}

@Composable
private fun EventChunk(events: List<com.fluxdown.core.protocol.BtGuardEvent>, openSheet: (GuardSheet) -> Unit) {
    val rows = events.map { event ->
        val subtitle = BtGuardText.subtitle(
            listOf(
                event.ipGroup,
                if (event.rule.isEmpty()) "" else ruleLabel(event.rule),
                if (event.action.isEmpty()) "" else actionLabel(event.action),
            ),
            "",
        )
        Triple(event, eventKindLabel(event.kind), subtitle to guardTime(event.at))
    }
    GlassSection {
        rows.forEach { (event, kind, pair) ->
            row {
                FluxListRow(
                    title = kind,
                    subtitle = pair.first.ifEmpty { null },
                    value = pair.second.ifEmpty { null },
                    chevron = true,
                    onClick = { openSheet(GuardSheet.EventDetail(event)) },
                )
            }
        }
    }
}

// ───────────────────────────── 列表页脚 ─────────────────────────────

@Composable
private fun ListFooter(
    loading: Boolean,
    failed: Boolean,
    empty: Boolean,
    filtered: Boolean,
    emptyRes: Int,
    filteredRes: Int,
    more: Boolean,
    shown: Int,
    total: Long,
    onMore: () -> Unit,
    onRetry: () -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        when {
            failed -> GlassSection {
                row {
                    FluxActionRow(
                        title = str(R.string.btGuardReload),
                        subtitle = str(R.string.btGuardLoadFailed),
                        icon = FluxIcons.RefreshCw,
                        tone = Tone.Coral,
                        loading = loading,
                        onClick = onRetry,
                    )
                }
            }
            loading && empty -> Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { FluxSpinner() }
            empty -> FluxSectionFoot(str(if (filtered) filteredRes else emptyRes))
            more -> GlassSection {
                row {
                    FluxActionRow(
                        title = str(R.string.btGuardLoadMore, "shown" to shown, "total" to total),
                        icon = FluxIcons.ChevronDown,
                        loading = loading,
                        onClick = onMore,
                    )
                }
            }
        }
    }
}
