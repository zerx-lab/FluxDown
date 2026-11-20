package com.fluxdown.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.data.AppearanceState
import com.fluxdown.app.data.ThemeMode
import com.fluxdown.app.feature.devices.localizedName
import com.fluxdown.app.feature.devices.localizedSubtitle
import com.fluxdown.app.feature.settings.bt.btReadout
import com.fluxdown.app.feature.settings.bt.ed2kReadout
import com.fluxdown.app.feature.settings.diagnostics.diagnosticsReadout
import com.fluxdown.app.feature.settings.general.notifyReadout
import com.fluxdown.app.feature.settings.network.networkReadout
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.nav.SheetRoute
import com.fluxdown.app.shell.LocalAppearance
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.format.Format
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.CustomCategoryDto
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.SettingsForm
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.store.Connection
import com.fluxdown.fluxui.chrome.FluxHeader
import com.fluxdown.fluxui.chrome.FluxHostPill
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldAction
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxPresenceDot
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.feedback.FluxEmpty
import com.fluxdown.fluxui.feedback.FluxGlyph
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import java.util.Locale

/**
 * S1 · 设置首页（Tab 根页）。分类顺序同 iOS / PC（[visibleSettingsPages]）；各分类行的读数由所在页提供
 * （`xxxReadout(ctx)`，与页面渲染共用同一份判定）。顶部搜索框过滤 [SettingsIndex]（与全局搜索同一份索引），
 * 命中后跳到目标页并定位、高亮该行。断线时「引擎」分组降为 40% 并出现只读横幅（分类仍可点进，页内只读）。
 */
