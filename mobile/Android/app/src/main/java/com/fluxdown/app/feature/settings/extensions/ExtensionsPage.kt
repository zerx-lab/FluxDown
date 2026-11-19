package com.fluxdown.app.feature.settings.extensions

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.LocalSettingsFocus
import com.fluxdown.app.feature.settings.SettingRowBox
import com.fluxdown.app.feature.settings.SettingsEntry
import com.fluxdown.app.feature.settings.SettingsPageFrame
import com.fluxdown.app.feature.settings.SettingsSearchContext
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.rememberLastNonNull
import com.fluxdown.app.feature.settings.PickerSheetHost
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.rememberSettingsCtx
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.app.ui.launchDocumentPicker
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.HostCapability
import com.fluxdown.core.protocol.MarketEntry
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.PluginResolverUrlConflict
import com.fluxdown.core.protocol.has
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.FluxTag
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxProgressLine
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxOverlayState
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle

internal data class ResolverUrlOverlap(
    val identity: String,
    val patterns: PluginResolverUrlConflict,
    val pending: Boolean,
)

@Composable
internal fun rememberResolverUrlConflicts(plugins: List<PluginDto>, identities: Set<String>? = null) =
    remember(plugins, identities) {
        val enabled = plugins.filter { it.enabled }.mapTo(HashSet()) { it.identity }
        plugins.flatMap { plugin ->
            plugin.resolverUrlConflicts.mapNotNull { conflict ->
                if (identities == null || plugin.identity in identities || conflict.otherIdentity in identities) {
                    ResolverUrlOverlap(
                        plugin.identity,
                        conflict,
                        !plugin.enabled || conflict.otherIdentity !in enabled,
                    )
                } else {
                    null
                }
            }
        }
    }

@Composable
internal fun ResolverUrlConflictSection(conflicts: List<ResolverUrlOverlap>) {
    if (conflicts.isEmpty()) return
    GlassSection(title = str(R.string.pluginResolverUrlConflictsTitle)) {
        if (conflicts.any { (identity, patterns, pending) -> identity != patterns.otherIdentity && !pending }) {
            row {
                FluxBanner(str(R.string.pluginResolverUrlConflictsHint), kind = FluxBannerKind.Warn)
            }
        }
        for ((identity, conflict, pending) in conflicts) {
            row {
                Column {
                    if (identity == conflict.otherIdentity) {
                        FluxText(str(R.string.pluginResolverUrlDuplicateHint))
                    } else if (pending) {
                        FluxText(str(R.string.pluginResolverUrlPendingHint))
                    }
                    FluxText("$identity: ${conflict.pattern}")
                    FluxText("${conflict.otherIdentity}: ${conflict.otherPattern}")
                }
            }
        }
    }
}

/**
 * S11 · 扩展：插件（已安装 / 市场 / 文件安装）与组件，**仅远端 `--server` 主机**。
 *
 * Android 本机引擎同 iOS：`native/mobile` 不启用 `plugins` feature，也没有可执行的外部组件，
 * 因此本机主机只显示一条说明，插件 / 市场 / 文件安装 / 组件整体隐藏。
 * 插件列表来自 `daemon.plugins` 分区；操作走通用通道 `daemon.plugin.*`。
 */
@Composable
internal fun ExtensionsPage() {
    val container = LocalAppContainer.current
    val host by container.host.collectAsStateWithLifecycle()
    if (host is HostRef.Local) LocalHostExtensionsNote() else ExtensionsContent()
}

/** 本机主机：说明插件在已连接的 FluxDown 主机上运行。 */
@Composable
private fun LocalHostExtensionsNote() {
    val nav = LocalNavigator.current
    SettingsPageFrame(title = str(R.string.settingsCatExtensions), onBack = { nav.pop() }) {
        item(key = "note") {
            FluxBanner(str(R.string.mobilePluginsRemoteOnlyNote), kind = FluxBannerKind.Info, icon = FluxIcons.Puzzle)
        }
    }
}

