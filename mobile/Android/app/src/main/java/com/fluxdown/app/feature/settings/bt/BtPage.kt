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
import com.fluxdown.app.feature.settings.settingText
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.AgentSessionDto
import com.fluxdown.core.protocol.BtPortMappingView
import com.fluxdown.core.protocol.BtPortSegment
import com.fluxdown.core.protocol.BtPortSegmentState
import com.fluxdown.core.protocol.BtPortStatus
import com.fluxdown.core.protocol.BtSettingsGroup
import com.fluxdown.core.protocol.BtUtpCongestion
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.callJson
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import kotlinx.coroutines.CancellationException
import com.fluxdown.core.protocol.HostNotice
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import kotlinx.coroutines.delay
import com.fluxdown.core.protocol.BtMseMode
import com.fluxdown.core.protocol.BtPieceOrder
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

// LazyColumn 项 key（搜索定位用，与 [btSearchEntries] 共用）：一组一个 key
private fun groupKey(group: BtSettingsGroup): String = when (group) {
    BtSettingsGroup.Basic -> "general"
    BtSettingsGroup.NewTasks -> "new-tasks"
    BtSettingsGroup.Listen -> "listen"
    BtSettingsGroup.Transport -> "transport"
    BtSettingsGroup.Sources -> "sources"
    BtSettingsGroup.Limits -> "limits"
    BtSettingsGroup.I2p -> "i2p"
    BtSettingsGroup.TrackerList -> "tracker-list"
    BtSettingsGroup.TrackerSub -> "tracker-sub"
    BtSettingsGroup.Announce -> "announce"
    BtSettingsGroup.SeedMain -> "seed-main"
    BtSettingsGroup.SeedLimits -> "seed-limits"
    BtSettingsGroup.Guard -> "guard"
}

/** 封禁名单搜索框输入后的防抖，避免逐字请求主机。 */
private const val SEARCH_DEBOUNCE_MS = 150L

private fun itemKeyOf(row: BtSettingsRow): String = when (row.tab) {
    BtSettingsTab.Guard -> guardItemKeyOf(row)
    else -> groupKey(row.group)
}

