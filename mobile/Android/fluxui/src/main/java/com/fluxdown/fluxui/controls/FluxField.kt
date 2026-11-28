package com.fluxdown.fluxui.controls

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawOutline
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.fluxdown.fluxui.icons.FluxIcon
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.theme.FluxPressIndicationFactory
import com.fluxdown.fluxui.theme.FluxScaleGroup
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import com.fluxdown.fluxui.theme.fluxPressable

/** 字段框最小高度（单行输入 / 选择器共用）。 */
internal val FieldMinHeight = 44.dp

/** 字段框内文字的上下留白：15sp 正文 × 1.35 行高 ≈ 20dp，加上下 12dp 恰为 [FieldMinHeight]。 */
internal val FieldTextPadding = 12.dp

/**
 * 字段外框（FluxField / FluxSelect 共用）：标签行（micro +6% + 计数）→ 框（min 44 · r12 · glass2 + hairline + 高光）→ 提示行。
 * 聚焦：底 → glass3、边框 accent@80%、1dp 外环 accent@70%（键盘焦点另加 §10.6 实色环）；错误：coral 边框 + 外环。
 * [boxModifier] 作用于框本身（可点击 / 点按聚焦），位于背景绘制之前，所以按压缩放会连背景一起缩放。
 */
@Composable
internal fun FieldFrame(
    modifier: Modifier,
    label: String?,
    count: String?,
    hint: String?,
    error: String?,
    warning: String?,
    enabled: Boolean,
    boxModifier: Modifier = Modifier,
    trailing: (@Composable () -> Unit)? = null,
    content: @Composable RowScope.() -> Unit,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val motion = FluxTheme.motion
    val shape = FluxTheme.shapes.tile
    var focused by remember { mutableStateOf(false) }
    val focusAnim = animateFloatAsState(if (focused && enabled) 1f else 0f, motion.of(motion.fluid), label = "field-focus")
    val hasError = error != null

    Column(modifier.alpha(if (enabled) 1f else 0.5f)) {
        if (label != null || count != null) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .padding(start = 4.dp, end = 4.dp, bottom = 6.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.Bottom,
            ) {
                if (label != null) {
                    FluxText(
                        label,
                        style = remember(t) { t.micro.copy(letterSpacing = 0.06.em) },
                        color = c.inkMuted,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                } else {
                    Box(Modifier.size(0.dp))
                }
                if (count != null) FluxText(count, style = t.monoS.copy(letterSpacing = 0.em), color = c.inkFaint, maxLines = 1)
            }
        }
        Box(
            Modifier
                .fillMaxWidth()
                .fluxFocusRing(shape)
                .onFocusChanged { focused = it.hasFocus },
        ) {
            Row(
                Modifier
                    .fillMaxWidth()
                    .then(boxModifier)
                    .drawWithCache {
                        val outline = shape.createOutline(size, layoutDirection, this)
                        val ring = outline.inflate(0.75.dp.toPx())
                        val hw = 0.5.dp.toPx()
                        val ringW = 1.dp.toPx()
                        val hl = Brush.verticalGradient(0f to c.highlight, 0.18f to Color.Transparent)
                        onDrawBehind {
                            val f = focusAnim.value
                            drawOutline(outline, lerp(c.glass2, c.glass3, f))
                            val border = if (hasError) c.coral.copy(alpha = 0.7f) else lerp(c.hairline, c.accent.copy(alpha = 0.8f), f)
                            drawOutline(outline, border, style = Stroke(hw))
                            drawOutline(outline, hl, style = Stroke(hw))
                            if (hasError) {
                                drawOutline(ring, c.coral.copy(alpha = 0.5f), style = Stroke(ringW))
                            } else if (f > 0.001f) {
                                drawOutline(ring, c.accent.copy(alpha = 0.7f * f), style = Stroke(ringW))
                            }
                        }
                    }
                    .heightIn(min = FieldMinHeight)
                    .padding(start = 14.dp, end = if (trailing != null) 4.dp else 14.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                content()
                trailing?.invoke()
            }
        }
        val line = error ?: warning ?: hint
        if (line != null) {
            FluxText(
                line,
                style = remember(t) { t.sm.copy(lineHeight = 1.4.em) },
                color = when {
                    hasError -> c.coralText
                    warning != null -> c.amberText
                    else -> c.inkFaint
                },
                modifier = Modifier.padding(start = 6.dp, end = 6.dp, top = 5.dp),
            )
        }
    }
}

/**
 * 输入框（§12.21）：基于 `BasicTextField`，聚焦 accent 环，光标 `accentHi`，占位符 `inkFaint`。
 * 标签 / 输入 / 提示都可换行，输入框只设 `min 44dp`。[mono] = 等宽 13.5（URL / 路径 / 哈希）。
 * [error] 非空时进入错误态并替换提示；[warning] 为琥珀色非阻断提示（如“已调整为 n”）。
 * 多行时至少显示 [rows] 行，超过 [maxRows] 行后内部滚动；两者相同则高度固定。[trailing] 建议放 [FluxFieldAction]。
 */
@Composable
fun FluxField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    label: String? = null,
    placeholder: String? = null,
    prefix: String? = null,
    suffix: String? = null,
    hint: String? = null,
    error: String? = null,
    warning: String? = null,
    count: String? = null,
    mono: Boolean = false,
    singleLine: Boolean = true,
    rows: Int = 1,
    maxRows: Int = maxOf(rows, 8),
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    readOnly: Boolean = false,
    enabled: Boolean = true,
    onFocusChange: ((Boolean) -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    val c = FluxTheme.colors
    val t = FluxTheme.type
    val focusRequester = remember { FocusRequester() }
    val focusCb = rememberUpdatedState(onFocusChange)
    val inputStyle = remember(t, mono, c) {
        val base: TextStyle = if (mono) {
            t.sized(t.mono, 13.5f, FluxScaleGroup.Ks).copy(letterSpacing = (-0.01f).em, lineHeight = 1.35.em)
        } else {
            t.body.copy(lineHeight = 1.35.em)
        }
        base.copy(color = c.ink)
    }
    val cursor = remember(c, readOnly) { SolidColor(if (readOnly) Color.Transparent else c.accentHi) }
    val prefixStyle = remember(t) { t.sm }
    val suffixStyle = remember(t) { t.weight(t.sm, 400, mono = true) }

    FieldFrame(
        modifier = modifier,
        label = label,
        count = count,
        hint = hint,
        error = error,
        warning = warning,
        enabled = enabled,
        boxModifier = Modifier.pointerInput(enabled, readOnly) {
            if (enabled) detectTapGestures { focusRequester.requestFocus() }
        },
        trailing = trailing,
    ) {
        if (prefix != null) FluxText(prefix, style = prefixStyle, color = c.inkMuted, maxLines = 1)
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            modifier = Modifier
                .weight(1f)
                .focusRequester(focusRequester)
                .onFocusChanged { focusCb.value?.invoke(it.isFocused) }
                .semantics {
                    val d = label ?: placeholder
                    if (d != null) contentDescription = d
                    if (error != null) this.error(error)
                },
            enabled = enabled,
            readOnly = readOnly,
            textStyle = inputStyle,
            keyboardOptions = keyboardOptions,
            keyboardActions = keyboardActions,
            singleLine = singleLine,
            minLines = if (singleLine) 1 else rows,
            maxLines = if (singleLine) 1 else maxRows,
            visualTransformation = visualTransformation,
            cursorBrush = cursor,
            decorationBox = { inner ->
                Box(Modifier.padding(vertical = FieldTextPadding)) {
                    if (value.isEmpty() && placeholder != null) {
                        FluxText(placeholder, style = inputStyle, color = c.inkFaint, maxLines = if (singleLine) 1 else rows)
                    }
                    inner()
                }
            },
        )
        if (suffix != null) FluxText(suffix, style = suffixStyle, color = c.inkMuted, maxLines = 1)
    }
}

