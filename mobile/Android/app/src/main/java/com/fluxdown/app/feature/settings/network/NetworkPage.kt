package com.fluxdown.app.feature.settings.network

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
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
import com.fluxdown.app.feature.settings.settingSwitch
import com.fluxdown.app.feature.settings.settingText
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.protocol.BtProxyHint
import com.fluxdown.core.protocol.NetworkPortInput
import com.fluxdown.core.protocol.NetworkProxyMode
import com.fluxdown.core.protocol.NetworkProxyTest
import com.fluxdown.core.protocol.NetworkRow
import com.fluxdown.core.protocol.SettingsCatalog
import com.fluxdown.core.protocol.ProxyRule
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxFieldRow
import com.fluxdown.fluxui.controls.FluxKeyValue
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.feedback.FluxSpinner
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

@StringRes
private fun NetworkProxyMode.titleRes(): Int = when (this) {
    NetworkProxyMode.None -> R.string.proxyModeNone
    NetworkProxyMode.System -> R.string.proxyModeSystem
    NetworkProxyMode.Manual -> R.string.proxyModeManual
    NetworkProxyMode.Auto -> R.string.proxyModeAuto
}

@StringRes
private fun NetworkProxyMode.descRes(): Int = when (this) {
    NetworkProxyMode.None -> R.string.proxyModeNoneDesc
    NetworkProxyMode.System -> R.string.proxyModeSystemDesc
    NetworkProxyMode.Manual -> R.string.proxyModeManualDesc
    NetworkProxyMode.Auto -> R.string.proxyModeAutoDesc
}

private val ProxyTypes = listOf("http", "https", "socks4", "socks5")

/** 代理模式在引擎里会让多 CDN 并发失效；自动模式保留直连，不冲突。 */
private fun NetworkProxyMode.isProxied(): Boolean = this == NetworkProxyMode.System || this == NetworkProxyMode.Manual

/**
 * S8 · 网络与代理：代理模式（none / system / manual / auto）、系统代理检测、手动配置、连通性测试、已保存的网站凭据。
 * `proxy_*` 键是主机配置、不参与云同步（不显示 ☁︎）。写入全部经 ConfigEditor；断线（只读）时主机侧控件置灰。
 */
