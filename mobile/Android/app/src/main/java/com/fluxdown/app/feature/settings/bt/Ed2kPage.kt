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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.LocalSettingsFocus
import com.fluxdown.app.feature.settings.PickerSheetHost
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.feature.settings.SettingsEntry
import com.fluxdown.app.feature.settings.SettingsPageFrame
import com.fluxdown.app.feature.settings.SettingsSearchContext
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.rememberSettingsCtx
import com.fluxdown.app.feature.settings.settingNumber
import com.fluxdown.app.feature.settings.settingSwitch
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.core.protocol.Ed2kReadout
import com.fluxdown.core.protocol.Ed2kSettingsRow
import com.fluxdown.core.protocol.Ed2kSettingsTab
import com.fluxdown.core.protocol.SubscriptionKind
import com.fluxdown.core.protocol.SubscriptionListFormat
import com.fluxdown.fluxui.controls.FluxSectionFoot
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind

private const val K_GENERAL = "general"
private const val K_SERVER_LIST = "server-list"
private const val K_SERVER_SUB = "server-sub"
private const val K_KAD_NODES = "kad-nodes"

private fun itemKeyOf(row: Ed2kSettingsRow): String = when (row) {
    Ed2kSettingsRow.Kad, Ed2kSettingsRow.Upnp, Ed2kSettingsRow.ListenPort, Ed2kSettingsRow.IdCheck -> K_GENERAL
    Ed2kSettingsRow.ServerList -> K_SERVER_LIST
    Ed2kSettingsRow.ServerSub, Ed2kSettingsRow.ServerSubUrls, Ed2kSettingsRow.ServerSubStatus -> K_SERVER_SUB
    Ed2kSettingsRow.NodesSubUrls, Ed2kSettingsRow.NodesSubStatus -> K_KAD_NODES
}

@StringRes
private fun tabTitle(tab: Ed2kSettingsTab): Int = when (tab) {
    Ed2kSettingsTab.General -> R.string.settingsTabGeneral
    Ed2kSettingsTab.Servers -> R.string.settingsTabServers
    Ed2kSettingsTab.Kad -> R.string.settingsTabKadNodes
}

@StringRes
private fun titleOf(row: Ed2kSettingsRow): Int = when (row) {
    Ed2kSettingsRow.Kad -> R.string.ed2kEnableKad
    Ed2kSettingsRow.Upnp -> R.string.ed2kEnableUpnp
    Ed2kSettingsRow.ListenPort -> R.string.ed2kListenPort
    Ed2kSettingsRow.IdCheck -> R.string.ed2kIdCheck
    Ed2kSettingsRow.ServerList -> R.string.ed2kServerList
    Ed2kSettingsRow.ServerSub -> R.string.ed2kServerSub
    Ed2kSettingsRow.ServerSubUrls -> R.string.ed2kServerSubUrls
    Ed2kSettingsRow.ServerSubStatus -> R.string.ed2kServerSubUpdateNow
    Ed2kSettingsRow.NodesSubUrls -> R.string.ed2kNodesSubUrls
    Ed2kSettingsRow.NodesSubStatus -> R.string.ed2kNodesSubUpdateNow
}

@StringRes
private fun detailOf(row: Ed2kSettingsRow): Int? = when (row) {
    Ed2kSettingsRow.Kad -> R.string.ed2kEnableKadDesc
    Ed2kSettingsRow.Upnp -> R.string.ed2kEnableUpnpDesc
    Ed2kSettingsRow.ListenPort -> R.string.ed2kListenPortDesc
    Ed2kSettingsRow.IdCheck -> R.string.ed2kIdCheckDesc
    Ed2kSettingsRow.ServerList -> R.string.ed2kServerListDesc
    Ed2kSettingsRow.ServerSub -> R.string.ed2kServerSubDesc
    Ed2kSettingsRow.ServerSubUrls -> R.string.ed2kServerSubUrlsDesc
    Ed2kSettingsRow.ServerSubStatus -> null
    Ed2kSettingsRow.NodesSubUrls -> R.string.ed2kNodesSubUrlsDesc
    Ed2kSettingsRow.NodesSubStatus -> null
}

/**
 * S7 · eD2K 设置（常规 · 服务器 · Kad 节点）。行与 GPUI `sections/ed2k.rs` 同序同键同范围。
 * `ed2k_server_list` 存逗号分隔的 `host:port`，编辑区每行一条，写回时按条目去空白并忽略大小写去重；
 * 两个订阅地址列表（服务器 / nodes.dat）均为换行分隔。
 */