/** 字段尾部的 32dp 小按钮（清除 / 显隐密码 / 粘贴），独立命中区 48dp，按压 `scale .9`。 */
@Composable
fun FluxFieldAction(
    icon: ImageVector,
    contentDescription: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = FluxTheme.colors
    Box(
        modifier
            .size(48.dp)
            .fluxPressable(onClick = onClick, scale = 0.9f, role = Role.Button)
            .semantics { this.contentDescription = contentDescription },
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(32.dp), contentAlignment = Alignment.Center) {
            FluxIcon(icon, null, size = 18.dp, tint = c.inkMuted)
        }
    }
}

/**
 * 密码框：尾部眼睛按钮切换明文（默认遮蔽，无障碍树里不泄露明文）。
 */
@Composable
fun FluxPasswordField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    label: String? = null,
    placeholder: String? = null,
    hint: String? = null,
    error: String? = null,
    keyboardOptions: KeyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    enabled: Boolean = true,
    onFocusChange: ((Boolean) -> Unit)? = null,
) {
    var visible by rememberSaveable { mutableStateOf(false) }
    FluxField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier,
        label = label,
        placeholder = placeholder,
        hint = hint,
        error = error,
        mono = visible,
        keyboardOptions = keyboardOptions,
        keyboardActions = keyboardActions,
        visualTransformation = if (visible) VisualTransformation.None else PasswordVisualTransformation(),
        enabled = enabled,
        onFocusChange = onFocusChange,
        trailing = {
            FluxFieldAction(
                icon = if (visible) FluxIcons.EyeOff else FluxIcons.Eye,
                contentDescription = if (visible) "隐藏密码" else "显示密码",
                onClick = { visible = !visible },
            )
        },
    )
}