@Composable
internal fun NetworkPage() {
    val nav = LocalNavigator.current
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val haptics = FluxTheme.haptics
    val hostState = hostState()
    val hostRef by container.host.collectAsStateWithLifecycle()
    val isLive by remember { derivedStateOf { hostState.value.connection == Connection.Live } }
    var picker by remember { mutableStateOf<PickerSpec?>(null) }
    val ctx = rememberSettingsCtx { picker = it }
    val proxy = remember { NetworkProxyModel() }
    val siteAuth = remember { SiteAuthModel() }
    var sheet by remember { mutableStateOf<SiteAuthTarget?>(null) }
    val rulesModel = remember(container) { ProxyRulesModel(container) }
    var ruleEditor by remember { mutableStateOf<ProxyRuleTarget?>(null) }
    val gate = rememberFlowInGate()
    val scope = rememberCoroutineScope()
    val errorText: (com.fluxdown.core.host.HostException) -> String = { actions.errorText(it) }

    val form = ctx.form
    val mode = NetworkProxyMode.of(form.value("proxy_mode"))
    val manualRequest = NetworkProxyTest.manual(form)
    val systemRequest = NetworkProxyTest.system(proxy.system)
    val hostId = hostRef.id
    val rulesRaw = form.string("proxy_rules")
    val rules = remember(rulesRaw) { ProxyRule.parseList(rulesRaw) }
    val enabledRules = rules.count { it.enabled }
    val remoteHostName = if (ctx.isLocalHost) null else hostRef.displayName

    val cancelText = str(R.string.cancel)
    val cdnTitle = str(R.string.proxyCdnMultiConfirmTitle)
    val cdnDesc = str(R.string.proxyCdnMultiConfirmDesc)
    val cdnEnable = str(R.string.proxyCdnMultiConfirmEnable)
    val tlsHint = str(R.string.proxyTestTlsEndpointHint)
    val clearTitle = str(R.string.settingsSiteAuthClearAll)
    val deleteTitle = str(R.string.settingsSiteAuthDelete)

    // 两路测试参数各自变化后只作废自己的结果；主机变化两路都作废。
    LaunchedEffect(manualRequest) { proxy.resetTest(NetworkProxyModel.TestSource.Manual) }
    LaunchedEffect(systemRequest) { proxy.resetTest(NetworkProxyModel.TestSource.System) }
    LaunchedEffect(hostId) {
        proxy.resetTests()
        proxy.resetDetection()
        rulesModel.reset()
    }
    // 系统代理检测：切到系统模式 / 切换主机 / 连接恢复时重新检测。
    LaunchedEffect(hostId, isLive, mode == NetworkProxyMode.System) {
        if (isLive && mode == NetworkProxyMode.System) proxy.detect(container.session, errorText)
    }
    LaunchedEffect(hostId, isLive) { siteAuth.load(container.session, hostId, errorText) }

    fun selectMode(next: NetworkProxyMode) {
        if (next == mode) return
        if (next.isProxied() && !mode.isProxied() && form.bool("cdn_multi_enabled")) {
            overlays.showDialog(
                FluxDialogSpec(
                    title = cdnTitle,
                    message = cdnDesc,
                    icon = FluxIcons.TriangleAlert,
                    buttons = listOf(
                        FluxDialogButton(cancelText, FluxDialogButtonStyle.Secondary),
                        FluxDialogButton(cdnEnable, FluxDialogButtonStyle.Primary) {
                            ctx.editor.setNow(mapOf("cdn_multi_enabled" to "false", "proxy_mode" to next.wire))
                        },
                    ),
                ),
            )
        } else {
            ctx.editor.set("proxy_mode", next.wire)
        }
    }

    fun reportFailure(e: com.fluxdown.core.host.HostException) {
        haptics.reject()
        overlays.toast(actions.errorText(e), FluxToastKind.Error)
    }

    fun deleteEntry(site: String, user: String) {
        val session = container.session
        overlays.showDialog(
            FluxDialogSpec(
                title = deleteTitle,
                message = if (user.isEmpty()) site else "$site · $user",
                icon = FluxIcons.Trash2,
                buttons = listOf(
                    FluxDialogButton(cancelText, FluxDialogButtonStyle.Secondary),
                    FluxDialogButton(deleteTitle, FluxDialogButtonStyle.Destructive) {
                        scope.launch {
                            try {
                                siteAuth.delete(site, session)
                                haptics.tick()
                            } catch (e: com.fluxdown.core.host.HostException) {
                                reportFailure(e)
                            }
                        }
                    },
                ),
            ),
        )
    }

    val clearMessage = str(R.string.mobileSiteAuthClearConfirm, "n" to siteAuth.entries.size)
    fun clearAll() {
        val session = container.session
        overlays.showDialog(
            FluxDialogSpec(
                title = clearTitle,
                message = clearMessage,
                icon = FluxIcons.TriangleAlert,
                buttons = listOf(
                    FluxDialogButton(cancelText, FluxDialogButtonStyle.Secondary),
                    FluxDialogButton(clearTitle, FluxDialogButtonStyle.Destructive) {
                        scope.launch {
                            try {
                                siteAuth.clearAll(session)
                                haptics.tick()
                            } catch (e: com.fluxdown.core.host.HostException) {
                                reportFailure(e)
                            }
                        }
                    },
                ),
            ),
        )
    }

    Box(Modifier.fillMaxSize()) {
        SettingsPageFrame(title = str(R.string.settingsCatProxy), onBack = { nav.pop() }) {
            var index = 0
            if (ctx.readOnly) {
                flowItem(index++, gate, "banner") {
                    FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            if (form.isLoaded) {
                flowItem(index++, gate, "mode") {
                    GlassSection(footer = str(R.string.proxyBtNote)) {
                        row {
                            SettingRowBox(ctx, NetworkRow.Mode.rowId, "proxy_mode") {
                                val title = str(R.string.proxySettings)
                                val options = NetworkProxyMode.entries.map {
                                    SelectOption(it.wire, str(it.titleRes()), hint = str(it.descRes()))
                                }
                                FluxListRow(
                                    title = title,
                                    subtitle = str(R.string.proxySettingsDesc),
                                    value = str(mode.titleRes()),
                                    chevron = true,
                                    enabled = !ctx.readOnly,
                                    onClick = {
                                        ctx.openPicker(
                                            PickerSpec(title, options, mode.wire) { selectMode(NetworkProxyMode.of(it)) },
                                        )
                                    },
                                )
                            }
                        }
                        if (enabledRules > 0) {
                            custom(padded = true) {
                                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
                                    FluxIcon(FluxIcons.Info, null, size = 18.dp, tint = FluxTheme.colors.inkMuted)
                                    FluxText(
                                        str(R.string.proxyRulesActiveHint, "count" to enabledRules),
                                        style = FluxTheme.type.sm,
                                        color = FluxTheme.colors.inkMuted,
                                        modifier = Modifier.weight(1f),
                                    )
                                }
                            }
                        }
                    }
                }
                when (mode) {
                    NetworkProxyMode.None -> Unit
                    NetworkProxyMode.Auto -> flowItem(index++, gate, "auto") {
                        GlassSection {
                            custom(padded = true) {
                                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
                                    FluxIcon(FluxIcons.Info, null, size = 18.dp, tint = FluxTheme.colors.inkMuted)
                                    FluxText(
                                        str(R.string.proxyModeAutoDesc),
                                        style = FluxTheme.type.sm,
                                        color = FluxTheme.colors.inkMuted,
                                        modifier = Modifier.weight(1f),
                                    )
                                }
                            }
                        }
                    }
                    NetworkProxyMode.System -> flowItem(index++, gate, "system") {
                        SystemSection(ctx, proxy, systemRequest != null, onRetry = {
                            val session = container.session
                            scope.launch { proxy.detect(session, errorText) }
                        }, onTest = {
                            runTest(scope, container.session, proxy, NetworkProxyModel.TestSource.System, systemRequest, errorText, tlsHint)
                        })
                    }
                    NetworkProxyMode.Manual -> Unit
                }
                // 手动字段在非手动模式下仍被使用（自动备选、站点规则、任务级手动代理），配置加载后始终可编辑。
                flowItem(index++, gate, "manual") {
                    ManualSection(
                        ctx = ctx,
                        footer = str(
                            if (mode == NetworkProxyMode.Manual) R.string.proxyModeManualDesc else R.string.proxyManualKeptDesc,
                        ),
                        test = proxy.manualTest,
                        onTest = {
                            runTest(scope, container.session, proxy, NetworkProxyModel.TestSource.Manual, manualRequest, errorText, tlsHint)
                        },
                    )
                }
                flowItem(index++, gate, "btProxy") { BtProxySection(ctx) }
                // 站点规则在任何模式下都生效（包括「不使用代理」和「自动」）。
                flowItem(index++, gate, "rules") {
                    ProxyRulesSection(
                        ctx = ctx,
                        model = rulesModel,
                        rules = rules,
                        mode = mode,
                        rulesRaw = rulesRaw,
                        onEdit = { rule ->
                            val now = System.currentTimeMillis()
                            ruleEditor = if (rule == null) {
                                ProxyRuleTarget(ProxyRule(id = "pr_$now"), isNew = true, nonce = now)
                            } else {
                                ProxyRuleTarget(rule, isNew = false, nonce = System.nanoTime())
                            }
                        },
                        onFailure = ::reportFailure,
                        errorText = errorText,
                    )
                }
            } else {
                flowItem(index++, gate, "loading") {
                    GlassSection {
                        row {
                            val loading = str(R.string.mobileLoading)
                            FluxListRow(
                                title = loading,
                                trailing = { FluxSpinner(size = 18.dp, label = loading) },
                            )
                        }
                    }
                }
            }
            flowItem(index++, gate, "siteAuth") {
                SiteAuthSection(
                    ctx = ctx,
                    model = siteAuth,
                    remoteHostName = remoteHostName,
                    onEdit = { sheet = it },
                    onDelete = { deleteEntry(it.site, it.user) },
                    onClear = ::clearAll,
                    onReload = {
                        val session = container.session
                        scope.launch { siteAuth.load(session, hostId, errorText) }
                    },
                )
            }
        }
        SiteAuthEditSheet(
            target = sheet,
            model = siteAuth,
            onDismiss = { sheet = null },
            onSaved = { haptics.tick() },
        )
        ProxyRuleEditorSheet(
            target = ruleEditor,
            ctx = ctx,
            model = rulesModel,
            rules = rules,
            errorText = errorText,
            onDismiss = { ruleEditor = null },
            onSaved = { haptics.tick() },
        )
        PickerSheetHost(picker = picker, onDismiss = { picker = null })
    }
}

