package com.fluxdown.app.feature.settings

import androidx.annotation.StringRes
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.R
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.SettingsCatalog
import com.fluxdown.core.format.FileSizeUnits
import com.fluxdown.core.protocol.SettingsForm
import com.fluxdown.core.protocol.preferences
import com.fluxdown.fluxui.chrome.FluxPill
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldRow
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxNumberField
import com.fluxdown.fluxui.controls.FluxPasswordField
import com.fluxdown.fluxui.controls.FluxSelect
import com.fluxdown.fluxui.controls.FluxStepperRow
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import java.util.Locale
import kotlin.math.roundToLong

/*
 * 设置页通用行构件（所有设置分类页共用，禁止各页另造一套）。
 * - 读：[SettingsCtx.form]（乐观值 ?: 主机值 ?: 目录默认）；写：[SettingsCtx.editor]（唯一写入口）。
 * - 每行：☁︎ 云同步标记（[SettingsCatalog.isSynced]）、失败说明（[ConfigEditor.failures]）、
 *   搜索定位高亮（[settingsFocus]，`id` = [SettingsEntry.id]）。
 * - 只读（断线）时控件置灰；daemon 键在配置未加载时整行不渲染（[SettingsForm.has]）。
 */

/** 一次组合内的设置页上下文。 */
@Stable
internal class SettingsCtx(
    val editor: ConfigEditor,
    val form: SettingsForm,
    val readOnly: Boolean,
    /** 当前主机是本机（进程内引擎）：本机路径 / 系统集成类的行据此分支。 */
    val isLocalHost: Boolean,
    val openPicker: (PickerSpec) -> Unit,
) {
    fun synced(key: String): Boolean = SettingsCatalog.isSynced(key)
}

/** 绑定当前主机状态与全局 [ConfigEditor]；[openPicker] 由页面的 [PickerSheetHost] 承接。 */
@Composable
internal fun rememberSettingsCtx(openPicker: (PickerSpec) -> Unit): SettingsCtx {
    val editor = LocalConfigEditor.current
    val host = hostState()
    val container = LocalAppContainer.current
    val hostRef by container.host.collectAsStateWithLifecycle()
    val config by remember { derivedStateOf { host.value.config } }
    val prefs by remember { derivedStateOf { host.value.preferences.values } }
    val readOnly by remember { derivedStateOf { host.value.isReadOnly } }
    val form = remember(editor, config, prefs) { SettingsForm(config, editor.optimistic, prefs, editor.optimisticPrefs) }
    val latestPicker by rememberUpdatedState(openPicker)
    return remember(editor, form, readOnly, hostRef) {
        SettingsCtx(editor, form, readOnly, hostRef is HostRef.Local) { latestPicker(it) }
    }
}

// ───────────────────────────── 搜索定位 ─────────────────────────────

/**
 * 设置搜索的定位请求（全局单例：搜索结果 → 目标页滚动到所在分组并脉冲高亮该行）。
 * 由 [SettingsPageFrame] 消费 [pendingItemKey]（滚动），行经 [settingsFocus] 消费 [highlight]。
 */
@Stable
internal class SettingsFocus {
    var pendingItemKey by mutableStateOf<String?>(null)
    var highlight by mutableStateOf<String?>(null)

    fun request(entry: SettingsEntry) {
        pendingItemKey = entry.itemKey
        highlight = entry.id
    }
}

internal val LocalSettingsFocus = staticCompositionLocalOf<SettingsFocus> { error("SettingsFocus missing") }

/** 行被搜索命中时短暂脉冲强调底色（两次闪烁后复位）。 */
@Composable
internal fun Modifier.settingsFocus(id: String): Modifier {
    val focus = LocalSettingsFocus.current
    val target = focus.highlight == id
    val alpha = remember(id) { Animatable(0f) }
    val tint = FluxTheme.colors.accentLo
    LaunchedEffect(target) {
        if (!target) return@LaunchedEffect
        repeat(2) {
            alpha.animateTo(1f, tween(220))
            alpha.animateTo(0f, tween(420))
        }
        if (focus.highlight == id) focus.highlight = null
    }
    return drawBehind { if (alpha.value > 0f) drawRect(tint.copy(alpha = tint.alpha * alpha.value)) }
}