/**
 * 数值输入框（设置域 §1.3）：数字键盘、非法字符即时过滤、失焦 / 完成键提交，越界钳制并以琥珀色提示“已调整为 n”。
 * [value] 外部变化会重置草稿。适合大范围 / 自由输入；小范围整数请用 [FluxStepper]。
 */
@Composable
fun FluxNumberField(
    value: Long,
    onValueChange: (Long) -> Unit,
    range: LongRange,
    modifier: Modifier = Modifier,
    label: String? = null,
    placeholder: String? = null,
    suffix: String? = null,
    hint: String? = null,
    mono: Boolean = true,
    enabled: Boolean = true,
    adjustedHint: (Long) -> String = { "已调整为 $it" },
) {
    val haptics = FluxTheme.haptics
    val focusManager = LocalFocusManager.current
    var draft by remember(value) { mutableStateOf(value.toString()) }
    var adjusted by remember { mutableStateOf<Long?>(null) }
    val valueNow = rememberUpdatedState(value)
    val allowNegative = range.first < 0
    val focusSeen = remember { BooleanArray(1) }

    fun commit() {
        val parsed = draft.toLongOrNull()
        if (parsed == null) {
            draft = valueNow.value.toString()
            return
        }
        val clamped = parsed.coerceIn(range.first, range.last)
        if (clamped != parsed) {
            adjusted = clamped
            haptics.reject()
        }
        draft = clamped.toString()
        if (clamped != valueNow.value) onValueChange(clamped)
    }

    FluxField(
        value = draft,
        onValueChange = { s ->
            adjusted = null
            draft = s.filterIndexed { i, ch -> ch.isDigit() || (allowNegative && i == 0 && ch == '-') }.take(18)
        },
        modifier = modifier,
        label = label,
        placeholder = placeholder,
        suffix = suffix,
        hint = hint,
        warning = adjusted?.let(adjustedHint),
        mono = mono,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = {
            focusSeen[0] = false
            commit()
            focusManager.clearFocus()
        }),
        enabled = enabled,
        // 挂载时 Compose 会先回调一次 isFocused = false：只在真正获得过焦点后的失焦才提交，
        // 否则仅仅显示一个越界的主机值就会被钳位并写回（用户什么都没改）。
        onFocusChange = { focused ->
            if (focused) {
                focusSeen[0] = true
            } else if (focusSeen[0]) {
                focusSeen[0] = false
                commit()
            }
        },
    )
}