@StringRes
private fun tabTitle(tab: BtSettingsTab): Int = when (tab) {
    BtSettingsTab.General -> R.string.settingsTabGeneral
    BtSettingsTab.Connection -> R.string.settingsTabConnection
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
    BtSettingsRow.UploadSlots -> R.string.btUploadSlots
    BtSettingsRow.PortMappingStatus -> R.string.btPortMappingStatus
    BtSettingsRow.UtpCongestion -> R.string.btUtpCongestion
    BtSettingsRow.Lsd -> R.string.btEnableLsd
    BtSettingsRow.Webseed -> R.string.btEnableWebseed
    BtSettingsRow.MaxConnections -> R.string.btMaxConnections
    BtSettingsRow.MaxConnectionsPerTorrent -> R.string.btMaxConnectionsPerTorrent
    BtSettingsRow.I2pEnabled -> R.string.btI2pEnabled
    BtSettingsRow.I2pSamHost -> R.string.btI2pSamHost
    BtSettingsRow.I2pSamPort -> R.string.btI2pSamPort
    BtSettingsRow.I2pMixed -> R.string.btI2pMixed
    BtSettingsRow.AnnounceAllTiers -> R.string.btAnnounceAllTiers
    BtSettingsRow.AnnounceAllTrackers -> R.string.btAnnounceAllTrackers
    BtSettingsRow.PieceOrder -> R.string.btPieceOrder
    BtSettingsRow.FirstLastPiecesFirst -> R.string.btFirstLastPiecesFirstDefault
    BtSettingsRow.SelectionAutoDefault -> R.string.btSelectionAutoDefault
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
    BtSettingsRow.UploadSlots -> R.string.btUploadSlotsDesc
    BtSettingsRow.UtpCongestion -> R.string.btUtpCongestionDesc
    BtSettingsRow.Lsd -> R.string.btEnableLsdDesc
    BtSettingsRow.Webseed -> R.string.btEnableWebseedDesc
    BtSettingsRow.MaxConnections -> R.string.btMaxConnectionsDesc
    BtSettingsRow.MaxConnectionsPerTorrent -> R.string.btMaxConnectionsPerTorrentDesc
    BtSettingsRow.I2pEnabled -> R.string.btI2pEnabledDesc
    BtSettingsRow.I2pMixed -> R.string.btI2pMixedDesc
    BtSettingsRow.AnnounceAllTiers -> R.string.btAnnounceAllTiersDesc
    BtSettingsRow.AnnounceAllTrackers -> R.string.btAnnounceAllTrackersDesc
    BtSettingsRow.PieceOrder -> R.string.btPieceOrderDesc
    BtSettingsRow.FirstLastPiecesFirst -> R.string.btFirstLastPiecesFirstDefaultDesc
    BtSettingsRow.SelectionAutoDefault -> R.string.btSelectionAutoDefaultDesc
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

    // 端口映射状态：仅「连接」页签可见、映射开启且页面在前台时每 5 秒轮询（同 GPUI `sync_port_status_poll`）
    var portStatus by remember { mutableStateOf<BtPortStatus?>(null) }
    val portStatusVisible = tab == BtSettingsTab.Connection && BtSettingsRow.PortMappingStatus.isVisible(form)
    LaunchedEffect(container.session, portStatusVisible, lifecycleOwner) {
        portStatus = null
        if (!portStatusVisible) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                try {
                    portStatus = BtPortStatus.fromJson(container.session.callJson(HostMethod.daemonBtPortStatus))
                } catch (e: CancellationException) {
                    throw e
                } catch (_: HostException) {
                    // 旧 daemon 无此方法 / 断线：行内留空（同 GPUI），本次可见期间不再轮询
                    portStatus = null
                    return@repeatOnLifecycle
                }
                delay(BtPortMappingView.POLL_INTERVAL_MS)
            }
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
                        flowItem(index++, gate, groupKey(BtSettingsGroup.Basic)) { BasicSection(ctx) }
                        if (BtSettingsRow.PieceOrder.isVisible(form) || BtSettingsRow.FirstLastPiecesFirst.isVisible(form) ||
                            BtSettingsRow.SelectionAutoDefault.isVisible(form)
                        ) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.NewTasks)) { NewTasksSection(ctx) }
                        }
                    }
                    BtSettingsTab.Connection -> {
                        val groups = BtSettingsRow.visible(BtSettingsTab.Connection, form).map { it.group }.distinct()
                        for (group in groups) {
                            flowItem(index++, gate, groupKey(group)) {
                                when (group) {
                                    BtSettingsGroup.Listen -> ListenSection(ctx, portStatus)
                                    BtSettingsGroup.Transport -> TransportSection(ctx)
                                    BtSettingsGroup.Sources -> SourcesSection(ctx)
                                    BtSettingsGroup.Limits -> LimitsSection(ctx)
                                    else -> I2pSection(ctx)
                                }
                            }
                        }
                    }
                    BtSettingsTab.Tracker -> {
                        if (BtSettingsRow.CustomTrackers.isVisible(form)) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.TrackerList)) {
                                GlassSection {
                                    linesRow(
                                        ctx, BtSettingsRow.CustomTrackers.id, BtSettingsRow.CustomTrackers.configKey,
                                        R.string.btTrackerList, R.string.btTrackerListDesc, R.string.btTrackerPlaceholder,
                                    )
                                }
                            }
                        }
                        val trackerRows = BtSettingsRow.visible(BtSettingsTab.Tracker, form)
                        if (trackerRows.any { it.group == BtSettingsGroup.TrackerSub }) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.TrackerSub)) { TrackerSubSection(ctx) }
                        }
                        if (trackerRows.any { it.group == BtSettingsGroup.Announce }) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.Announce)) { AnnounceSection(ctx) }
                        }
                    }
                    BtSettingsTab.Seeding -> {
                        val seeding = BtSettingsRow.visible(BtSettingsTab.Seeding, form)
                        if (seeding.isNotEmpty()) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.SeedMain)) { SeedMainSection(ctx) }
                        }
                        if (seeding.any {
                                it != BtSettingsRow.SeedEnabled && it != BtSettingsRow.SeedMaxActive && it != BtSettingsRow.AutoReseed
                            }
                        ) {
                            flowItem(index++, gate, groupKey(BtSettingsGroup.SeedLimits)) { SeedLimitsSection(ctx) }
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
private fun BasicSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (BtSettingsRow.Enabled.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Enabled.configKey, R.string.btEnabled, R.string.btEnabledDesc, id = BtSettingsRow.Enabled.id)
        }
    }
}