private fun runTest(
    scope: kotlinx.coroutines.CoroutineScope,
    session: com.fluxdown.core.host.HostSession,
    proxy: NetworkProxyModel,
    source: NetworkProxyModel.TestSource,
    request: com.fluxdown.core.protocol.ProxyTestRequest?,
    errorText: (com.fluxdown.core.host.HostException) -> String,
    tlsHint: String,
) {
    if (request == null) return
    scope.launch { proxy.runTest(source, request, session, errorText, tlsHint) }
}

// ───────────────────────────── 手动代理服务器 ─────────────────────────────

@Composable
private fun ManualSection(
    ctx: SettingsCtx,
    footer: String,
    test: NetworkProxyModel.Test,
    onTest: () -> Unit,
) {
    val form = ctx.form
    GlassSection(title = str(R.string.proxyModeManual), footer = footer) {
        row {
            SettingRowBox(ctx, NetworkRow.Type.rowId, "proxy_type") {
                FluxFieldRow(title = str(R.string.proxyType)) {
                    FluxSegmented(
                        options = ProxyTypes.map { SegOption(it, it.uppercase()) },
                        selected = form.string("proxy_type", "http"),
                        onSelect = { ctx.editor.set("proxy_type", it) },
                    )
                }
            }
        }
        settingText(
            ctx, "proxy_host", R.string.proxyHost, null, R.string.proxyHostPlaceholder,
            id = NetworkRow.Host.rowId,
        )
        row { SettingRowBox(ctx, NetworkRow.Port.rowId, "proxy_port") { PortField(ctx) } }
        settingText(
            ctx, "proxy_username", R.string.proxyUsername, null, R.string.proxyUsernamePlaceholder,
            id = NetworkRow.Username.rowId, mono = false,
        )
        settingText(
            ctx, "proxy_password", R.string.proxyPassword, null, R.string.proxyPasswordPlaceholder,
            id = NetworkRow.Password.rowId, secret = true,
        )
        settingText(
            ctx, "proxy_no_list", R.string.proxyNoList, R.string.proxyNoListDesc,
            R.string.proxyNoListPlaceholder, id = NetworkRow.NoList.rowId,
        )
        testRow(ctx, NetworkRow.Test.rowId, test, onTest)
    }
}

