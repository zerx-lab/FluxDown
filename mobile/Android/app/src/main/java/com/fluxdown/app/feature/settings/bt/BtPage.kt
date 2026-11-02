package com.fluxdown.app.feature.settings.bt

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.LocalSettingsFocus
import com.fluxdown.app.feature.settings.PickerSheetHost
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.SettingRowBox
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.feature.settings.SettingsEntry
import com.fluxdown.app.feature.settings.SettingsPageFrame
import com.fluxdown.app.feature.settings.SettingsSearchContext
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.rememberSettingsCtx
import com.fluxdown.app.feature.settings.settingChoice
import com.fluxdown.app.feature.settings.settingNumber
import com.fluxdown.app.feature.settings.settingStepper
import com.fluxdown.app.feature.settings.settingSwitch
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.protocol.AgentSessionDto
import com.fluxdown.core.protocol.HostNotice
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import kotlinx.coroutines.delay
import com.fluxdown.core.protocol.BtMseMode
import com.fluxdown.core.protocol.BtPortRange
import com.fluxdown.core.protocol.BtReadout
import com.fluxdown.core.protocol.BtSettingsRow
import com.fluxdown.core.protocol.BtSettingsTab
import com.fluxdown.core.protocol.SettingsCatalog
import com.fluxdown.core.protocol.SubscriptionKind
import com.fluxdown.fluxui.controls.FluxFieldRow
import com.fluxdown.fluxui.controls.FluxNumberField
import com.fluxdown.fluxui.controls.FluxSectionFoot
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme

// LazyColumn 项 key（搜索定位用，与 [btSearchEntries] 共用）
private const val K_GENERAL = "general"
private const val K_TRACKER_LIST = "tracker-list"
private const val K_TRACKER_SUB = "tracker-sub"
private const val K_SEED_MAIN = "seed-main"
private const val K_SEED_LIMITS = "seed-limits"

/** 封禁名单搜索框输入后的防抖，避免逐字请求主机。 */
private const val SEARCH_DEBOUNCE_MS = 150L

private fun itemKeyOf(row: BtSettingsRow): String = when (row.tab) {
    BtSettingsTab.Guard -> guardItemKeyOf(row)
    else -> when (row) {
        BtSettingsRow.Enabled, BtSettingsRow.Dht, BtSettingsRow.Upnp, BtSettingsRow.PortStart, BtSettingsRow.PortEnd, BtSettingsRow.MseMode -> K_GENERAL
        BtSettingsRow.CustomTrackers -> K_TRACKER_LIST
        BtSettingsRow.TrackerSub, BtSettingsRow.TrackerSubUrls, BtSettingsRow.TrackerSubStatus -> K_TRACKER_SUB
        BtSettingsRow.SeedEnabled, BtSettingsRow.SeedMaxActive, BtSettingsRow.AutoReseed -> K_SEED_MAIN
        else -> K_SEED_LIMITS
    }
}

@StringRes
private fun tabTitle(tab: BtSettingsTab): Int = when (tab) {
    BtSettingsTab.General -> R.string.settingsTabGeneral
    BtSettingsTab.Tracker -> R.string.settingsTabTracker
    BtSettingsTab.Seeding -> R.string.settingsTabSeeding
    BtSettingsTab.Guard -> R.string.btGuardTab
}