@Composable
fun SettingsScreen() {
    val nav = LocalNavigator.current
    val container = LocalAppContainer.current
    val context = LocalContext.current
    val configuration = LocalConfiguration.current
    val focus = LocalSettingsFocus.current
    val focusManager = LocalFocusManager.current
    val hostState = hostState()
    val readOnly by remember { derivedStateOf { hostState.value.isReadOnly } }
    val host by container.host.collectAsState()
    val appearance = LocalAppearance.current
    val cfg by remember { derivedStateOf { hostState.value.config } }
    val prefs by remember { derivedStateOf { hostState.value.sections[HostSection.agentPreferences] } }
    val info by remember { derivedStateOf { hostState.value.info } }
    val listState = rememberLazyListState()
    val gate = rememberFlowInGate()
    val statusTop = WindowInsets.statusBars.asPaddingValues().calculateTopPadding()
    val version = remember(context) { context.appVersionName() }
    var query by rememberSaveable { mutableStateOf("") }

    val isLocal = host is HostRef.Local
    val ctx = remember(context, configuration, cfg, prefs, info, isLocal) {
        val state = hostState.value
        SettingsSearchContext(context, state, SettingsForm(state.config, prefs = state.preferences.values), isLocal)
    }
    val pages = remember(ctx) { visibleSettingsPages(ctx.state, isLocal) }
    val trimmed = query.trim()
    val hits = remember(ctx, trimmed) {
        if (trimmed.isEmpty()) emptyList() else SettingsSearch.filter(SettingsIndex.entries(ctx), trimmed)
    }

    val hostTitle = host.localizedName()
    val switchHostDescription = str(R.string.mobileSettingsSwitchHost, "name" to hostTitle)

    fun open(page: SettingsPage) = nav.push(Route.Settings(page))

    @Composable
    fun readout(page: SettingsPage): String? = when (page) {
        SettingsPage.General -> null
        SettingsPage.Appearance -> appearanceReadout(appearance)
        SettingsPage.Notify -> notifyReadout(ctx)
        SettingsPage.Download -> downloadReadout(ctx, isLocal)
        SettingsPage.Bt -> btReadout(ctx)
        SettingsPage.Ed2k -> ed2kReadout(ctx)
        SettingsPage.Network -> networkReadout(ctx)
        SettingsPage.Diagnostics -> diagnosticsReadout(ctx)
        SettingsPage.About -> "v$version"
        else -> null
    }

    @Composable
    fun categoryRow(page: SettingsPage) {
        val meta = page.meta()
        FluxListRow(
            title = str(meta.title),
            subtitle = readout(page)?.ifEmpty { null } ?: meta.desc?.let { str(it) },
            icon = meta.icon,
            chevron = true,
            onClick = { open(page) },
        )
    }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        LazyColumn(
            state = listState,
            modifier = Modifier.widthIn(max = PageMaxWidth).fillMaxWidth().fillMaxSize(),
            contentPadding = PaddingValues(
                start = FluxTheme.space.screenMargin,
                end = FluxTheme.space.screenMargin,
                top = statusTop,
                bottom = FluxTheme.space.dockClearance + WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding(),
            ),
            verticalArrangement = Arrangement.spacedBy(SectionGap),
        ) {
            flowItem(0, gate, "header") {
                FluxHeader(title = str(R.string.settings))
            }
            flowItem(1, gate, "search") {
                FluxField(
                    value = query,
                    onValueChange = { query = it },
                    placeholder = str(R.string.settingsSearchHint),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    trailing = {
                        if (query.isNotEmpty()) {
                            FluxFieldAction(FluxIcons.X, str(R.string.webSearchClear), onClick = { query = "" })
                        } else {
                            FluxIcon(FluxIcons.Search, null, Modifier.padding(end = 12.dp), 18.dp, FluxTheme.colors.inkMuted)
                        }
                    },
                )
            }
            if (trimmed.isNotEmpty()) {
                if (hits.isEmpty()) {
                    item(key = "empty") {
                        FluxEmpty(
                            glyph = FluxGlyph.Search,
                            title = str(R.string.settingsSearchNoResults),
                        )
                    }
                } else {
                    item(key = "results") {
                        GlassSection {
                            for (entry in hits) {
                                row(hasIcon = true) {
                                    FluxListRow(
                                        title = entry.title,
                                        subtitle = entry.breadcrumb,
                                        icon = entry.icon,
                                        chevron = true,
                                        onClick = {
                                            focusManager.clearFocus()
                                            focus.request(entry)
                                            open(entry.page)
                                        },
                                    )
                                }
                            }
                        }
                    }
                }
                return@LazyColumn
            }
            if (readOnly) {
                flowItem(2, gate, "banner") {
                    FluxBanner(text = str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
                }
            }
            if (SettingsPage.Account in pages) {
                flowItem(3, gate, "account") {
                    GlassSection { row(hasIcon = true) { categoryRow(SettingsPage.Account) } }
                }
            }
            flowItem(4, gate, "host") {
                GlassSection {
                    row(hasIcon = true) {
                        FluxListRow(
                            title = hostTitle,
                            subtitle = host.localizedSubtitle(),
                            icon = if (host is HostRef.Remote) FluxIcons.Server else FluxIcons.Smartphone,
                            trailing = { ConnectionBadge() },
                            chevron = true,
                            onClick = { nav.openSheet(SheetRoute.HostSwitch) },
                        )
                    }
                }
            }
            flowItem(5, gate, "personal") {
                GlassSection(title = str(R.string.settingsGroupPersonal)) {
                    for (page in pages.filter { it in PersonalPages }) row(hasIcon = true) { categoryRow(page) }
                }
            }
            flowItem(6, gate, "engine") {
                Box(Modifier.alpha(if (readOnly) 0.4f else 1f)) {
                    GlassSection(
                        title = str(R.string.settingsGroupEngine),
                        action = {
                            FluxHostPill(
                                name = hostTitle,
                                online = !readOnly,
                                onClick = { nav.openSheet(SheetRoute.HostSwitch) },
                                description = switchHostDescription,
                            )
                        },
                    ) {
                        for (page in pages.filter { it in EnginePages }) row(hasIcon = true) { categoryRow(page) }
                    }
                }
            }
            flowItem(7, gate, "maintenance") {
                GlassSection(title = str(R.string.settingsGroupMaintenance)) {
                    for (page in pages.filter { it in MaintenancePages }) row(hasIcon = true) { categoryRow(page) }
                }
            }
            flowItem(8, gate, "footer") {
                FluxText(
                    text = str(R.string.mobileFooter),
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                    style = FluxTheme.type.sm.copy(textAlign = TextAlign.Center),
                    color = FluxTheme.colors.inkFaint,
                )
            }
        }
    }
}