// ───────────────────────────── BitTorrent ─────────────────────────────

/** BitTorrent 走代理（GPUI `proxy.rs::bt_section`）：总开关；开启后「代理来源」「节点连接也走代理」+ 一条提示。 */
@Composable
private fun BtProxySection(ctx: SettingsCtx) {
    val form = ctx.form
    val hint = BtProxyHint.of(form)
    val hintText = hint?.let {
        str(
            when (it) {
                BtProxyHint.SystemNote -> R.string.btProxySystemNote
                BtProxyHint.NoServer -> R.string.btProxyNoServer
                BtProxyHint.UnsupportedType -> R.string.btProxyUnsupportedType
                BtProxyHint.NoUdp -> R.string.btProxyNoUdp
            },
        )
    }
    val sourceLabels = mapOf(
        "manual" to str(R.string.btProxySourceManual),
        "system" to str(R.string.btProxySourceSystem),
    )
    GlassSection(title = str(R.string.proxyBtSection)) {
        if (NetworkRow.BtProxyEnabled.isVisible(form)) {
            settingSwitch(
                ctx, "bt_proxy_enabled", R.string.btProxyEnabled, R.string.btProxyEnabledDesc,
                id = NetworkRow.BtProxyEnabled.rowId,
            )
        }
        if (NetworkRow.BtProxySource.isVisible(form)) {
            settingChoice(
                ctx, "bt_proxy_source", R.string.btProxySource, R.string.btProxySourceDesc,
                id = NetworkRow.BtProxySource.rowId,
            ) {
                SettingsCatalog.btProxySources.map { wire -> SelectOption(wire, sourceLabels[wire] ?: wire) }
            }
        }
        if (NetworkRow.BtProxyPeers.isVisible(form)) {
            settingSwitch(
                ctx, "bt_proxy_peers", R.string.btProxyPeers, R.string.btProxyPeersDesc,
                id = NetworkRow.BtProxyPeers.rowId,
            )
        }
        if (hintText != null) {
            custom(padded = true) {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
                    FluxIcon(FluxIcons.Info, null, size = 18.dp, tint = FluxTheme.colors.inkMuted)
                    FluxText(
                        hintText,
                        style = FluxTheme.type.sm,
                        color = FluxTheme.colors.inkMuted,
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

// ───────────────────────────── 系统代理 ─────────────────────────────

@Composable
private fun SystemSection(
    ctx: SettingsCtx,
    proxy: NetworkProxyModel,
    hasRequest: Boolean,
    onRetry: () -> Unit,
    onTest: () -> Unit,
) {
    val detection = proxy.detection
    val dto = proxy.system
    GlassSection(title = str(R.string.proxyModeSystem), footer = str(R.string.proxyModeSystemDesc)) {
        when (detection) {
            NetworkProxyModel.Detection.Idle, NetworkProxyModel.Detection.Detecting -> row {
                val detecting = str(R.string.proxySystemDetecting)
                FluxListRow(title = detecting, trailing = { FluxSpinner(size = 18.dp, label = detecting) })
            }
            is NetworkProxyModel.Detection.Detected -> row(hasIcon = true) {
                if (detection.dto.detected) {
                    FluxListRow(title = str(R.string.proxySystemDetected), icon = FluxIcons.CircleCheck)
                } else {
                    FluxListRow(title = str(R.string.proxySystemNotConfigured), icon = FluxIcons.Info)
                }
            }
            is NetworkProxyModel.Detection.Failed -> {
                row(hasIcon = true) {
                    FluxListRow(title = detection.message, icon = FluxIcons.CircleAlert, danger = true)
                }
                row(hasIcon = true) {
                    FluxActionRow(str(R.string.mobileRetry), onClick = onRetry, icon = FluxIcons.RefreshCw)
                }
            }
        }
        if (dto != null) {
            row { SettingRowBox(ctx, "network.system.type", null) { FluxKeyValue(str(R.string.proxyType), dto.proxyType.uppercase()) } }
            row { SettingRowBox(ctx, "network.system.host", null) { FluxKeyValue(str(R.string.proxyHost), dto.host, mono = true) } }
            row { SettingRowBox(ctx, "network.system.port", null) { FluxKeyValue(str(R.string.proxyPort), dto.port.toString(), mono = true) } }
            if (dto.noList.isNotEmpty()) {
                row { SettingRowBox(ctx, "network.system.noList", null) { FluxKeyValue(str(R.string.proxyNoList), dto.noList, mono = true) } }
            }
            if (hasRequest) testRow(ctx, "network.system.test", proxy.systemTest, onTest)
        }
    }
}

// ───────────────────────────── 测试连接 ─────────────────────────────

/** `proxyTestConnection`：慢 RPC——行内转圈 + 「测试中…」，页面其余部分保持可交互；结果行成功 / 失败着色，失败伴随 REJECT 触感。 */
private fun GlassSectionScope.testRow(
    ctx: SettingsCtx,
    rowId: String,
    test: NetworkProxyModel.Test,
    onRun: () -> Unit,
) {
    row(hasIcon = true) {
        SettingRowBox(ctx, rowId, null) {
            val haptics = FluxTheme.haptics
            val running = test == NetworkProxyModel.Test.Running
            LaunchedEffect(test) {
                when (test) {
                    is NetworkProxyModel.Test.Success -> haptics.tick()
                    is NetworkProxyModel.Test.Failure -> haptics.reject()
                    else -> Unit
                }
            }
            Column {
                FluxActionRow(
                    title = str(if (running) R.string.proxyTesting else R.string.proxyTestConnection),
                    onClick = onRun,
                    icon = FluxIcons.Network,
                    loading = running,
                    enabled = !ctx.readOnly,
                )
                val c = FluxTheme.colors
                when (test) {
                    is NetworkProxyModel.Test.Success -> ResultLine(
                        str(R.string.proxyTestSuccess, "ms" to test.latencyMs),
                        c.mintText,
                    )
                    is NetworkProxyModel.Test.Failure -> ResultLine(
                        str(R.string.proxyTestFailed, "error" to test.detail),
                        c.coralText,
                    )
                    NetworkProxyModel.Test.Idle, NetworkProxyModel.Test.Running -> Unit
                }
            }
        }
    }
}

@Composable
internal fun ResultLine(text: String, color: androidx.compose.ui.graphics.Color) {
    FluxText(
        text,
        style = FluxTheme.type.sm,
        color = color,
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
    )
}

// ───────────────────────────── 端口 ─────────────────────────────

/** `proxy_port`：文本；只接受 ASCII 数字，提交（失焦 / 完成 / 离开页面）时钳位到 1–65535，空 = 清空。 */
@Composable
private fun PortField(ctx: SettingsCtx) {
    val current = ctx.form.string("proxy_port")
    var draft by remember(current) { mutableStateOf(current) }
    var adjusted by remember { mutableStateOf<Int?>(null) }
    var hadFocus by remember { mutableStateOf(false) }
    val latestDraft by rememberUpdatedState(draft)
    val latestCurrent by rememberUpdatedState(current)
    val focusManager = LocalFocusManager.current

    fun commit() {
        val result = NetworkPortInput.commit(draft)
        draft = result.wire
        adjusted = result.adjusted
        if (result.wire != current) ctx.editor.set("proxy_port", result.wire)
    }

    LaunchedEffect(adjusted) {
        if (adjusted != null) {
            delay(3000)
            adjusted = null
        }
    }
    DisposableEffect(Unit) {
        onDispose {
            val wire = NetworkPortInput.commit(latestDraft).wire
            if (wire != latestCurrent) ctx.editor.set("proxy_port", wire)
        }
    }

    val hint = adjusted?.let { str(R.string.mobileAdjustedTo, "n" to it) }
    FluxFieldRow(title = str(R.string.proxyPort)) {
        FluxField(
            value = draft,
            onValueChange = { draft = NetworkPortInput.digits(it) },
            placeholder = str(R.string.proxyPortPlaceholder),
            warning = hint,
            mono = true,
            enabled = !ctx.readOnly,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = {
                commit()
                focusManager.clearFocus()
            }),
            onFocusChange = { focused ->
                if (focused) {
                    hadFocus = true
                } else if (hadFocus) {
                    hadFocus = false
                    commit()
                }
            },
        )
    }
}

// ───────────────────────────── 搜索索引 / 读数 ─────────────────────────────

@StringRes
private fun NetworkRow.titleRes(): Int = when (this) {
    NetworkRow.Mode -> R.string.proxySettings
    NetworkRow.Type -> R.string.proxyType
    NetworkRow.Host -> R.string.proxyHost
    NetworkRow.Port -> R.string.proxyPort
    NetworkRow.Username -> R.string.proxyUsername
    NetworkRow.Password -> R.string.proxyPassword
    NetworkRow.NoList -> R.string.proxyNoList
    NetworkRow.Test -> R.string.proxyTestConnection
    NetworkRow.BtProxyEnabled -> R.string.btProxyEnabled
    NetworkRow.BtProxySource -> R.string.btProxySource
    NetworkRow.BtProxyPeers -> R.string.btProxyPeers
    NetworkRow.Rules -> R.string.proxyRulesTitle
    NetworkRow.RulesAdd -> R.string.proxyRulesAdd
    NetworkRow.RulesTest -> R.string.proxyRouteTest
    NetworkRow.SiteAuth -> R.string.settingsSiteAuthTitle
    NetworkRow.SiteAuthAdd -> R.string.settingsSiteAuthAdd
    NetworkRow.SiteAuthClear -> R.string.settingsSiteAuthClearAll
}

@StringRes
private fun NetworkRow.detailRes(): Int? = when (this) {
    NetworkRow.Mode -> R.string.proxySettingsDesc
    NetworkRow.NoList -> R.string.proxyNoListDesc
    NetworkRow.BtProxyEnabled -> R.string.btProxyEnabledDesc
    NetworkRow.BtProxySource -> R.string.btProxySourceDesc
    NetworkRow.BtProxyPeers -> R.string.btProxyPeersDesc
    NetworkRow.Rules -> R.string.proxyRulesDesc
    NetworkRow.RulesTest -> R.string.proxyRouteTestDesc
    NetworkRow.SiteAuth -> R.string.settingsSiteAuthDesc
    else -> null
}

@StringRes
private fun NetworkRow.Group.titleRes(): Int = when (this) {
    NetworkRow.Group.Mode -> R.string.proxySettings
    NetworkRow.Group.Manual -> R.string.proxyModeManual
    NetworkRow.Group.BtProxy -> R.string.proxyBtSection
    NetworkRow.Group.Rules -> R.string.proxyRulesTitle
    NetworkRow.Group.SiteAuth -> R.string.settingsSiteAuthTitle
}

private fun NetworkRow.itemKey(): String = when (group) {
    NetworkRow.Group.Mode -> "mode"
    NetworkRow.Group.Manual -> "manual"
    NetworkRow.Group.BtProxy -> "btProxy"
    NetworkRow.Group.Rules -> "rules"
    NetworkRow.Group.SiteAuth -> "siteAuth"
}

/** 本页所有可见行：代理模式；已加载配置下的手动字段与测试按钮（与当前模式无关）；BitTorrent 分组；站点凭据（标题 / 添加 / 全部清除）。系统模式的测试按钮取决于异步检测结果，不进索引。 */
internal fun networkSearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> {
    return NetworkRow.visible(ctx.form).map { row ->
        ctx.entry(
            id = row.rowId,
            page = SettingsPage.Network,
            itemKey = row.itemKey(),
            title = row.titleRes(),
            detail = row.detailRes(),
            breadcrumb = if (row.titleRes() == row.group.titleRes()) {
                ctx.crumb(R.string.settingsCatProxy)
            } else {
                ctx.crumb(R.string.settingsCatProxy, row.group.titleRes())
            },
            icon = if (row.group == NetworkRow.Group.SiteAuth) FluxIcons.Key else FluxIcons.Globe,
        )
    }
}

/** 设置首页读数：当前代理模式（配置未加载时 null）。 */
internal fun networkReadout(ctx: SettingsSearchContext): String? {
    if (!ctx.form.isLoaded) return null
    val mode = ctx.str(NetworkProxyMode.of(ctx.form.value("proxy_mode")).titleRes())
    val rules = ProxyRule.enabledCount(ctx.form.value("proxy_rules"))
    return if (rules > 0) "$mode · ${ctx.str(R.string.proxyRulesCount, "count" to rules)}" else mode
}