@Composable
private fun ExtensionsContent() {
    val nav = LocalNavigator.current
    val model = rememberExtensionsModel()
    val env = rememberExtensionsEnv()
    val overlays = LocalFluxOverlays.current
    val hostState = hostState()
    val readOnly by remember { derivedStateOf { hostState.value.isReadOnly } }
    val loaded by remember { derivedStateOf { hostState.value.info != null } }
    val hasPlugins by remember { derivedStateOf { hostState.value.has(HostCapability.daemonPlugins) } }
    val hasComponents by remember { derivedStateOf { hostState.value.has(HostCapability.daemonComponents) } }
    val plugins by rememberPlugins()
    val conflicts = rememberResolverUrlConflicts(plugins)
    var picker by remember { mutableStateOf<PickerSpec?>(null) }
    val ctx = rememberSettingsCtx { picker = it }
    val gate = rememberFlowInGate()
    val focus = LocalSettingsFocus.current
    var sheet by remember { mutableStateOf<PluginSheet?>(null) }

    val tab = when {
        !hasComponents -> ExtensionsModel.Tab.Plugins
        !hasPlugins -> ExtensionsModel.Tab.Components
        else -> model.tab
    }

    // 搜索定位到组件页的行：先切到对应页签，再由页面骨架滚动到该项。
    LaunchedEffect(focus.pendingItemKey) {
        val key = focus.pendingItemKey ?: return@LaunchedEffect
        if (key.startsWith("component.")) model.tab = ExtensionsModel.Tab.Components
        else if (key in PluginTabKeys) model.tab = ExtensionsModel.Tab.Plugins
    }
    LaunchedEffect(readOnly, hasPlugins) { if (hasPlugins) model.ensureMarketLoaded(env) }
    LaunchedEffect(readOnly, hasPlugins) { if (hasPlugins && !readOnly) model.refreshSiteDefaults() }
    PluginAutoDisabledEffect(env)

    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) model.installFile(env, uri)
    }
    val noFilePicker = str(R.string.mobileNoFilePicker)
    val installLabel = when (val phase = model.installPhase) {
        is ExtensionsModel.InstallPhase.Uploading ->
            str(R.string.webPluginUploading, "percent" to Math.round(phase.fraction * 100))
        ExtensionsModel.InstallPhase.Installing -> str(R.string.marketInstallingButton)
        null -> str(R.string.pluginInstallZipButton)
    }

    Box(Modifier.fillMaxSize()) {
        SettingsPageFrame(title = str(R.string.settingsCatExtensions), onBack = { nav.pop() }) {
            var index = 0
            if (readOnly) {
                flowItem(index++, gate, "banner") {
                    FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            if (loaded && !hasPlugins && !hasComponents) {
                flowItem(index++, gate, "unsupported") {
                    FluxBanner(str(R.string.settingsUnsupportedOnPlatform), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            if (hasPlugins && hasComponents) {
                flowItem(index++, gate, "tabs") {
                    FluxSegmented(
                        options = listOf(
                            SegOption(ExtensionsModel.Tab.Plugins, str(R.string.settingsCatPlugins)),
                            SegOption(ExtensionsModel.Tab.Components, str(R.string.settingsCatComponents)),
                        ),
                        selected = tab,
                        onSelect = { model.tab = it },
                    )
                }
            }
            if (tab == ExtensionsModel.Tab.Plugins && hasPlugins && conflicts.isNotEmpty()) {
                flowItem(index++, gate, "resolverUrlConflicts") { ResolverUrlConflictSection(conflicts) }
            }
            if (tab == ExtensionsModel.Tab.Plugins && hasPlugins) {
                flowItem(index++, gate, "plugins") {
                    SettingRowBox(ctx, "extensions.plugins", null) {
                        GlassSection(title = str(R.string.pluginsSectionTitle)) {
                            if (plugins.isEmpty()) {
                                row(hasIcon = true) {
                                    FluxListRow(title = str(R.string.pluginsEmpty), icon = FluxIcons.Puzzle)
                                }
                            }
                            for (plugin in plugins) {
                                row {
                                    PluginRow(
                                        plugin = plugin,
                                        update = model.updateFor(plugin),
                                        yanked = com.fluxdown.core.protocol.PluginMarket.installedVersionYanked(model.marketEntries, plugin),
                                        onClick = { nav.push(Route.Settings(SettingsPage.PluginDetail, plugin.identity)) },
                                        onLongClick = {
                                            showPluginActions(
                                                overlays, env, model, plugin,
                                                readOnly = readOnly,
                                                onSheet = { sheet = it },
                                            )
                                        },
                                    )
                                }
                            }
                        }
                    }
                }
                flowItem(index++, gate, "install") {
                    SettingRowBox(ctx, "extensions.install", null) {
                        GlassSection {
                            row(hasIcon = true) {
                                FluxActionRow(
                                    title = installLabel,
                                    icon = FluxIcons.FileUp,
                                    loading = model.installPhase != null,
                                    enabled = !readOnly && model.installPhase == null,
                                    onClick = { launchDocumentPicker(overlays, noFilePicker) { importer.launch(arrayOf("*/*")) } },
                                )
                            }
                            val phase = model.installPhase
                            if (phase is ExtensionsModel.InstallPhase.Uploading) {
                                custom(padded = true) { FluxProgressLine(phase.fraction) }
                            }
                        }
                    }
                }
                if (model.siteDefaults.isNotEmpty()) {
                    flowItem(index++, gate, "siteDefaults") {
                        SettingRowBox(ctx, "extensions.siteDefaults", null) {
                            SiteDefaultsSection(model, env, plugins, readOnly) { picker = it }
                        }
                    }
                }
                flowItem(index++, gate, "market") {
                    SettingRowBox(ctx, "extensions.market", null) {
                        GlassSection {
                            row(hasIcon = true) {
                                FluxListRow(
                                    title = str(R.string.marketSectionTitle),
                                    subtitle = str(R.string.marketSectionDesc),
                                    icon = FluxIcons.Store,
                                    iconTone = Tone.Accent,
                                    chevron = true,
                                    onClick = { nav.push(Route.Settings(SettingsPage.PluginMarket)) },
                                )
                            }
                        }
                    }
                }
            }
            if (tab == ExtensionsModel.Tab.Components && hasComponents) {
                componentItems(index, gate, ctx, model, env)
            }
        }
        PluginSheetHost(sheet = sheet, plugins = plugins, onDismiss = { sheet = null })
        PickerSheetHost(picker = picker, onDismiss = { picker = null })
        ExtensionsOverlays(model, env)
    }
}

/** 属于「插件」页签的行 key（搜索定位时据此切页签）。 */
private val PluginTabKeys = setOf("plugins", "install", "market", "siteDefaults")

/** 站点默认插件：每行 站点 + 候选插件选择器；自动选定（未确认）的条目带警示标签与「确认」按钮。 */
@Composable
private fun SiteDefaultsSection(
    model: ExtensionsModel,
    env: ExtensionsEnv,
    plugins: List<PluginDto>,
    readOnly: Boolean,
    openPicker: (PickerSpec) -> Unit,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val names = remember(plugins) { plugins.associate { it.identity to it.name.ifEmpty { it.identity } } }
    GlassSection(title = str(R.string.pluginSiteDefaultsTitle)) {
        custom(padded = true) {
            FluxText(str(R.string.pluginSiteDefaultsDesc), style = t.sm, color = c.inkMuted)
        }
        for (entry in model.siteDefaults) {
            val enabled = !readOnly && entry.site !in model.siteDefaultBusy
            val pickerTitle = entry.site
            row {
                FluxListRow(
                    title = entry.site,
                    subtitle = if (entry.automatic) str(R.string.pluginSiteDefaultAutomaticHint) else null,
                    value = names[entry.identity] ?: entry.identity,
                    trailing = if (entry.automatic) {
                        { FluxTag(str(R.string.pluginSiteDefaultAutomatic), tone = Tone.Amber) }
                    } else {
                        null
                    },
                    chevron = true,
                    enabled = enabled,
                    onClick = {
                        openPicker(
                            PickerSpec(
                                pickerTitle,
                                entry.candidates.map { SelectOption(it, names[it] ?: it) },
                                entry.identity,
                            ) { id -> if (id != entry.identity) model.setSiteDefault(env, entry.site, id) },
                        )
                    },
                )
            }
            custom(padded = true) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (entry.automatic) {
                        FluxButton(
                            str(R.string.pluginSiteDefaultConfirm),
                            { model.setSiteDefault(env, entry.site, entry.identity) },
                            variant = ButtonVariant.Secondary,
                            size = ButtonSize.Sm,
                            enabled = enabled,
                        )
                    }
                    FluxButton(
                        str(R.string.pluginSiteDefaultRemove),
                        { model.setSiteDefault(env, entry.site, null) },
                        variant = ButtonVariant.Ghost,
                        size = ButtonSize.Sm,
                        enabled = enabled,
                    )
                }
            }
        }
    }
}

// ───────────────────────────── 行 ─────────────────────────────

/** 徽标：开发模式 / 加载状态 / 禁用原因 / 撤回标记 / 可用更新。 */
@Composable
internal fun pluginBadges(plugin: PluginDto, update: MarketEntry?, yanked: String?): List<Badge> {
    val out = ArrayList<Badge>()
    if (plugin.devMode) out += Badge(str(R.string.pluginDevModeBadge), Tone.Accent, FluxIcons.Code)
    if (plugin.loadFailed) {
        out += Badge(str(R.string.pluginLoadStatusFailed), Tone.Coral, FluxIcons.TriangleAlert)
    } else {
        out += Badge(str(R.string.pluginLoadStatusLoaded), Tone.Mint, FluxIcons.CircleCheck)
    }
    if (plugin.disabledReason == "Manual") out += Badge(str(R.string.pluginDisabledManual), Tone.Neutral, FluxIcons.CirclePause)
    if (plugin.disabledReason == "CircuitBreaker") out += Badge(str(R.string.pluginDisabledCircuitBreaker), Tone.Coral, FluxIcons.ZapOff)
    if (plugin.disabledReason == "Revoked") out += Badge(str(R.string.pluginDisabledRevoked), Tone.Coral, FluxIcons.ShieldAlert)
    val yankedRes = yanked?.let { yankedLabelRes(it) }
    if (yankedRes != null) {
        out += Badge(str(R.string.pluginInstalledVersionYanked, "label" to str(yankedRes)), Tone.Coral, FluxIcons.ShieldAlert)
    }
    if (update != null) {
        out += Badge(str(R.string.pluginUpdateAvailable, "version" to update.version), Tone.Accent, FluxIcons.CircleArrowDown)
    }
    return out
}

/** 已安装插件行（纯信息）：图标 · 名称 / 版本 / 主页 / 描述 / 徽标流 / 加载错误；启用开关在详情页。 */
@Composable
private fun PluginRow(
    plugin: PluginDto,
    update: MarketEntry?,
    yanked: String?,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val tile = RoundedCornerShape(10.dp)
    val failed = plugin.loadFailed || plugin.disabledReason == "CircuitBreaker"
    Row(
        Modifier
            .fillMaxWidth()
            .fluxPressable(onClick = onClick, scale = 0.99f, onLongClick = onLongClick)
            .heightIn(min = 56.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Box(
            Modifier.size(32.dp).background(c.glass2, tile).border(0.5.dp, c.hairline, tile),
            contentAlignment = Alignment.Center,
        ) {
            FluxIcon(FluxIcons.Puzzle, null, size = 18.dp, tint = if (failed) c.coralText else c.accentHi)
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.Bottom) {
                FluxText(plugin.name, Modifier.weight(1f, fill = false), style = t.weight(t.body, 500), color = c.ink, maxLines = 2)
                FluxText("v${plugin.version}", style = t.monoS, color = c.inkMuted, maxLines = 1)
            }
            if (plugin.homepage.isNotEmpty()) FluxText(plugin.homepage, style = t.sm, color = c.inkMuted, maxLines = 1)
            if (plugin.description.isNotEmpty()) FluxText(plugin.description, style = t.sm, color = c.inkMuted, maxLines = 2)
            BadgeFlow(pluginBadges(plugin, update, yanked))
            if (plugin.loadFailed && plugin.loadError.isNotEmpty()) {
                FluxText(plugin.loadError, style = t.sm, color = c.coralText, maxLines = 2)
            }
        }
        FluxIcon(FluxIcons.ChevronRight, null, size = 18.dp, tint = c.inkFaint, modifier = Modifier.padding(top = 7.dp))
    }
}

/** 长按行：快捷操作对话框（设置 / 登录 / 更新 / 重载 / 复制错误 / 卸载）。 */
private fun showPluginActions(
    overlays: FluxOverlayState,
    env: ExtensionsEnv,
    model: ExtensionsModel,
    plugin: PluginDto,
    readOnly: Boolean,
    onSheet: (PluginSheet) -> Unit,
) {
    // 自上而下的视觉顺序；FluxDialogSpec 的声明约定是「取消在前、主操作在后」，纵排时逆序显示
    val actions = ArrayList<FluxDialogButton>()
    model.updateFor(plugin)?.let { update ->
        if (!readOnly) {
            actions += FluxDialogButton(env.text(R.string.marketUpdateButton)) { model.requestInstall(env, update, plugin) }
        }
    }
    if (!plugin.loadFailed && plugin.settings.isNotEmpty()) {
        actions += FluxDialogButton(env.text(R.string.pluginSettingsTooltip)) { onSheet(PluginSheet.Settings(plugin.identity)) }
    }
    if (!plugin.loadFailed && plugin.authSupported && !readOnly) {
        actions += FluxDialogButton(env.text(R.string.pluginAuthButton)) { onSheet(PluginSheet.Auth(plugin.identity)) }
    }
    if (plugin.devMode && !readOnly) {
        actions += FluxDialogButton(env.text(R.string.pluginReloadTooltip)) { model.reload(env, plugin) }
    }
    if (plugin.loadFailed && plugin.loadError.isNotEmpty()) {
        actions += FluxDialogButton(env.text(R.string.pluginLoadErrorCopy)) {
            env.context.copyPlainText(plugin.loadError)
            env.toast(env.text(R.string.pluginLoadErrorCopied), FluxToastKind.Success)
        }
    }
    if (!readOnly) {
        actions += FluxDialogButton(env.text(R.string.pluginUninstallTooltip), FluxDialogButtonStyle.Destructive) {
            confirmUninstall(overlays, env, model, plugin)
        }
    }
    val buttons = listOf(FluxDialogButton(env.text(R.string.cancel))) + actions.asReversed()
    overlays.showDialog(FluxDialogSpec(title = plugin.name, buttons = buttons, stacked = true))
}

/** 卸载确认对话框（行菜单与详情页共用）。 */
internal fun confirmUninstall(
    overlays: FluxOverlayState,
    env: ExtensionsEnv,
    model: ExtensionsModel,
    plugin: PluginDto,
    onDone: () -> Unit = {},
) {
    overlays.showDialog(
        FluxDialogSpec(
            title = env.text(R.string.pluginUninstallTitle),
            message = env.text(R.string.pluginUninstallMsg, "name" to plugin.name),
            icon = FluxIcons.Trash,
            buttons = listOf(
                FluxDialogButton(env.text(R.string.cancel)),
                FluxDialogButton(env.text(R.string.pluginUninstallTooltip), FluxDialogButtonStyle.Destructive) {
                    model.uninstall(env, plugin, onDone)
                },
            ),
        ),
    )
}

// ───────────────────────────── 共用浮层：权限确认 / 缺组件提醒 ─────────────────────────────

/**
 * 市场安装的权限确认 Sheet 与「缺少基础组件」提醒：扩展页 / 市场页 / 详情页共用
 * （同一时刻只有栈顶页面在组合中，由它呈现）。
 */
@Composable
internal fun ExtensionsOverlays(model: ExtensionsModel, env: ExtensionsEnv) {
    val nav = LocalNavigator.current
    val overlays = LocalFluxOverlays.current
    PermissionConfirmSheet(model, env)
    val missing = model.missingComponents
    LaunchedEffect(missing) {
        if (missing == null) return@LaunchedEffect
        model.missingComponents = null
        val names = missing.joinToString(", ") { env.context.componentTitle(it) }
        overlays.showDialog(
            FluxDialogSpec(
                title = env.text(R.string.pluginDepsMissingTitle),
                message = env.text(R.string.pluginDepsMissingBody, "components" to names),
                icon = FluxIcons.TriangleAlert,
                buttons = listOf(
                    FluxDialogButton(env.text(R.string.pluginDepsLater)),
                    FluxDialogButton(env.text(R.string.pluginDepsGoToComponents), FluxDialogButtonStyle.Primary) {
                        goToComponents(nav, model)
                    },
                ),
            ),
        )
    }
}

/** 回到扩展根页并切到「组件」页签。 */
private fun goToComponents(nav: com.fluxdown.app.nav.AppNavigator, model: ExtensionsModel) {
    while (true) {
        val top = nav.top as? Route.Settings ?: break
        if (top.page != SettingsPage.PluginDetail && top.page != SettingsPage.PluginMarket) break
        nav.pop()
    }
    model.tab = ExtensionsModel.Tab.Components
}

@Composable
private fun PermissionConfirmSheet(model: ExtensionsModel, env: ExtensionsEnv) {
    val request = model.permissionRequest
    val shown = rememberLastNonNull(request)
    val dismiss = { model.permissionRequest = null }
    FluxPortal {
        FluxSheet(
            visible = request != null,
            onDismissRequest = dismiss,
            detent = FluxSheetDetent.Wrap,
            title = shown?.let { str(if (it.isUpdate) R.string.pluginPermConfirmUpdateTitle else R.string.pluginPermConfirmInstallTitle, "name" to it.entry.displayName) },
            header = {
                if (shown != null) {
                    FluxSheetHeader(
                        title = str(
                            if (shown.isUpdate) R.string.pluginPermConfirmUpdateTitle else R.string.pluginPermConfirmInstallTitle,
                            "name" to shown.entry.displayName,
                        ),
                        subtitle = str(
                            if (shown.isUpdate) R.string.pluginPermConfirmUpdateBody else R.string.pluginPermConfirmInstallBody,
                            "version" to shown.entry.version,
                        ),
                        onClose = dismiss,
                    )
                }
            },
            footer = {
                if (shown != null) {
                    FluxSheetFooter {
                        FluxButton(str(R.string.cancel), onClick = dismiss, modifier = Modifier.weight(1f))
                        FluxButton(
                            str(if (shown.isUpdate) R.string.pluginPermConfirmUpdateOk else R.string.pluginPermConfirmInstallOk),
                            onClick = { model.confirmInstall(env, shown) },
                            modifier = Modifier.weight(1f),
                            variant = ButtonVariant.Primary,
                        )
                    }
                }
            },
        ) {
            if (shown != null) {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    if (shown.versionChanged) {
                        FluxBanner(str(R.string.pluginErrorMarketVersionChanged), kind = FluxBannerKind.Warn, slim = true)
                    }
                    GlassSection {
                        for (permission in shown.permissions) {
                            row { PermissionBlock(permission, Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) }
                        }
                    }
                }
            }
        }
    }
}

// ───────────────────────────── 搜索索引 ─────────────────────────────

/** 扩展页的搜索条目（iOS 无行级条目；行 id 用 `extensions.<行>`，itemKey = 所在 flowItem 的 key）。 */
internal fun extensionsSearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> {
    if (ctx.isLocalHost) return emptyList()
    val list = ArrayList<SettingsEntry>()
    if (ctx.has(HostCapability.daemonPlugins)) {
        val crumb = ctx.crumb(R.string.settingsCatExtensions, R.string.settingsCatPlugins)
        list += ctx.entry("extensions.plugins", SettingsPage.Extensions, "plugins", R.string.pluginsSectionTitle, null, crumb, FluxIcons.Puzzle)
        list += ctx.entry("extensions.install", SettingsPage.Extensions, "install", R.string.pluginInstallZipButton, null, crumb, FluxIcons.FileUp)
        list += ctx.entry("extensions.market", SettingsPage.Extensions, "market", R.string.marketSectionTitle, R.string.marketSectionDesc, crumb, FluxIcons.Store)
    }
    if (ctx.has(HostCapability.daemonComponents)) {
        val crumb = ctx.crumb(R.string.settingsCatExtensions, R.string.settingsCatComponents)
        list += ctx.entry("extensions.ffmpeg", SettingsPage.Extensions, "component.ffmpeg", R.string.componentsFfmpegTitle, R.string.componentsFfmpegDesc, crumb, FluxIcons.Film)
        list += ctx.entry("extensions.ytdlp", SettingsPage.Extensions, "component.ytdlp", R.string.componentsYtdlpTitle, R.string.componentsYtdlpDesc, crumb, FluxIcons.Download)
        list += ctx.entry("extensions.componentMirror", SettingsPage.Extensions, "component.mirror", R.string.mobileComponentMirrorBase, R.string.mobileComponentMirrorBaseDesc, crumb, FluxIcons.Globe)
    }
    return list
}