/** 监听端口与端口映射。本页签除「连接上限」外的改动都会重建 BT 会话，提示挂在第一组（同 GPUI）。 */
@Composable
private fun ListenSection(ctx: SettingsCtx, portStatus: BtPortStatus?) {
    val form = ctx.form
    val start = form.int(BtSettingsRow.PortStart.configKey, 6881)
    val end = form.int(BtSettingsRow.PortEnd.configKey, 6891)
    val template = stringResource(R.string.mobileAdjustedTo)
    val invalid = str(R.string.btPortRangeInvalid)
    val footer = buildString {
        append(str(R.string.btSettingsRestartHint))
        if (ctx.isLocalHost) append("\n\n").append(str(R.string.mobileBtAndroidFootnote))
    }
    GlassSection(title = str(R.string.btSectionListen), footer = footer) {
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
        if (BtSettingsRow.Upnp.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Upnp.configKey, R.string.btEnableUpnp, R.string.btEnableUpnpDesc, id = BtSettingsRow.Upnp.id)
        }
        if (BtSettingsRow.PortMappingStatus.isVisible(form)) {
            row { PortMappingStatusRow(ctx, portStatus) }
        }
    }
}

/**
 * 端口映射状态行（只读）：每段一行；失败原因压成单行并截断，长按以 Toast 显示全文。
 * 尚无快照（首轮在途 / 旧 daemon 无此方法）时副标题留空（同 GPUI）。
 */
@Composable
private fun PortMappingStatusRow(ctx: SettingsCtx, status: BtPortStatus?) {
    val overlays = LocalFluxOverlays.current
    val segments = status?.let(BtPortMappingView::segments).orEmpty()
    val idle = str(R.string.btPortMappingIdle)
    val pending = str(R.string.btPortMappingPending)
    val failed = str(R.string.btPortMappingFailed)
    val mappedTpl = str(R.string.btPortMappingMapped)
    val ipTpl = str(R.string.btPortMappingExternalIp)
    val lines = ArrayList<String>(segments.size)
    val fulls = ArrayList<String>()
    for (segment in segments) {
        lines += when (segment) {
            BtPortSegment.Idle -> idle
            is BtPortSegment.ExternalIp -> ipTpl.fill("ip" to segment.ip)
            is BtPortSegment.Mapping -> {
                val text = when (val s = segment.state) {
                    is BtPortSegmentState.Mapped -> mappedTpl.fill("method" to s.method)
                    BtPortSegmentState.Pending -> pending
                    is BtPortSegmentState.Failed -> if (s.message.isEmpty()) {
                        failed
                    } else {
                        fulls += "${segment.port} $failed: ${s.message}"
                        "$failed: ${BtPortMappingView.singleLine(s.message)}"
                    }
                    is BtPortSegmentState.Other -> s.raw
                }
                "${segment.port} $text"
            }
        }
    }
    SettingRowBox(ctx, BtSettingsRow.PortMappingStatus.id, null) {
        FluxListRow(
            title = str(R.string.btPortMappingStatus),
            subtitle = lines.takeIf { it.isNotEmpty() }?.joinToString("\n"),
            onLongClick = if (fulls.isEmpty()) null else ({ overlays.toast(fulls.joinToString("\n"), durationMs = 8000L) }),
        )
    }
}