@StringRes
private fun titleOf(row: BtSettingsRow): Int = when (row) {
    BtSettingsRow.Enabled -> R.string.btEnabled
    BtSettingsRow.Dht -> R.string.btEnableDht
    BtSettingsRow.Upnp -> R.string.btEnableUpnp
    BtSettingsRow.Utp -> R.string.btEnableUtp
    BtSettingsRow.PortStart -> R.string.btListenPortStart
    BtSettingsRow.PortEnd -> R.string.btListenPortEnd
    BtSettingsRow.MseMode -> R.string.btMseMode
    BtSettingsRow.CustomTrackers -> R.string.btTrackerList
    BtSettingsRow.TrackerSub -> R.string.btTrackerSub
    BtSettingsRow.TrackerSubUrls -> R.string.btTrackerSubUrls
    BtSettingsRow.TrackerSubStatus -> R.string.btTrackerSubUpdateNow
    BtSettingsRow.SeedEnabled -> R.string.btSeedEnabled
    BtSettingsRow.SeedMaxActive -> R.string.btSeedMaxActive
    BtSettingsRow.AutoReseed -> R.string.btAutoReseed
    BtSettingsRow.SeedRatio -> R.string.btSeedRatioLimit
    BtSettingsRow.SeedPostRatio -> R.string.btSeedPostRatioLimit
    BtSettingsRow.SeedTimeLimit -> R.string.btSeedTimeLimit
    BtSettingsRow.SeedInactiveTimeLimit -> R.string.btSeedInactiveTimeLimit
    BtSettingsRow.SeedOperator -> R.string.btSeedConditionsOperator
    BtSettingsRow.SeedThenAction -> R.string.btSeedThenAction
    BtSettingsRow.GuardEnabled -> R.string.btGuardEnabled
    BtSettingsRow.GuardRuleExcessive -> R.string.btGuardRuleExcessive
    BtSettingsRow.GuardRuleRerequest -> R.string.btGuardRuleRerequest
    BtSettingsRow.GuardRuleRewind -> R.string.btGuardRuleRewind
    BtSettingsRow.GuardRuleLag -> R.string.btGuardRuleLag
    BtSettingsRow.GuardRuleFingerprint -> R.string.btGuardRuleFingerprint
    BtSettingsRow.GuardExcessRatio -> R.string.btGuardExcessRatio
    BtSettingsRow.GuardMinTorrent -> R.string.btGuardMinTorrent
    BtSettingsRow.GuardRewindTolerance -> R.string.btGuardRewindTolerance
    BtSettingsRow.GuardThrottle -> R.string.btGuardThrottleKib
    BtSettingsRow.GuardBanBase -> R.string.btGuardBanBaseHours
    BtSettingsRow.GuardBanMax -> R.string.btGuardBanMaxDays
    BtSettingsRow.GuardDownloadingMax -> R.string.btGuardDownloadingMaxAction
    BtSettingsRow.GuardSkipPrivate -> R.string.btGuardSkipPrivate
    BtSettingsRow.GuardSkipLan -> R.string.btGuardSkipLan
    BtSettingsRow.GuardIpv6Prefix -> R.string.btGuardIpv6Prefix
    BtSettingsRow.GuardCloudEnabled -> R.string.btGuardCloudEnabled
    BtSettingsRow.GuardCloudMode -> R.string.btGuardCloudMode
    BtSettingsRow.GuardFilterUrls -> R.string.btGuardFilterUrls
    BtSettingsRow.GuardReportEnabled -> R.string.btGuardReportEnabled
}

/** 说明文案；时长行的「说明」只用于搜索（命中单位标题），页面不渲染。 */
@StringRes
private fun detailOf(row: BtSettingsRow): Int? = when (row) {
    BtSettingsRow.Enabled -> R.string.btEnabledDesc
    BtSettingsRow.Dht -> R.string.btEnableDhtDesc
    BtSettingsRow.Upnp -> R.string.btEnableUpnpDesc
    BtSettingsRow.Utp -> R.string.btEnableUtpDesc
    BtSettingsRow.PortStart -> R.string.btListenPortDesc
    BtSettingsRow.MseMode -> R.string.btMseModeDesc
    BtSettingsRow.CustomTrackers -> R.string.btTrackerListDesc
    BtSettingsRow.TrackerSub -> R.string.btTrackerSubDesc
    BtSettingsRow.TrackerSubUrls -> R.string.btTrackerSubUrlsDesc
    BtSettingsRow.SeedEnabled -> R.string.btSeedEnabledDesc
    BtSettingsRow.SeedMaxActive -> R.string.btSeedMaxActiveDesc
    BtSettingsRow.AutoReseed -> R.string.btAutoReseedDesc
    BtSettingsRow.SeedTimeLimit -> R.string.btSeedTimeLimitUnit
    BtSettingsRow.SeedInactiveTimeLimit -> R.string.btSeedInactiveTimeLimitUnit
    BtSettingsRow.GuardEnabled -> R.string.btGuardEnabledDesc
    BtSettingsRow.GuardRuleExcessive -> R.string.btGuardRuleExcessiveDesc
    BtSettingsRow.GuardRuleRerequest -> R.string.btGuardRuleRerequestDesc
    BtSettingsRow.GuardRuleRewind -> R.string.btGuardRuleRewindDesc
    BtSettingsRow.GuardRuleLag -> R.string.btGuardRuleLagDesc
    BtSettingsRow.GuardRuleFingerprint -> R.string.btGuardRuleFingerprintDesc
    BtSettingsRow.GuardExcessRatio -> R.string.btGuardExcessRatioDesc
    BtSettingsRow.GuardMinTorrent -> R.string.btGuardMinTorrentDesc
    BtSettingsRow.GuardRewindTolerance -> R.string.btGuardRewindToleranceDesc
    BtSettingsRow.GuardThrottle -> R.string.btGuardThrottleKibDesc
    BtSettingsRow.GuardBanBase -> R.string.btGuardBanBaseHoursDesc
    BtSettingsRow.GuardDownloadingMax -> R.string.btGuardDownloadingMaxActionDesc
    BtSettingsRow.GuardSkipPrivate -> R.string.btGuardSkipPrivateDesc
    BtSettingsRow.GuardSkipLan -> R.string.btGuardSkipLanDesc
    BtSettingsRow.GuardIpv6Prefix -> R.string.btGuardIpv6PrefixDesc
    BtSettingsRow.GuardCloudEnabled -> R.string.btGuardCloudEnabledDesc
    BtSettingsRow.GuardFilterUrls -> R.string.btGuardFilterUrlsDesc
    BtSettingsRow.GuardReportEnabled -> R.string.btGuardReportEnabledDesc
    else -> null
}