@Composable
internal fun Ed2kPage() {
    val nav = LocalNavigator.current
    val focus = LocalSettingsFocus.current
    var picker by remember { mutableStateOf<PickerSpec?>(null) }
    val ctx = rememberSettingsCtx { picker = it }
    val form = ctx.form
    val gate = rememberFlowInGate()
    var tabIndex by rememberSaveable { mutableIntStateOf(0) }
    val tab = Ed2kSettingsTab.entries[tabIndex]

    // 搜索命中：先切到目标行所在页签
    LaunchedEffect(focus.highlight) {
        focus.highlight?.let { Ed2kSettingsRow.tabForRowId(it) }?.let { tabIndex = it.ordinal }
    }

    val tabOptions = Ed2kSettingsTab.entries.map { SegOption(it, str(tabTitle(it))) }
    val legend = str(R.string.settingsSyncLegend)

    Box(Modifier.fillMaxSize()) {
        SettingsPageFrame(title = str(R.string.settingsCatEd2k), onBack = { nav.pop() }) {
            var index = 0
            if (ctx.readOnly) {
                flowItem(index++, gate, "banner") {
                    FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            flowItem(index++, gate, "tabs") {
                FluxSegmented(options = tabOptions, selected = tab, onSelect = { tabIndex = it.ordinal })
            }
            if (!form.isLoaded) {
                flowItem(index++, gate, "loading") {
                    Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { FluxSpinner() }
                }
            } else {
                when (tab) {
                    Ed2kSettingsTab.General -> flowItem(index++, gate, K_GENERAL) { GeneralSection(ctx) }
                    Ed2kSettingsTab.Servers -> {
                        if (Ed2kSettingsRow.ServerList.isVisible(form)) {
                            flowItem(index++, gate, K_SERVER_LIST) {
                                GlassSection {
                                    linesRow(
                                        ctx, Ed2kSettingsRow.ServerList.id, Ed2kSettingsRow.ServerList.configKey,
                                        R.string.ed2kServerList, R.string.ed2kServerListDesc, R.string.ed2kServerPlaceholder,
                                        format = SubscriptionListFormat.Comma,
                                    )
                                }
                            }
                        }
                        if (Ed2kSettingsRow.visible(Ed2kSettingsTab.Servers, form).any { it != Ed2kSettingsRow.ServerList }) {
                            flowItem(index++, gate, K_SERVER_SUB) { ServerSubSection(ctx) }
                        }
                    }
                    Ed2kSettingsTab.Kad -> {
                        if (Ed2kSettingsRow.visible(Ed2kSettingsTab.Kad, form).isNotEmpty()) {
                            flowItem(index++, gate, K_KAD_NODES) { KadNodesSection(ctx) }
                        }
                    }
                }
                flowItem(index++, gate, "legend") { FluxSectionFoot(legend) }
            }
        }
        PickerSheetHost(picker = picker, onDismiss = { picker = null })
    }
}

@Composable
private fun GeneralSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (Ed2kSettingsRow.Kad.isVisible(form)) {
            settingSwitch(
                ctx, Ed2kSettingsRow.Kad.configKey, R.string.ed2kEnableKad, R.string.ed2kEnableKadDesc,
                id = Ed2kSettingsRow.Kad.id,
            )
        }
        if (Ed2kSettingsRow.Upnp.isVisible(form)) {
            settingSwitch(
                ctx, Ed2kSettingsRow.Upnp.configKey, R.string.ed2kEnableUpnp, R.string.ed2kEnableUpnpDesc,
                id = Ed2kSettingsRow.Upnp.id,
            )
        }
        if (Ed2kSettingsRow.ListenPort.isVisible(form)) {
            settingNumber(
                ctx, Ed2kSettingsRow.ListenPort.configKey, R.string.ed2kListenPort, R.string.ed2kListenPortDesc,
                range = 0L..65_535L, default = 0L, id = Ed2kSettingsRow.ListenPort.id,
            )
        }
        if (Ed2kSettingsRow.IdCheck.isVisible(form)) ed2kIdCheckRow(ctx, Ed2kSettingsRow.IdCheck.id)
    }
}

@Composable
private fun ServerSubSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (Ed2kSettingsRow.ServerSub.isVisible(form)) {
            settingSwitch(
                ctx, Ed2kSettingsRow.ServerSub.configKey, R.string.ed2kServerSub, R.string.ed2kServerSubDesc,
                id = Ed2kSettingsRow.ServerSub.id,
            )
        }
        if (Ed2kSettingsRow.ServerSubUrls.isVisible(form)) {
            linesRow(
                ctx, Ed2kSettingsRow.ServerSubUrls.id, Ed2kSettingsRow.ServerSubUrls.configKey,
                R.string.ed2kServerSubUrls, R.string.ed2kServerSubUrlsDesc, R.string.ed2kServerSubPlaceholder,
                enabled = form.bool(Ed2kSettingsRow.ServerSub.configKey),
            )
        }
        if (Ed2kSettingsRow.ServerSubStatus.isVisible(form)) subscriptionStatusRow(ctx, SubscriptionKind.Ed2kServers)
    }
}

@Composable
private fun KadNodesSection(ctx: SettingsCtx) {
    val form = ctx.form
    GlassSection {
        if (Ed2kSettingsRow.NodesSubUrls.isVisible(form)) {
            linesRow(
                ctx, Ed2kSettingsRow.NodesSubUrls.id, Ed2kSettingsRow.NodesSubUrls.configKey,
                R.string.ed2kNodesSubUrls, R.string.ed2kNodesSubUrlsDesc, R.string.ed2kNodesSubPlaceholder,
                enabled = form.bool(Ed2kSettingsRow.Kad.configKey),
            )
        }
        if (Ed2kSettingsRow.NodesSubStatus.isVisible(form)) subscriptionStatusRow(ctx, SubscriptionKind.Ed2kNodes)
    }
}

// ───────────────────────────── 搜索索引 / 读数 ─────────────────────────────

/** 设置搜索索引：每个页签的所有可见行（与页面渲染共用 [Ed2kSettingsRow.isVisible]）。 */
internal fun ed2kSearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> =
    Ed2kSettingsRow.entries.filter { it.isVisible(ctx.form) }.map { row ->
        ctx.entry(
            id = row.id,
            page = SettingsPage.Ed2k,
            itemKey = itemKeyOf(row),
            title = titleOf(row),
            detail = detailOf(row),
            breadcrumb = ctx.crumb(R.string.settingsCatEd2k, tabTitle(row.tab)),
            icon = FluxIcons.Server,
        )
    }

/** 设置首页该分类行的读数（null = 使用分类说明文案）。 */
internal fun ed2kReadout(ctx: SettingsSearchContext): String? =
    Ed2kReadout.text(ctx.form) { ctx.str(R.string.ed2kServerCount, "n" to it) }