@Composable
private fun TransportSection(ctx: SettingsCtx) {
    val form = ctx.form
    val mseLabels = mapOf(
        "disabled" to (str(R.string.btMseModeDisabled) to str(R.string.btMseModeDisabledDesc)),
        "enabled" to (str(R.string.btMseModeEnabled) to str(R.string.btMseModeEnabledDesc)),
        "forced" to (str(R.string.btMseModeForced) to str(R.string.btMseModeForcedDesc)),
    )
    val utpLabels = mapOf(
        "ledbat" to str(R.string.btUtpCongestionLedbat),
        "cubic" to str(R.string.btUtpCongestionCubic),
    )
    GlassSection(title = str(R.string.btSectionTransport)) {
        if (BtSettingsRow.Utp.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Utp.configKey, R.string.btEnableUtp, R.string.btEnableUtpDesc, id = BtSettingsRow.Utp.id)
        }
        if (BtSettingsRow.UtpCongestion.isVisible(form)) {
            settingChoice(
                ctx, BtSettingsRow.UtpCongestion.configKey, R.string.btUtpCongestion, R.string.btUtpCongestionDesc,
                id = BtSettingsRow.UtpCongestion.id,
            ) {
                BtUtpCongestion.all.map { wire -> SelectOption(wire, utpLabels[wire] ?: wire) }
            }
        }
        if (BtSettingsRow.MseMode.isVisible(form)) {
            settingChoice(ctx, BtSettingsRow.MseMode.configKey, R.string.btMseMode, R.string.btMseModeDesc, id = BtSettingsRow.MseMode.id) {
                BtMseModeOptions(mseLabels)
            }
        }
        if (BtSettingsRow.UploadSlots.isVisible(form)) {
            settingStepper(
                ctx, BtSettingsRow.UploadSlots.configKey, R.string.btUploadSlots, R.string.btUploadSlotsDesc,
                range = 0..512, default = 0, zeroLabel = R.string.auto,
                id = BtSettingsRow.UploadSlots.id,
            )
        }
    }
}

@Composable
private fun SourcesSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btSectionSources)) {
        if (BtSettingsRow.Dht.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Dht.configKey, R.string.btEnableDht, R.string.btEnableDhtDesc, id = BtSettingsRow.Dht.id)
        }
        if (BtSettingsRow.Lsd.isVisible(form)) {
            settingSwitch(ctx, BtSettingsRow.Lsd.configKey, R.string.btEnableLsd, R.string.btEnableLsdDesc, id = BtSettingsRow.Lsd.id)
        }
        if (BtSettingsRow.Webseed.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.Webseed.configKey, R.string.btEnableWebseed, R.string.btEnableWebseedDesc,
                id = BtSettingsRow.Webseed.id,
            )
        }
    }
}

/** 连接数上限：热生效，不重建 BT 会话。 */
@Composable
private fun LimitsSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btSectionLimits)) {
        if (BtSettingsRow.MaxConnections.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.MaxConnections.configKey, R.string.btMaxConnections, R.string.btMaxConnectionsDesc,
                range = 0L..65_535L, default = 0L, id = BtSettingsRow.MaxConnections.id,
            )
        }
        if (BtSettingsRow.MaxConnectionsPerTorrent.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.MaxConnectionsPerTorrent.configKey, R.string.btMaxConnectionsPerTorrent,
                R.string.btMaxConnectionsPerTorrentDesc,
                range = 1L..4096L, default = 512L, id = BtSettingsRow.MaxConnectionsPerTorrent.id,
            )
        }
    }
}