/**
 * S6 · BitTorrent 设置（常规 · Tracker · 做种）。行与 GPUI `sections/bt.rs` 同序同键同范围；
 * 全部写入走 ConfigEditor。做种关闭时其余做种行整体隐藏；订阅关闭时订阅地址置灰；断线只读。
 */
@Composable
internal fun BtPage() {
    val nav = LocalNavigator.current
    val focus = LocalSettingsFocus.current
    var picker by remember { mutableStateOf<PickerSpec?>(null) }
    val ctx = rememberSettingsCtx { picker = it }
    val form = ctx.form
    val gate = rememberFlowInGate()
    var tabIndex by rememberSaveable { mutableIntStateOf(0) }
    val visibleTabs = BtSettingsRow.visibleTabs(form)
    val tab = BtSettingsTab.entries[tabIndex].takeIf { it in visibleTabs } ?: BtSettingsTab.General

    // 防吸血：数据层、操作与浮层（只在防吸血页签可见且连接就绪时加载）
    val container = LocalAppContainer.current
    val scope = rememberCoroutineScope()
    val model = remember(container.session, scope) { BtGuardModel(container.session, scope) }
    val guardActions = rememberGuardActions(model, scope)
    var viewIndex by rememberSaveable { mutableIntStateOf(0) }
    val guardView = GuardView.entries[viewIndex]
    var guardSheet by remember { mutableStateOf<GuardSheet?>(null) }
    val host = hostState()
    val sessionRaw by remember { derivedStateOf { host.value.sections[HostSection.agentSession] } }
    val loggedIn = remember(sessionRaw) { AgentSessionDto.fromJson(Json.parseOrNull(sessionRaw)) != null }
    val guardActive = tab == BtSettingsTab.Guard && form.isLoaded && !ctx.readOnly
    // 可见子视图登记（只有当前可见的列表才随通知刷新）
    LaunchedEffect(model, guardActive, guardView) {
        model.markVisible(
            entries = guardActive && guardView == GuardView.Bans,
            events = guardActive && guardView == GuardView.Events,
        )
    }
    // 通知订阅绑定生命周期：回到前台（STARTED）时先全量刷新一次，再随 btGuardChanged 增量刷新
    val lifecycleOwner = LocalLifecycleOwner.current
    LaunchedEffect(model, guardActive, lifecycleOwner) {
        if (!guardActive) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            model.onChanged()
            container.store.notices.collect { if (it.name == HostNotice.btGuardChanged) model.onChanged() }
        }
    }
    val bansVisible = guardActive && guardView == GuardView.Bans
    LaunchedEffect(model, bansVisible, model.layer, model.rule, model.query) {
        if (bansVisible) {
            delay(SEARCH_DEBOUNCE_MS)
            model.reloadEntries()
        }
    }
    val eventsVisible = guardActive && guardView == GuardView.Events
    LaunchedEffect(model, eventsVisible, model.eventKind, model.eventRule) {
        if (eventsVisible) model.reloadEvents()
    }

    // 搜索命中：先切到目标行所在页签（防吸血行落在设置子视图）
    LaunchedEffect(focus.highlight) {
        focus.highlight?.let { BtSettingsRow.tabForRowId(it) }?.let {
            tabIndex = it.ordinal
            if (it == BtSettingsTab.Guard) viewIndex = GuardView.Settings.ordinal
        }
    }

    // 起始端口被本机改到结束端口之上时，把结束端口一并抬到起始端口（只响应本机的待提交修改）
    val start = form.int(BtSettingsRow.PortStart.configKey, 6881)
    LaunchedEffect(start) {
        val endKey = BtSettingsRow.PortEnd.configKey
        if (!ctx.readOnly && ctx.editor.optimistic[BtSettingsRow.PortStart.configKey] != null &&
            ctx.form.int(endKey, 6891) < start
        ) {
            ctx.editor.set(endKey, start.toString())
        }
    }

    val tabOptions = visibleTabs.map { SegOption(it, str(tabTitle(it))) }
    val legend = str(R.string.settingsSyncLegend)

    Box(Modifier.fillMaxSize()) {
        SettingsPageFrame(title = str(R.string.settingsCatBt), onBack = { nav.pop() }) {
            var index = 0
            if (ctx.readOnly) {
                flowItem(index++, gate, "banner") {
                    FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            if (visibleTabs.size > 1) {
                flowItem(index++, gate, "tabs") {
                    FluxSegmented(options = tabOptions, selected = tab, onSelect = { tabIndex = it.ordinal })
                }
            }
            if (!form.isLoaded) {
                flowItem(index++, gate, "loading") {
                    Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { FluxSpinner() }
                }
            } else {
                when (tab) {
                    BtSettingsTab.General -> {
                        flowItem(index++, gate, K_GENERAL) { GeneralSection(ctx) }
                    }
                    BtSettingsTab.Tracker -> {
                        if (BtSettingsRow.CustomTrackers.isVisible(form)) {
                            flowItem(index++, gate, K_TRACKER_LIST) {
                                GlassSection {
                                    linesRow(
                                        ctx, BtSettingsRow.CustomTrackers.id, BtSettingsRow.CustomTrackers.configKey,
                                        R.string.btTrackerList, R.string.btTrackerListDesc, R.string.btTrackerPlaceholder,
                                    )
                                }
                            }
                        }
                        if (BtSettingsRow.visible(BtSettingsTab.Tracker, form).any { it != BtSettingsRow.CustomTrackers }) {
                            flowItem(index++, gate, K_TRACKER_SUB) { TrackerSubSection(ctx) }
                        }
                    }
                    BtSettingsTab.Seeding -> {
                        val seeding = BtSettingsRow.visible(BtSettingsTab.Seeding, form)
                        if (seeding.isNotEmpty()) {
                            flowItem(index++, gate, K_SEED_MAIN) { SeedMainSection(ctx) }
                        }
                        if (seeding.any {
                                it != BtSettingsRow.SeedEnabled && it != BtSettingsRow.SeedMaxActive && it != BtSettingsRow.AutoReseed
                            }
                        ) {
                            flowItem(index++, gate, K_SEED_LIMITS) { SeedLimitsSection(ctx) }
                        }
                    }
                    BtSettingsTab.Guard -> {
                        index = btGuardItems(
                            start = index, gate = gate, ctx = ctx, model = model, actions = guardActions,
                            view = guardView, loggedIn = loggedIn,
                            onView = { viewIndex = it.ordinal },
                            openSheet = { guardSheet = it },
                        )
                    }
                }
                flowItem(index++, gate, "legend") { FluxSectionFoot(legend) }
            }
        }
        PickerSheetHost(picker = picker, onDismiss = { picker = null })
        GuardSheetHost(sheet = guardSheet, onDismiss = { guardSheet = null }, model = model)
    }
}

@Composable
private fun GeneralSection(ctx: SettingsCtx) {
    val form = ctx.form
    val start = form.int(BtSettingsRow.PortStart.configKey, 6881)
    val end = form.int(BtSettingsRow.PortEnd.configKey, 6891)
    val template = stringResource(R.string.mobileAdjustedTo)
    val invalid = str(R.string.btPortRangeInvalid)
    val mseLabels = mapOf(
        "disabled" to (str(R.string.btMseModeDisabled) to str(R.string.btMseModeDisabledDesc)),
        "enabled" to (str(R.string.btMseModeEnabled) to str(R.string.btMseModeEnabledDesc)),
        "forced" to (str(R.string.btMseModeForced) to str(R.string.btMseModeForcedDesc)),
    )
    val footer = buildString {
        append(str(R.string.btSettingsRestartHint))
        if (ctx.isLocalHost) append("\n\n").append(str(R.string.mobileBtAndroidFootnote))
    }
    GlassSection(footer = footer) {
        if (BtSettingsRow.Enabled.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Enabled.configKey, R.string.btEnabled, R.string.btEnabledDesc, id = BtSettingsRow.Enabled.id)
        }
        if (BtSettingsRow.Dht.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Dht.configKey, R.string.btEnableDht, R.string.btEnableDhtDesc, id = BtSettingsRow.Dht.id)
        }
        if (BtSettingsRow.Upnp.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Upnp.configKey, R.string.btEnableUpnp, R.string.btEnableUpnpDesc, id = BtSettingsRow.Upnp.id)
        }
        if (BtSettingsRow.Utp.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Utp.configKey, R.string.btEnableUtp, R.string.btEnableUtpDesc, id = BtSettingsRow.Utp.id)
        }
        if (BtSettingsRow.PortStart.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.PortStart.configKey, R.string.btListenPortStart, R.string.btListenPortDesc,
                range = BtPortRange.bounds.first.toLong()..BtPortRange.bounds.last.toLong(), default = 6881L,
                id = BtSettingsRow.PortStart.id,
            )
        }
        if (BtSettingsRow.PortEnd.isVisible(form)) {
            val endKey = BtSettingsRow.PortEnd.configKey
            row {
                SettingRowBox(ctx, BtSettingsRow.PortEnd.id, endKey) {
                    FluxFieldRow(title = str(R.string.btListenPortEnd), cloud = ctx.synced(endKey)) {
                        FluxNumberField(
                            value = end.toLong(),
                            onValueChange = { ctx.editor.set(endKey, it.toString()) },
                            range = start.coerceIn(BtPortRange.bounds).toLong()..BtPortRange.bounds.last.toLong(),
                            enabled = !ctx.readOnly,
                            adjustedHint = { template.fill("n" to it) },
                        )
                    }
                }
            }
            if (!BtPortRange.isValid(start, end)) {
                custom(padded = true) {
                    FluxText(invalid, style = FluxTheme.type.sm, color = FluxTheme.colors.amberText)
                }
            }
        }
        if (BtSettingsRow.MseMode.isVisible(form)) {
            settingChoice(ctx, BtSettingsRow.MseMode.configKey, R.string.btMseMode, R.string.btMseModeDesc, id = BtSettingsRow.MseMode.id) {
                BtMseModeOptions(mseLabels)
            }
        }
    }
}

