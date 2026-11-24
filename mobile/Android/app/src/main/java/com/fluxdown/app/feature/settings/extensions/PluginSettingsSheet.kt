package com.fluxdown.app.feature.settings.extensions

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.PickerSheetHost
import com.fluxdown.app.feature.settings.RemoteDirectoryPickerSheet
import com.fluxdown.app.feature.settings.rememberLastNonNull
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.PluginFieldError
import com.fluxdown.core.protocol.PluginSettingField
import com.fluxdown.core.protocol.PluginSettings
import com.fluxdown.core.protocol.callUnit
import com.fluxdown.core.protocol.pluginUpdateSettingsParams
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldAction
import com.fluxdown.fluxui.controls.FluxPasswordField
import com.fluxdown.fluxui.controls.FluxSelect
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.launch

/**
 * 设置表单 / 登录 / 加载错误详情 / 反馈 Sheet 的路由：每次打开是一个新实例（刻意不用 data class，
 * 以实例同一性区分「再次打开」），由页面持有、[PluginSheetHost] 呈现；按 identity 读取最新插件。
 */
internal sealed class PluginSheet(val identity: String) {
    class Settings(identity: String) : PluginSheet(identity)
    class Auth(identity: String) : PluginSheet(identity)
    class LoadError(identity: String) : PluginSheet(identity)
    class Feedback(identity: String) : PluginSheet(identity)
}

@Composable
internal fun PluginSheetHost(sheet: PluginSheet?, plugins: List<PluginDto>, onDismiss: () -> Unit) {
    val shown = rememberLastNonNull(sheet)
    val plugin = shown?.let { s -> plugins.firstOrNull { it.identity == s.identity } }
    // 打开期间插件被卸载：收起。
    LaunchedEffect(sheet, plugin == null) { if (sheet != null && plugin == null) onDismiss() }
    if (shown == null || plugin == null) return
    when (shown) {
        is PluginSheet.Settings -> PluginSettingsSheet(shown, visible = sheet === shown, plugin = plugin, onDismiss = onDismiss)
        is PluginSheet.Auth -> PluginAuthSheet(shown, visible = sheet === shown, plugin = plugin, onDismiss = onDismiss)
        is PluginSheet.LoadError -> PluginLoadErrorSheet(visible = sheet === shown, plugin = plugin, onDismiss = onDismiss)
        is PluginSheet.Feedback -> PluginFeedbackSheet(shown, visible = sheet === shown, plugin = plugin, onDismiss = onDismiss)
    }
}

// ───────────────────────────── 设置表单 ─────────────────────────────

/** 表单在打开瞬间建立，之后快照更新不覆盖用户输入。 */
@Stable
private class SettingsFormState(plugin: PluginDto) {
    val values = mutableStateMapOf<String, String>().apply {
        putAll(PluginSettings.initialValues(plugin.settings, plugin.settingsValues))
    }
    val errors = mutableStateMapOf<String, PluginFieldError>()
    var serverError by mutableStateOf<String?>(null)
    var saving by mutableStateOf(false)

    fun set(key: String, value: String) {
        values[key] = value
        errors.remove(key)
    }
}

/** 连续的开关合并进同一个分组；其余字段各占一个分组。 */
private sealed interface FieldBlock {
    val key: String

    class Toggles(val fields: List<PluginSettingField>) : FieldBlock {
        override val key: String = "toggles:" + fields.first().key
    }

    class Single(val field: PluginSettingField) : FieldBlock {
        override val key: String = field.key
    }
}

private fun blocksOf(fields: List<PluginSettingField>): List<FieldBlock> {
    val result = ArrayList<FieldBlock>()
    val toggles = ArrayList<PluginSettingField>()
    for (field in fields) {
        if (field.widget == "toggle") {
            toggles += field
        } else {
            if (toggles.isNotEmpty()) {
                result += FieldBlock.Toggles(toggles.toList())
                toggles.clear()
            }
            result += FieldBlock.Single(field)
        }
    }
    if (toggles.isNotEmpty()) result += FieldBlock.Toggles(toggles.toList())
    return result
}

/**
 * S11.4 · 插件设置表单：按 manifest `SettingFieldDto` 动态生成控件，提交前做
 * required / number / min-max / select 前置校验，全部通过才发起 `daemon.plugin.updateSettings`
 * （`pattern` 由主机校验，失败文案显示在顶部）。
 */
