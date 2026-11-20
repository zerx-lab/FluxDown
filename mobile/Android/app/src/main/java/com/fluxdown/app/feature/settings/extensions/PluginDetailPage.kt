package com.fluxdown.app.feature.settings.extensions

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.selection.SelectionContainer
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
import com.fluxdown.app.feature.settings.SettingsPageFrame
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.openLink
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.protocol.PluginAuth
import com.fluxdown.core.protocol.PluginDetail
import com.fluxdown.core.protocol.PluginMarket
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxKeyValue
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import androidx.compose.ui.platform.LocalContext

/** S11.3 · 插件详情（已安装）：manifest 信息 + 操作区（启用 / 设置 / 登录 / 更新 / 重载 / 卸载）。 */
@Composable
internal fun PluginDetailPage(pluginId: String) {
    val nav = LocalNavigator.current
    val model = rememberExtensionsModel()
    val env = rememberExtensionsEnv()
    val overlays = LocalFluxOverlays.current
    val hostState = hostState()
    val readOnly by remember { derivedStateOf { hostState.value.isReadOnly } }
    val plugins by rememberPlugins()
    val plugin = plugins.firstOrNull { it.identity == pluginId }
    val conflicts = rememberResolverUrlConflicts(plugins, remember(pluginId) { setOf(pluginId) })
    val gate = rememberFlowInGate()
    var sheet by remember { mutableStateOf<PluginSheet?>(null) }

    // 插件被卸载（含其他客户端操作）或切换主机后不存在：回到列表。
    LaunchedEffect(plugin == null) {
        if (plugin == null && nav.top == Route.Settings(SettingsPage.PluginDetail, pluginId)) nav.pop()
    }
    LaunchedEffect(readOnly) { model.ensureMarketLoaded(env) }
    PluginAutoDisabledEffect(env)

    Box(Modifier.fillMaxSize()) {
        SettingsPageFrame(title = plugin?.name.orEmpty(), onBack = { nav.pop() }) {
            if (plugin == null) return@SettingsPageFrame
            val busy = readOnly || plugin.identity in model.busy
            val yanked = PluginMarket.installedVersionYanked(model.marketEntries, plugin)
            val update = model.updateFor(plugin)
            var index = 0
            if (readOnly) {
                flowItem(index++, gate, "banner") {
                    FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            flowItem(index++, gate, "head") {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    FluxText("v${plugin.version}", Modifier.padding(horizontal = 6.dp), style = FluxTheme.type.mono, color = FluxTheme.colors.inkMuted)
                    BadgeFlow(pluginBadges(plugin, null, yanked), Modifier.padding(horizontal = 6.dp))
                }
            }
            if (plugin.disabledReason == "CircuitBreaker") {
                flowItem(index++, gate, "breaker") {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        FluxBanner(str(R.string.pluginDisabledCircuitBreaker), kind = FluxBannerKind.Error, icon = FluxIcons.ZapOff, slim = true)
                        GlassSection {
                            row {
                                FluxActionRow(
                                    title = str(R.string.pluginAutoDisabledReenable),
                                    enabled = !busy && !plugin.loadFailed,
                                    onClick = { model.setEnabled(env, plugin, true) },
                                )
                            }
                        }
                    }
                }
            }
            if (plugin.loadFailed) {
                flowItem(index++, gate, "loadError") {
                    GlassSection {
                        custom(padded = true) {
                            SelectionContainer {
                                FluxText(
                                    plugin.loadError.ifEmpty { str(R.string.pluginLoadErrorTitle) },
                                    Modifier.fillMaxWidth(),
                                    style = FluxTheme.type.sm,
                                    color = FluxTheme.colors.coralText,
                                )
                            }
                        }
                        row(hasIcon = true) {
                            FluxActionRow(
                                title = str(R.string.pluginLoadErrorTitle),
                                icon = FluxIcons.CircleAlert,
                                onClick = { sheet = PluginSheet.LoadError(plugin.identity) },
                            )
                        }
                        row(hasIcon = true) {
                            FluxActionRow(
                                title = str(R.string.pluginLoadErrorCopy),
                                icon = FluxIcons.Copy,
                                enabled = plugin.loadError.isNotEmpty(),
                                onClick = {
                                    env.context.copyPlainText(plugin.loadError)
                                    env.toast(env.text(R.string.pluginLoadErrorCopied), FluxToastKind.Success)
                                },
                            )
                        }
                    }
                }
            }
            if (conflicts.isNotEmpty()) {
                flowItem(index++, gate, "resolverUrlConflicts") {
                    ResolverUrlConflictSection(conflicts)
                }
            }
            flowItem(index++, gate, "info") { PluginInfoBlock(PluginDetail.of(plugin), env) }
            flowItem(index++, gate, "actions") {
                GlassSection {
                    row {
                        FluxSwitchRow(
                            title = str(R.string.rssEnabledLabel),
                            checked = plugin.enabled && !plugin.loadFailed,
                            onCheckedChange = { model.setEnabled(env, plugin, it) },
                            enabled = !busy && !plugin.loadFailed,
                        )
                    }
                    if (!plugin.loadFailed && plugin.settings.isNotEmpty()) {
                        row(hasIcon = true) {
                            FluxListRow(
                                title = str(R.string.pluginSettingsTooltip),
                                icon = FluxIcons.Settings,
                                chevron = true,
                                enabled = !busy,
                                onClick = { sheet = PluginSheet.Settings(plugin.identity) },
                            )
                        }
                    }
                    if (!plugin.loadFailed && plugin.authSupported) {
                        row(hasIcon = true) {
                            FluxListRow(
                                title = str(R.string.pluginAuthButton),
                                icon = FluxIcons.KeyRound,
                                chevron = true,
                                enabled = !busy,
                                onClick = { sheet = PluginSheet.Auth(plugin.identity) },
                            )
                        }
                    }
                    if (update != null) {
                        row(hasIcon = true) {
                            FluxActionRow(
                                title = str(R.string.pluginUpdateAvailable, "version" to update.version),
                                icon = FluxIcons.CircleArrowDown,
                                loading = plugin.identity in model.marketPending,
                                enabled = !busy && plugin.identity !in model.marketPending,
                                onClick = { model.requestInstall(env, update, plugin) },
                            )
                        }
                    }
                    if (plugin.devMode) {
                        row(hasIcon = true) {
                            FluxActionRow(
                                title = str(R.string.pluginReloadTooltip),
                                icon = FluxIcons.RefreshCw,
                                enabled = !busy,
                                onClick = { model.reload(env, plugin) },
                            )
                        }
                    }
                    row(hasIcon = true) {
                        FluxActionRow(
                            title = str(R.string.pluginUninstallTooltip),
                            icon = FluxIcons.Trash,
                            tone = Tone.Coral,
                            enabled = !busy,
                            onClick = { confirmUninstall(overlays, env, model, plugin) },
                        )
                    }
                }
            }
        }
        PluginSheetHost(sheet = sheet, plugins = plugins, onDismiss = { sheet = null })
        ExtensionsOverlays(model, env)
    }
}