private fun BtMseModeOptions(labels: Map<String, Pair<String, String>>): List<SelectOption<String>> =
    BtMseMode.all.map { wire ->
        val (title, hint) = labels[wire] ?: (wire to "")
        SelectOption(wire, title, hint = hint.ifEmpty { null })
    }

@Composable
private fun TrackerSubSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (BtSettingsRow.TrackerSub.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.TrackerSub.configKey, R.string.btTrackerSub, R.string.btTrackerSubDesc,
                id = BtSettingsRow.TrackerSub.id,
            )
        }
        if (BtSettingsRow.TrackerSubUrls.isVisible(form)) {
            linesRow(
                ctx, BtSettingsRow.TrackerSubUrls.id, BtSettingsRow.TrackerSubUrls.configKey,
                R.string.btTrackerSubUrls, R.string.btTrackerSubUrlsDesc, R.string.btTrackerSubPlaceholder,
                enabled = form.bool(BtSettingsRow.TrackerSub.configKey),
            )
        }
        if (BtSettingsRow.TrackerSubStatus.isVisible(form)) subscriptionStatusRow(ctx, SubscriptionKind.BtTrackers)
    }
}

@Composable
private fun SeedMainSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (BtSettingsRow.SeedEnabled.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.SeedEnabled.configKey, R.string.btSeedEnabled, R.string.btSeedEnabledDesc,
                id = BtSettingsRow.SeedEnabled.id,
            )
        }
        if (BtSettingsRow.SeedMaxActive.isVisible(form)) {
            settingStepper(
                ctx, BtSettingsRow.SeedMaxActive.configKey, R.string.btSeedMaxActive, R.string.btSeedMaxActiveDesc,
                range = 0..1_000_000, default = 0, zeroLabel = R.string.btSeedMaxActiveUnlimited,
                id = BtSettingsRow.SeedMaxActive.id,
            )
        }
        if (BtSettingsRow.AutoReseed.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.AutoReseed.configKey, R.string.btAutoReseed, R.string.btAutoReseedDesc,
                id = BtSettingsRow.AutoReseed.id,
            )
        }
    }
}