@Composable
private fun PluginSettingsSheet(sheet: PluginSheet.Settings, visible: Boolean, plugin: PluginDto, onDismiss: () -> Unit) {
    val container = LocalAppContainer.current
    val env = rememberExtensionsEnv()
    val haptics = FluxTheme.haptics
    val focusManager = LocalFocusManager.current
    val form = remember(sheet) { SettingsFormState(plugin) }
    var picker by remember(sheet) { mutableStateOf<PickerSpec?>(null) }
    var folderKey by remember(sheet) { mutableStateOf<String?>(null) }
    val blocks = remember(plugin.settings) { blocksOf(plugin.settings) }
    val title = str(R.string.pluginSettingsDialogTitle, "name" to plugin.name)

    fun save() {
        if (form.saving) return
        focusManager.clearFocus()
        form.serverError = null
        val found = PluginSettings.validateAll(plugin.settings, form.values)
        form.errors.clear()
        form.errors.putAll(found)
        if (found.isNotEmpty()) {
            haptics.reject()
            return
        }
        form.saving = true
        val entries = PluginSettings.submitEntries(plugin.settings, form.values)
        container.appScope.launch {
            try {
                container.session.callUnit(HostMethod.daemonPluginUpdateSettings, pluginUpdateSettingsParams(plugin.identity, entries))
                onDismiss()
            } catch (e: HostException) {
                form.serverError = env.error(e)
                haptics.reject()
            } finally {
                form.saving = false
            }
        }
    }

    FluxPortal {
        FluxSheet(
            visible = visible,
            onDismissRequest = onDismiss,
            detent = FluxSheetDetent.Full,
            dismissible = !form.saving,
            title = title,
            header = { FluxSheetHeader(title = title, onClose = if (form.saving) null else onDismiss) },
            footer = {
                FluxSheetFooter {
                    FluxButton(str(R.string.cancel), onClick = onDismiss, modifier = Modifier.weight(1f), enabled = !form.saving)
                    FluxButton(
                        str(R.string.pluginSettingsSaveButton),
                        onClick = ::save,
                        modifier = Modifier.weight(1f),
                        variant = ButtonVariant.Primary,
                        loading = form.saving,
                    )
                }
            },
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                form.serverError?.let {
                    FluxBanner(str(R.string.pluginSettingsSaveFailed, "message" to it), kind = FluxBannerKind.Error, slim = true)
                }
                for (block in blocks) {
                    when (block) {
                        is FieldBlock.Toggles -> GlassSection {
                            for (field in block.fields) row { ToggleRow(field, form, env) }
                        }
                        is FieldBlock.Single -> GlassSection(
                            title = block.field.displayTitle,
                            footer = block.field.description.ifEmpty { null },
                        ) {
                            custom(padded = true) {
                                FieldControl(
                                    block.field, form, env,
                                    openPicker = { picker = it },
                                    openFolder = { folderKey = it },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    PickerSheetHost(picker = picker, onDismiss = { picker = null })
    val folderField = folderKey?.let { key -> plugin.settings.firstOrNull { it.key == key } }
    RemoteDirectoryPickerSheet(
        visible = folderField != null,
        startPath = folderField?.let { form.values[it.key] }.orEmpty(),
        onPick = { path ->
            folderKey?.let { form.set(it, path) }
            folderKey = null
        },
        onDismiss = { folderKey = null },
    )
}

@Composable
private fun ToggleRow(field: PluginSettingField, form: SettingsFormState, env: ExtensionsEnv) {
    Column {
        FluxSwitchRow(
            title = field.displayTitle,
            subtitle = field.description.ifEmpty { null },
            checked = form.values[field.key] == "true",
            onCheckedChange = { form.set(field.key, if (it) "true" else "false") },
            enabled = !form.saving,
        )
        HelperScriptButton(field, env, Modifier.padding(horizontal = 16.dp, vertical = 4.dp))
        form.errors[field.key]?.let { ErrorLine(it, Modifier.padding(horizontal = 16.dp, vertical = 6.dp)) }
    }
}

@Composable
private fun HelperScriptButton(field: PluginSettingField, env: ExtensionsEnv, modifier: Modifier = Modifier) {
    val script = field.helperScript
    if (script.isNullOrEmpty()) return
    FluxButton(
        text = field.helperLabel?.takeIf { it.isNotEmpty() } ?: str(R.string.pluginCopyHelperScript),
        onClick = {
            env.context.copyPlainText(script)
            env.toast(env.text(R.string.pluginHelperScriptCopied), FluxToastKind.Success)
        },
        modifier = modifier,
        variant = ButtonVariant.Ghost,
        icon = FluxIcons.Copy,
    )
}

@Composable
private fun ErrorLine(error: PluginFieldError, modifier: Modifier = Modifier) {
    FluxText(errorText(error), modifier, style = FluxTheme.type.sm, color = FluxTheme.colors.coralText)
}

@Composable
private fun errorText(error: PluginFieldError): String = when (error) {
    PluginFieldError.Required -> str(R.string.pluginErrRequired)
    PluginFieldError.Number -> str(R.string.pluginErrNumber)
    is PluginFieldError.Min -> str(R.string.pluginErrMin, "min" to error.min)
    is PluginFieldError.Max -> str(R.string.pluginErrMax, "max" to error.max)
    PluginFieldError.Select -> str(R.string.pluginErrSelect)
    is PluginFieldError.Server -> error.message
}

@Composable
private fun FieldControl(
    field: PluginSettingField,
    form: SettingsFormState,
    env: ExtensionsEnv,
    openPicker: (PickerSpec) -> Unit,
    openFolder: (String) -> Unit,
) {
    val value = form.values[field.key].orEmpty()
    val error = form.errors[field.key]?.let { errorText(it) }
    val enabled = !form.saving
    val title = field.displayTitle
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        when (field.widget) {
            "select" -> {
                val known = field.options.firstOrNull { it.value == value }
                // 已保存值不在选项里时照原样显示（提交前会被 select 校验拦下并提示）。
                val shownValue = known?.label ?: value.ifEmpty { null }
                FluxSelect(
                    value = shownValue,
                    onClick = {
                        val options = field.options.map { SelectOption(it.value, it.label) }
                            .let { if (known == null && value.isNotEmpty()) it + SelectOption(value, value) else it }
                        openPicker(PickerSpec(title, options, value) { form.set(field.key, it) })
                    },
                    placeholder = str(R.string.pluginSelectPlaceholder),
                    error = error,
                    enabled = enabled,
                )
            }
            "textarea" -> FluxField(
                value = value,
                onValueChange = { form.set(field.key, it) },
                singleLine = false,
                rows = 3,
                error = error,
                enabled = enabled,
            )
            "password" -> FluxPasswordField(
                value = value,
                onValueChange = { form.set(field.key, it) },
                error = error,
                enabled = enabled,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
            )
            "folder" -> FluxField(
                value = value,
                onValueChange = { form.set(field.key, it) },
                placeholder = str(R.string.pluginFolderPickPlaceholder),
                mono = true,
                error = error,
                enabled = enabled,
                trailing = {
                    FluxFieldAction(FluxIcons.FolderOpen, str(R.string.pluginFolderPickPlaceholder), { openFolder(field.key) })
                },
            )
            else -> {
                val isNumber = field.settingType == "number"
                FluxField(
                    value = value,
                    onValueChange = { form.set(field.key, it) },
                    placeholder = if (isNumber) PluginSettings.rangeHint(field) else field.defaultValue,
                    error = error,
                    enabled = enabled,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = when {
                            !isNumber -> KeyboardType.Text
                            (field.min ?: -1.0) >= 0 -> KeyboardType.Decimal
                            else -> KeyboardType.Text
                        },
                        imeAction = ImeAction.Done,
                    ),
                    keyboardActions = KeyboardActions.Default,
                )
            }
        }
        HelperScriptButton(field, env)
    }
}

// ───────────────────────────── 加载失败详情 ─────────────────────────────

/** 加载失败详情（`pluginLoadErrorTitle` / `pluginLoadErrorBody` + 复制）。 */
@Composable
private fun PluginLoadErrorSheet(visible: Boolean, plugin: PluginDto, onDismiss: () -> Unit) {
    val env = rememberExtensionsEnv()
    FluxPortal {
        FluxSheet(
            visible = visible,
            onDismissRequest = onDismiss,
            title = str(R.string.pluginLoadErrorTitle),
            header = {
                FluxSheetHeader(
                    title = str(R.string.pluginLoadErrorTitle),
                    subtitle = str(R.string.pluginLoadErrorBody),
                    onClose = onDismiss,
                )
            },
            footer = {
                FluxSheetFooter {
                    FluxButton(
                        str(R.string.pluginLoadErrorCopy),
                        onClick = {
                            env.context.copyPlainText(plugin.loadError)
                            env.toast(env.text(R.string.pluginLoadErrorCopied), FluxToastKind.Success)
                        },
                        modifier = Modifier.weight(1f),
                        variant = ButtonVariant.Primary,
                        icon = FluxIcons.Copy,
                        enabled = plugin.loadError.isNotEmpty(),
                    )
                }
            },
        ) {
            GlassSection {
                custom(padded = true) {
                    SelectionContainer {
                        FluxText(
                            plugin.loadError.ifEmpty { "—" },
                            Modifier.fillMaxWidth(),
                            style = FluxTheme.type.mono,
                            color = FluxTheme.colors.ink,
                        )
                    }
                }
            }
        }
    }
}