/** I2P：SAM 桥地址与混合模式只在启用后显示。 */
@Composable
private fun I2pSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btSectionI2p)) {
        if (BtSettingsRow.I2pEnabled.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.I2pEnabled.configKey, R.string.btI2pEnabled, R.string.btI2pEnabledDesc,
                id = BtSettingsRow.I2pEnabled.id,
            )
        }
        if (BtSettingsRow.I2pSamHost.isVisible(form)) {
            settingText(ctx, BtSettingsRow.I2pSamHost.configKey, R.string.btI2pSamHost, null, id = BtSettingsRow.I2pSamHost.id)
        }
        if (BtSettingsRow.I2pSamPort.isVisible(form)) {
            settingNumber(
                ctx, BtSettingsRow.I2pSamPort.configKey, R.string.btI2pSamPort, null,
                range = 1L..65_535L, default = 7656L, id = BtSettingsRow.I2pSamPort.id,
            )
        }
        if (BtSettingsRow.I2pMixed.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.I2pMixed.configKey, R.string.btI2pMixed, R.string.btI2pMixedDesc,
                id = BtSettingsRow.I2pMixed.id,
            )
        }
    }
}

/** tracker 汇报策略（BEP 12 的两个开关）。 */
@Composable
private fun AnnounceSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection(title = str(R.string.btSectionAnnounce)) {
        if (BtSettingsRow.AnnounceAllTiers.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.AnnounceAllTiers.configKey, R.string.btAnnounceAllTiers, R.string.btAnnounceAllTiersDesc,
                id = BtSettingsRow.AnnounceAllTiers.id,
            )
        }
        if (BtSettingsRow.AnnounceAllTrackers.isVisible(form)) {
            settingSwitch(
                ctx, BtSettingsRow.AnnounceAllTrackers.configKey, R.string.btAnnounceAllTrackers,
                R.string.btAnnounceAllTrackersDesc, id = BtSettingsRow.AnnounceAllTrackers.id,
            )
        }
    }
}

@Composable
private fun NewTasksSection(ctx: SettingsCtx) {
    val form = ctx.form
    val pieceLabels = mapOf(
        "rarest" to (str(R.string.btPieceOrderRarest) to str(R.string.btPieceOrderRarestDesc)),
        "sequential" to (str(R.string.btPieceOrderSequential) to str(R.string.btPieceOrderSequentialDesc)),
    )
    GlassSection(title = str(R.string.btNewTasksSection)) {
        if (BtSettingsRow.PieceOrder.isVisible(form)) {
            settingChoice(
                ctx, BtSettingsRow.PieceOrder.configKey, R.string.btPieceOrder, R.string.btPieceOrderDesc,
                id = BtSettingsRow.PieceOrder.id,
            ) {
                BtPieceOrderOptions(pieceLabels)
            }
        }
        if (BtSettingsRow.FirstLastPiecesFirst.isVisible(form)) {
            settingSwitch(
                ctx,
                BtSettingsRow.FirstLastPiecesFirst.configKey,
                R.string.btFirstLastPiecesFirstDefault,
                R.string.btFirstLastPiecesFirstDefaultDesc,
                id = BtSettingsRow.FirstLastPiecesFirst.id,
            )
        }
        if (BtSettingsRow.SelectionAutoDefault.isVisible(form)) {
            settingSwitch(
                ctx,
                BtSettingsRow.SelectionAutoDefault.configKey,
                R.string.btSelectionAutoDefault,
                R.string.btSelectionAutoDefaultDesc,
                id = BtSettingsRow.SelectionAutoDefault.id,
            )
        }
    }
}

private fun BtPieceOrderOptions(labels: Map<String, Pair<String, String>>): List<SelectOption<String>> =
    BtPieceOrder.all.map { wire ->
        val (title, hint) = labels[wire] ?: (wire to "")
        SelectOption(wire, title, hint = hint.ifEmpty { null })
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