// ───────────────────────────── 行构件 ─────────────────────────────

/** 行内失败说明（写入被拒 / 主机报错，3 秒淡出）。 */
@Composable
internal fun SettingFailure(ctx: SettingsCtx, key: String) {
    val message = ctx.editor.failures[key] ?: return
    FluxText(
        message,
        style = FluxTheme.type.sm,
        color = FluxTheme.colors.coralText,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 10.dp),
    )
}

/** 带定位高亮与失败说明的行容器（自定义行也应包在里面）。 */
@Composable
internal fun SettingRowBox(ctx: SettingsCtx, id: String, key: String?, content: @Composable () -> Unit) {
    Column(Modifier.fillMaxWidth().settingsFocus(id)) {
        content()
        if (key != null) SettingFailure(ctx, key)
    }
}

internal fun GlassSectionScope.settingSwitch(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int? = null,
    id: String = key,
    enabled: Boolean = true,
    onChange: ((Boolean) -> Unit)? = null,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            FluxSwitchRow(
                title = str(title),
                subtitle = desc?.let { str(it) },
                checked = ctx.form.bool(key),
                cloud = ctx.synced(key),
                enabled = enabled && !ctx.readOnly,
                onCheckedChange = { on -> onChange?.invoke(on) ?: ctx.editor.set(key, SettingsForm.wire(on)) },
            )
        }
    }
}

internal fun GlassSectionScope.settingStepper(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    range: IntRange,
    default: Int,
    @StringRes zeroLabel: Int? = null,
    @StringRes minusOneLabel: Int? = null,
    id: String = key,
    format: ((Int) -> String)? = null,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            val zero = zeroLabel?.let { str(it) }
            val minusOne = minusOneLabel?.let { str(it) }
            FluxStepperRow(
                title = str(title),
                subtitle = desc?.let { str(it) },
                value = ctx.form.int(key, default).coerceIn(range),
                onValueChange = { ctx.editor.set(key, it.toString()) },
                range = range,
                format = { v ->
                    when {
                        v == 0 && zero != null -> zero
                        v == -1 && minusOne != null -> minusOne
                        format != null -> format(v)
                        else -> v.toString()
                    }
                },
                editable = true,
                enabled = !ctx.readOnly,
                cloud = ctx.synced(key),
            )
        }
    }
}

/** 单选行：点按打开选择器 Sheet；值显示为所选项文案。 */
internal fun GlassSectionScope.settingChoice(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    id: String = key,
    options: @Composable () -> List<SelectOption<String>>,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            val items = options()
            val current = ctx.form.string(key, SettingsCatalog.defaultWire(key))
            val name = str(title)
            FluxListRow(
                title = name,
                subtitle = desc?.let { str(it) },
                value = items.firstOrNull { it.value == current }?.label ?: current,
                chevron = true,
                cloud = ctx.synced(key),
                enabled = !ctx.readOnly,
                onClick = { ctx.openPicker(PickerSpec(name, items, current) { ctx.editor.set(key, it) }) },
            )
        }
    }
}

/** 整数输入行（越界自动钳位并提示「已调整为 n」）。 */
internal fun GlassSectionScope.settingNumber(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    range: LongRange,
    default: Long,
    @StringRes unit: Int? = null,
    unitLabel: String? = null,
    id: String = key,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            val template = stringResource(R.string.mobileAdjustedTo)
            FluxFieldRow(title = str(title), subtitle = desc?.let { str(it) }, cloud = ctx.synced(key)) {
                FluxNumberField(
                    value = ctx.form.long(key, default),
                    onValueChange = { ctx.editor.set(key, it.toString()) },
                    range = range,
                    hint = unitLabel ?: unit?.let { str(it) },
                    enabled = !ctx.readOnly,
                    adjustedHint = { template.fill("n" to it) },
                )
            }
        }
    }
}