private val PersonalPages = setOf(SettingsPage.General, SettingsPage.Appearance, SettingsPage.Notify)
private val EnginePages = setOf(
    SettingsPage.Download, SettingsPage.Bt, SettingsPage.Ed2k, SettingsPage.Network,
    SettingsPage.Extensions, SettingsPage.Webhook, SettingsPage.Api,
)
private val MaintenancePages = setOf(SettingsPage.Diagnostics, SettingsPage.About)

/** 连接状态读数：圆点 + 文字；只在 [Connection] 变化时重组。 */
@Composable
private fun ConnectionBadge() {
    val hostState = hostState()
    val connection by remember { derivedStateOf { hostState.value.connection } }
    val c = FluxTheme.colors
    val (tone, label) = when (connection) {
        Connection.Live -> Tone.Mint to str(R.string.mobileSettingsConnLive)
        Connection.Connecting -> Tone.Amber to str(R.string.mobileSettingsConnConnecting)
        Connection.Stale -> Tone.Amber to str(R.string.mobileSettingsConnStale)
        is Connection.Failed -> Tone.Coral to str(R.string.mobileSettingsConnFailed)
    }
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
        FluxPresenceDot(tone = tone, size = 7.dp)
        FluxText(label, style = FluxTheme.type.sm, color = c.inkMuted, maxLines = 1)
    }
}

@Composable
private fun appearanceReadout(a: AppearanceState): String {
    val mode = when (a.mode) {
        ThemeMode.System -> str(R.string.themeModeSystem)
        ThemeMode.Light -> str(R.string.themeModeLight)
        ThemeMode.Dark -> str(R.string.themeModeDark)
    }
    val accent = when {
        a.dynamicColor -> str(R.string.mobileSettingsReadoutWallpaper)
        a.scheme == "green" -> str(R.string.colorGreen)
        a.scheme == "violet" -> str(R.string.colorViolet)
        a.scheme == "rose" -> str(R.string.colorRose)
        a.scheme == "custom" -> str(R.string.colorCustom) + " " + a.customColor.hexRgb()
        else -> str(R.string.colorBlue)
    }
    val parts = ArrayList<String>(3)
    parts += mode
    parts += accent
    return parts.joinToString(" · ")
}

/** 下载分类读数：默认目录末段（仅远端主机） · 并发数 [· 限速]；全部来自 `config`。 */
@Composable
private fun downloadReadout(ctx: SettingsSearchContext, isLocal: Boolean): String {
    val config = ctx.state.config
    val dir = if (isLocal) "" else config["default_save_dir"].orEmpty().trimEnd('/', '\\').substringAfterLast('/').substringAfterLast('\\')
    val concurrent = config["max_concurrent_tasks"]
    val limit = config["speed_limit_bytes"]?.toLongOrNull() ?: 0L
    val parts = ArrayList<String>(4)
    when {
        dir.isNotEmpty() && concurrent != null ->
            parts += str(R.string.mobileSettingsReadoutDownload, "dir" to dir, "n" to concurrent)
        concurrent != null -> parts += str(R.string.mobileSettingsReadoutConcurrent, "n" to concurrent)
        dir.isNotEmpty() -> parts += dir
    }
    Format.speed(limit)?.let { parts += str(R.string.mobileSettingsReadoutLimit, "speed" to it.toString()) }
    ctx.form.pref(CustomCategoryDto.PREFERENCE_KEY)?.let {
        parts += str(R.string.mobileGeneralReadoutCategories, "n" to CustomCategoryDto.fromPreference(it).size)
    }
    return parts.joinToString(" · ")
}

/** `#RRGGBB`（忽略 alpha）。 */
internal fun Int.hexRgb(): String = String.format(Locale.ROOT, "#%06X", this and 0xFFFFFF)