/**
 * 详情信息分区（已安装与市场条目共用）：撤回横幅 / 标签 / 标识 / 作者 / 主页 / 发布时间 / 最低版本 / 设置项 /
 * 描述 / 权限 / 须知。
 */
@Composable
internal fun PluginInfoBlock(detail: PluginDetail, env: ExtensionsEnv) {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val noBrowser = str(R.string.mobileNoBrowser)
    Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
        yankedLabelRes(detail.yanked)?.let {
            FluxBanner(str(it), kind = FluxBannerKind.Error, icon = FluxIcons.ShieldAlert, slim = true)
        }
        if (detail.tags.isNotEmpty()) {
            BadgeFlow(detail.tags.map { Badge(it, Tone.Neutral) }, Modifier.padding(horizontal = 6.dp))
        }
        GlassSection {
            row {
                FluxKeyValue(
                    key = str(R.string.pluginDetailIdentity),
                    value = detail.identity,
                    mono = true,
                    onClick = {
                        env.context.copyPlainText(detail.identity)
                        env.toast(env.text(R.string.apiServiceCopied), FluxToastKind.Success)
                    },
                )
            }
            if (detail.author.isNotEmpty()) row { FluxKeyValue(str(R.string.pluginDetailAuthor), detail.author) }
            if (detail.homepage.isNotEmpty()) {
                val link = PluginAuth.safeHttpUrl(detail.homepage)
                row {
                    FluxKeyValue(
                        key = str(R.string.pluginDetailHomepage),
                        value = detail.homepage,
                        tone = if (link != null) Tone.Accent else Tone.Neutral,
                        onClick = link?.let { url -> { openLink(context, overlays, url, noBrowser) } },
                    )
                }
            }
            if (detail.publishTime.isNotEmpty()) row { FluxKeyValue(str(R.string.pluginDetailPublishTime), detail.publishTime) }
            if (detail.minAppVersion.isNotEmpty()) row { FluxKeyValue(str(R.string.pluginDetailMinAppVersion), detail.minAppVersion) }
            if (detail.settingsCount > 0) {
                row {
                    FluxKeyValue(
                        str(R.string.pluginDetailSettings),
                        str(R.string.pluginDetailSettingsCount, "count" to detail.settingsCount),
                    )
                }
            }
        }
        if (detail.description.isNotEmpty()) {
            GlassSection(title = str(R.string.pluginDetailDescription)) {
                custom(padded = true) {
                    SelectionContainer { FluxText(detail.description, Modifier.fillMaxWidth(), style = t.body, color = c.ink) }
                }
            }
        }
        if (detail.permissions.isNotEmpty()) {
            GlassSection(title = str(R.string.pluginDetailPermissions)) {
                for (permission in detail.permissions) {
                    row { PermissionBlock(permission, Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp)) }
                }
            }
        }
        GlassSection(title = str(R.string.pluginDetailUsage)) {
            custom(padded = true) { FluxText(str(R.string.pluginDetailUsageBody), style = t.sm, color = c.inkMuted) }
        }
    }
}