@Composable
private fun SeedLimitsSection(ctx: SettingsCtx) {
    val form = ctx.form
    val orLabel = str(R.string.btSeedOperatorOr)
    val andLabel = str(R.string.btSeedOperatorAnd)
    val stopLabel = str(R.string.btSeedStopSeeding)
    val deleteLabel = str(R.string.btSeedDeleteTask)
    val deleteFilesLabel = str(R.string.btSeedDeleteTaskAndFiles)
    GlassSection(title = str(R.string.btSeedLimitsTitle)) {
        if (BtSettingsRow.SeedRatio.isVisible(form)) {
            ratioRow(ctx, BtSettingsRow.SeedRatio.id, BtSettingsRow.SeedRatio.configKey, R.string.btSeedRatioLimit)
        }
        if (BtSettingsRow.SeedPostRatio.isVisible(form)) {
            ratioRow(ctx, BtSettingsRow.SeedPostRatio.id, BtSettingsRow.SeedPostRatio.configKey, R.string.btSeedPostRatioLimit)
        }
        val timeUnitKey = BtSettingsRow.SeedTimeLimit.unitKey
        if (BtSettingsRow.SeedTimeLimit.isVisible(form) && timeUnitKey != null) {
            durationRow(
                ctx, BtSettingsRow.SeedTimeLimit.id, BtSettingsRow.SeedTimeLimit.configKey, timeUnitKey,
                R.string.btSeedTimeLimit, R.string.btSeedTimeLimitUnit,
            )
        }
        val idleUnitKey = BtSettingsRow.SeedInactiveTimeLimit.unitKey
        if (BtSettingsRow.SeedInactiveTimeLimit.isVisible(form) && idleUnitKey != null) {
            durationRow(
                ctx, BtSettingsRow.SeedInactiveTimeLimit.id, BtSettingsRow.SeedInactiveTimeLimit.configKey, idleUnitKey,
                R.string.btSeedInactiveTimeLimit, R.string.btSeedInactiveTimeLimitUnit,
            )
        }
        if (BtSettingsRow.SeedOperator.isVisible(form)) {
            val opKey = BtSettingsRow.SeedOperator.configKey
            row {
                SettingRowBox(ctx, BtSettingsRow.SeedOperator.id, opKey) {
                    FluxFieldRow(title = str(R.string.btSeedConditionsOperator), cloud = ctx.synced(opKey)) {
                        FluxSegmented(
                            options = listOf(SegOption("or", orLabel), SegOption("and", andLabel)),
                            selected = ctx.form.string(opKey, SettingsCatalog.defaultWire(opKey)),
                            onSelect = { ctx.editor.set(opKey, it) },
                        )
                    }
                }
            }
        }
        if (BtSettingsRow.SeedThenAction.isVisible(form)) {
            settingChoice(
                ctx, BtSettingsRow.SeedThenAction.configKey, R.string.btSeedThenAction, null,
                id = BtSettingsRow.SeedThenAction.id,
            ) {
                listOf(
                    SelectOption("stop", stopLabel),
                    SelectOption("delete", deleteLabel),
                    SelectOption("delete_files", deleteFilesLabel),
                )
            }
        }
    }
}

// ───────────────────────────── 搜索索引 / 读数 ─────────────────────────────

/** 设置搜索索引：每个页签的所有可见行（与页面渲染共用 [BtSettingsRow.isVisible]）。 */
internal fun btSearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> =
    BtSettingsRow.entries.filter { it.isVisible(ctx.form) }.map { row ->
        ctx.entry(
            id = row.id,
            page = SettingsPage.Bt,
            itemKey = itemKeyOf(row),
            title = titleOf(row),
            detail = detailOf(row),
            breadcrumb = ctx.crumb(R.string.settingsCatBt, tabTitle(row.tab)),
            icon = FluxIcons.Magnet,
        )
    }

/** 设置首页该分类行的读数（null = 使用分类说明文案）。 */
internal fun btReadout(ctx: SettingsSearchContext): String? =
    BtReadout.text(ctx.form, ctx.str(R.string.btSeedingTitle))