/** 限速行（字节/秒，1024 进制；0 = 不限制）。 */
internal fun GlassSectionScope.settingSpeed(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    id: String = key,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            val name = str(title)
            FluxFieldRow(title = name, subtitle = desc?.let { str(it) }, cloud = ctx.synced(key)) {
                SpeedLimitControl(
                    bytes = ctx.form.long(key, 0L),
                    onChange = { ctx.editor.set(key, it.toString()) },
                    enabled = !ctx.readOnly,
                    name = name,
                    openPicker = ctx.openPicker,
                )
            }
        }
    }
}

/** 文本行：失焦 / 完成键 / 离开页面时提交；[secret] = 密码框。[multiline] 用于 tracker / 服务器列表等多行文本。 */
internal fun GlassSectionScope.settingText(
    ctx: SettingsCtx,
    key: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    @StringRes placeholder: Int? = null,
    id: String = key,
    mono: Boolean = true,
    secret: Boolean = false,
    multiline: Boolean = false,
) {
    if (!ctx.form.has(key)) return
    row {
        SettingRowBox(ctx, id, key) {
            FluxFieldRow(title = str(title), subtitle = desc?.let { str(it) }, cloud = ctx.synced(key)) {
                CommitTextField(
                    value = ctx.form.string(key),
                    onCommit = { ctx.editor.set(key, it) },
                    enabled = !ctx.readOnly,
                    placeholder = placeholder?.let { str(it) },
                    mono = mono,
                    secret = secret,
                    multiline = multiline,
                )
            }
        }
    }
}

// ───────────────────────────── 文本 / 限速控件 ─────────────────────────────

/** 失焦 / 完成键 / 离开页面时提交的文本框（路径、UA 等不宜逐字写入主机的字段）。 */
@Composable
internal fun CommitTextField(
    value: String,
    onCommit: (String) -> Unit,
    enabled: Boolean,
    placeholder: String?,
    mono: Boolean = true,
    secret: Boolean = false,
    multiline: Boolean = false,
    keyboardType: KeyboardType = KeyboardType.Text,
) {
    var draft by remember(value) { mutableStateOf(value) }
    val latestDraft by rememberUpdatedState(draft)
    val latestValue by rememberUpdatedState(value)
    val latestCommit by rememberUpdatedState(onCommit)
    val focusManager = LocalFocusManager.current

    DisposableEffect(Unit) {
        onDispose { if (latestDraft != latestValue) latestCommit(latestDraft) }
    }
    val commitOnBlur: (Boolean) -> Unit = { focused -> if (!focused && draft != value) onCommit(draft) }
    if (secret) {
        FluxPasswordField(
            value = draft,
            onValueChange = { draft = it },
            placeholder = placeholder,
            enabled = enabled,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = {
                if (draft != value) onCommit(draft)
                focusManager.clearFocus()
            }),
            onFocusChange = commitOnBlur,
        )
    } else {
        FluxField(
            value = draft,
            onValueChange = { draft = it },
            placeholder = placeholder,
            mono = mono,
            enabled = enabled,
            singleLine = !multiline,
            rows = if (multiline) 4 else 1,
            keyboardOptions = KeyboardOptions(
                keyboardType = keyboardType,
                imeAction = if (multiline) ImeAction.Default else ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = {
                if (draft != value) onCommit(draft)
                focusManager.clearFocus()
            }),
            onFocusChange = commitOnBlur,
        )
    }
}

internal enum class SpeedUnit(private val power: Int, val factor: Long) {
    KB(1, 1024L),
    MB(2, 1024L * 1024L),
    GB(3, 1024L * 1024L * 1024L);

    fun label(units: FileSizeUnits): String = units.rateLabel(power)
}

/** 快捷档位：0 = 不限制，其余 (数值, 单位)。 */
private val SpeedPresets: List<Pair<Int, SpeedUnit>> = listOf(
    0 to SpeedUnit.MB,
    512 to SpeedUnit.KB,
    1 to SpeedUnit.MB,
    2 to SpeedUnit.MB,
    5 to SpeedUnit.MB,
    10 to SpeedUnit.MB,
    20 to SpeedUnit.MB,
)

