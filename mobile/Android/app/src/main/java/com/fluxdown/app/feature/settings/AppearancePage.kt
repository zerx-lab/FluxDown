package com.fluxdown.app.feature.settings

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import com.fluxdown.fluxui.controls.FluxDivider
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.data.ThemeMode
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.shell.LocalAppearance
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.fluxui.controls.ButtonSize
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.core.format.FileSizeUnits
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.wallpaperAccent
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import java.io.IOException

/**
 * S4 · 外观（设备本地，断线不影响）：预览、语言、主题模式、主题色（含自定义取色器）、
 * 跟随壁纸取色、系统文字与显示大小。所有修改经 [com.fluxdown.app.data.AppearanceRepo] 持久化，
 * FluxTheme 随 DataStore 流实时重绘。
 */
@Composable
internal fun AppearancePage() {
    val nav = LocalNavigator.current
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val context = LocalContext.current
    val haptics = FluxTheme.haptics
    val repo = container.appearance
    val appearance = LocalAppearance.current
    val gate = rememberFlowInGate()
    val motion = FluxTheme.motion
    val animateSize = Modifier.animateContentSize(motion.of(motion.fluid))
    val failText = str(R.string.localServiceActionFailed)
    val noApp = str(R.string.mobileNoSettingsApp)

    /** 持久化写入：失败时 toast（Error 吐司自带 REJECT 触感）。 */
    fun write(block: suspend () -> Unit) {
        container.appScope.launch {
            try {
                block()
            } catch (e: IOException) {
                overlays.toast(failText, FluxToastKind.Error)
            }
        }
    }

    // 自定义色：取色器每次变化 → 250ms 防抖写入（仍处于“自定义”时才写，避免覆盖随后点选的预设）
    var customDraft by remember { mutableIntStateOf(0) }
    var customDirty by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        snapshotFlow { if (customDirty) customDraft else null }.collectLatest { argb ->
            if (argb != null) {
                delay(250)
                if (appearance.scheme == "custom") write { repo.setCustomColor(argb) }
                customDirty = false
            }
        }
    }
    DisposableEffect(Unit) {
        onDispose {
            if (customDirty && appearance.scheme == "custom") {
                val argb = customDraft
                container.appScope.launch {
                    try {
                        repo.setCustomColor(argb)
                    } catch (e: IOException) {
                        overlays.toast(failText, FluxToastKind.Error)
                    }
                }
            }
        }
    }

    SettingsPageFrame(title = str(R.string.settingsCatAppearance), onBack = { nav.pop() }) {
        flowItem(0, gate, "preview") {
            ThemePreview()
        }
        if (Build.VERSION.SDK_INT >= 33) {
            flowItem(1, gate, "language") {
                GlassSection(footer = str(R.string.mobileLanguageSystemHint)) {
                    row(hasIcon = true) {
                        FluxListRow(
                            title = str(R.string.language),
                            modifier = Modifier.settingsFocus("appearance.language"),
                            icon = FluxIcons.Globe,
                            value = str(R.string.languageNativeName),
                            chevron = true,
                            onClick = {
                                val intent = Intent(Settings.ACTION_APP_LOCALE_SETTINGS)
                                    .setData(Uri.fromParts("package", context.packageName, null))
                                startSettingsIntent(context, overlays, intent, noApp)
                            },
                        )
                    }
                }
            }
        }
        flowItem(2, gate, "theme") {
            GlassSection(animateSize, title = str(R.string.settingsGroupTheme), footer = str(R.string.settingsSyncLegend)) {
                custom(padded = true) {
                    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(22.dp)) {
                        SubBlock(str(R.string.themeMode), "appearance.mode") {
                            ModeTiles(
                                selected = appearance.mode,
                                onSelect = { mode: ThemeMode ->
                                    if (mode != appearance.mode) {
                                        haptics.tick()
                                        write { repo.setMode(mode) }
                                    }
                                },
                            )
                        }
                        SubBlock(str(R.string.themeColor), "appearance.color") {
                            Column(verticalArrangement = Arrangement.spacedBy(18.dp)) {
                                AccentDots(
                                    scheme = appearance.scheme,
                                    customColor = appearance.customColor,
                                    dimmed = appearance.dynamicColor,
                                    onPreset = { id ->
                                        if (id != appearance.scheme) {
                                            haptics.tick()
                                            write { repo.setScheme(id) }
                                        }
                                    },
                                    onCustom = {
                                        if (appearance.scheme != "custom") {
                                            haptics.tick()
                                            // 取色器以持久化的自定义色初始化
                                            write { repo.setScheme("custom") }
                                        }
                                    },
                                )
                                if (appearance.scheme == "custom" && !appearance.dynamicColor) {
                                    CustomColorPicker(
                                        initial = appearance.customColor,
                                        onChange = {
                                            customDraft = it
                                            customDirty = true
                                        },
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
        // 跟随壁纸取色依赖 Android 12 的系统调色板
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) flowItem(3, gate, "ambience") {
            GlassSection(animateSize, title = str(R.string.mobileGroupAmbience)) {
                row(hasIcon = true) {
                    FluxSwitchRow(
                        title = str(R.string.mobileDynamicColor),
                        subtitle = str(R.string.mobileDynamicColorDesc),
                        icon = FluxIcons.Palette,
                        checked = appearance.dynamicColor,
                        onCheckedChange = { on -> write { repo.setDynamicColor(on) } },
                    )
                }
                if (appearance.dynamicColor) {
                    custom { FluxDivider(startInset = 16.dp) }
                    custom(padded = true) {
                        WallpaperCard(onUseOwn = { write { repo.setDynamicColor(false) } })
                    }
                }
            }
        }
        flowItem(4, gate, "interface") {
            GlassSection(title = str(R.string.settingsGroupInterface)) {
                row(hasIcon = true) {
                    FluxListRow(
                        title = str(R.string.mobileSystemTextSize),
                        modifier = Modifier.settingsFocus("appearance.textSize"),
                        subtitle = str(R.string.mobileSystemTextSizeDesc),
                        icon = FluxIcons.Type,
                        chevron = true,
                        onClick = {
                            startSettingsIntent(context, overlays, Intent(Settings.ACTION_DISPLAY_SETTINGS), noApp)
                        },
                    )
                }
                row {
                    Column(
                        Modifier.fillMaxWidth().settingsFocus("appearance.fileSizeUnits"),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        FluxText(str(R.string.fileSizeUnits), style = FluxTheme.type.sm, color = FluxTheme.colors.ink)
                        FluxText(str(R.string.fileSizeUnitsDesc), style = FluxTheme.type.micro, color = FluxTheme.colors.inkMuted)
                        FluxSegmented(
                            options = listOf(
                                SegOption(FileSizeUnits.Legacy, str(R.string.fileSizeUnitsLegacy)),
                                SegOption(FileSizeUnits.Iec, str(R.string.fileSizeUnitsIec)),
                            ),
                            selected = appearance.fileSizeUnits,
                            onSelect = { units -> if (units != appearance.fileSizeUnits) write { repo.setFileSizeUnits(units) } },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun SubBlock(label: String, id: String, content: @Composable () -> Unit) {
    Column(Modifier.fillMaxWidth().settingsFocus(id), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            FluxText(
                label,
                style = FluxTheme.type.micro,
                color = FluxTheme.colors.inkMuted,
            )
            // 外观三键（模式 / 方案 / 自定义色）参与云同步：同行构件的 ☁︎ 标记
            FluxIcon(FluxIcons.Cloud, null, size = 13.dp, tint = FluxTheme.colors.inkFaint)
        }
        content()
    }
}

/** 跟随壁纸取色开启时的状态卡：壁纸色块 + 十六进制 + “改用自选色”。 */
@Composable
private fun WallpaperCard(onUseOwn: () -> Unit) {
    val context = LocalContext.current
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val wallpaper = remember(context) { wallpaperAccent(context) ?: Color.Unspecified }
    val effective = if (wallpaper == Color.Unspecified) c.accent else wallpaper
    val hex = remember(effective) { effective.toArgb().hexRgb() }
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Spacer(Modifier.size(40.dp).background(effective, CircleShape))
        FluxText(
            str(R.string.mobileDynamicColorActive, "hex" to hex),
            style = t.sm,
            color = c.inkMuted,
            modifier = Modifier.weight(1f),
        )
        FluxButton(
            text = str(R.string.mobileDynamicColorOff),
            onClick = onUseOwn,
            variant = ButtonVariant.Ghost,
            size = ButtonSize.Sm,
        )
    }
}

/** 本页可见行（与页面渲染同一判定：语言行仅 Android 13+ 有系统「应用语言」设置）。 */
internal fun appearanceSearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> {
    val crumb = ctx.crumb(R.string.settingsCatAppearance)
    return buildList {
        if (Build.VERSION.SDK_INT >= 33) {
            add(ctx.entry("appearance.language", SettingsPage.Appearance, "language", R.string.language, R.string.languageDesc, crumb, FluxIcons.Globe))
        }
        add(ctx.entry("appearance.mode", SettingsPage.Appearance, "theme", R.string.themeMode, null, crumb, FluxIcons.Palette))
        add(ctx.entry("appearance.color", SettingsPage.Appearance, "theme", R.string.themeColor, null, crumb, FluxIcons.Palette))
        add(
            ctx.entry(
                "appearance.textSize", SettingsPage.Appearance, "interface", R.string.mobileSystemTextSize,
                R.string.mobileSystemTextSizeDesc, ctx.crumb(R.string.settingsCatAppearance, R.string.settingsGroupInterface),
                FluxIcons.Type,
            ),
        )
        add(
            ctx.entry(
                "appearance.fileSizeUnits", SettingsPage.Appearance, "interface", R.string.fileSizeUnits,
                R.string.fileSizeUnitsDesc, ctx.crumb(R.string.settingsCatAppearance, R.string.settingsGroupInterface),
                FluxIcons.HardDrive,
            ),
        )
    }
}