/** 在 [unit] 中最多 2 位小数即可精确表示。 */
private fun representable(bytes: Long, unit: SpeedUnit): Boolean = speedBytes(speedText(bytes, unit), unit) == bytes

/** 首选单位能精确表示就用它；否则取能精确表示的最大单位；都不行回落 KB/s。 */
private fun effectiveUnit(bytes: Long, preferred: SpeedUnit): SpeedUnit {
    if (bytes <= 0L || representable(bytes, preferred)) return preferred
    return listOf(SpeedUnit.GB, SpeedUnit.MB, SpeedUnit.KB).firstOrNull { representable(bytes, it) } ?: SpeedUnit.KB
}

private fun speedText(bytes: Long, unit: SpeedUnit): String {
    if (bytes <= 0L) return ""
    val s = String.format(Locale.ROOT, "%.2f", bytes.toDouble() / unit.factor).trimEnd('0').trimEnd('.')
    return s.ifEmpty { "0" }
}

private fun speedBytes(text: String, unit: SpeedUnit): Long {
    val v = text.trim().replace(',', '.').toDoubleOrNull() ?: return 0L
    val raw = v.coerceAtLeast(0.0) * unit.factor
    return if (raw >= Long.MAX_VALUE.toDouble()) Long.MAX_VALUE else raw.roundToLong()
}

/** 字段 + 单位选择 + 快捷档位；基数 1024，两位小数；0 = 不限制。单位仅在本页会话内记忆。 */
@Composable
internal fun SpeedLimitControl(
    bytes: Long,
    onChange: (Long) -> Unit,
    enabled: Boolean,
    name: String,
    openPicker: (PickerSpec) -> Unit,
) {
    val container = LocalAppContainer.current
    val appearance by container.appearanceState.collectAsStateWithLifecycle()
    val units = appearance?.fileSizeUnits
    var preferred by rememberSaveable(name) { mutableStateOf(SpeedUnit.MB) }
    val unit = effectiveUnit(bytes, preferred)
    var draft by remember(bytes, unit) { mutableStateOf(speedText(bytes, unit)) }
    val focusManager = LocalFocusManager.current
    val unitTitle = str(R.string.mobileSpeedUnit)
    val unlimited = str(R.string.mobileSpeedUnlimited)

    fun commit(text: String, u: SpeedUnit) {
        val next = speedBytes(text, u)
        if (next != bytes) onChange(next)
    }

    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        FluxField(
            value = draft,
            onValueChange = { s ->
                var dot = false
                draft = s.filter { ch ->
                    when {
                        ch.isDigit() -> true
                        (ch == '.' || ch == ',') && !dot -> {
                            dot = true
                            true
                        }
                        else -> false
                    }
                }.take(12)
            },
            modifier = Modifier.weight(1f),
            placeholder = unlimited,
            mono = true,
            enabled = enabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = {
                commit(draft, unit)
                focusManager.clearFocus()
            }),
            onFocusChange = { focused -> if (!focused) commit(draft, unit) },
        )
        if (units == null) return@Row
        FluxSelect(
            value = unit.label(units),
            onClick = {
                openPicker(
                    PickerSpec(
                        title = unitTitle,
                        options = SpeedUnit.entries.map { SelectOption(it.name, it.label(units)) },
                        selected = unit.name,
                        onSelect = { id ->
                            val picked = SpeedUnit.valueOf(id)
                            preferred = picked
                            // 切换单位保留已输入的数字
                            commit(draft, picked)
                        },
                    ),
                )
            },
            modifier = Modifier.width(120.dp),
            placeholder = unit.label(units),
            enabled = enabled,
        )
    }
    if (units == null) return
    Row(
        Modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        for ((n, u) in SpeedPresets) {
            val value = n * u.factor
            FluxPill(
                text = if (n == 0) unlimited else "$n ${u.label(units)}",
                selected = bytes == value,
                onClick = {
                    if (enabled) {
                        if (n != 0) preferred = u
                        if (value != bytes) onChange(value)
                    }
                },
                small = true,
                toggleable = false,
            )
        }
    }
}
