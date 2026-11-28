package com.fluxdown.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.hostState
import com.fluxdown.core.capture.CaptureDeviceRules
import com.fluxdown.core.capture.CaptureDeviceRules.Rule
import com.fluxdown.core.protocol.DeviceRules
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxPickList
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme

internal const val CAPTURE_DEVICE_RULES_ROW = "download.captureDeviceRules"
private const val RULES_KEY = CaptureDeviceRules.PREFERENCE_KEY

internal fun GlassSectionScope.captureDeviceRulesRow(ctx: SettingsCtx, onEdit: () -> Unit) {
    row {
        SettingRowBox(ctx, CAPTURE_DEVICE_RULES_ROW, RULES_KEY) {
            FluxListRow(
                title = str(R.string.captureDeviceRules),
                subtitle = str(R.string.captureDeviceRulesDesc),
                chevron = true,
                onClick = onEdit,
            )
        }
    }
}

private data class RuleEdit(val index: Int? = null, val rule: Rule? = null)

@Composable
internal fun CaptureDeviceRulesSheet(ctx: SettingsCtx, visible: Boolean, onDismiss: () -> Unit) {
    val host = hostState()
    val cloudDevices by remember { derivedStateOf { host.value.cloudDevices } }
    val linkDevices by remember { derivedStateOf { host.value.linkDevices } }
    val connection by remember { derivedStateOf { host.value.connection } }
    val cloudConnection by remember { derivedStateOf { host.value.sections[HostSection.agentCloudConnection] } }
    val targets = remember(cloudDevices, linkDevices, connection, cloudConnection) {
        CaptureDeviceRules.targets(host.value)
    }
    val wire = ctx.form.string(RULES_KEY)
    val rules = remember(wire) { CaptureDeviceRules.parse(wire) }
    val overlays = LocalFluxOverlays.current
    val conflict = str(R.string.localServiceConflict)
    var edit by remember { mutableStateOf<RuleEdit?>(null) }
    LaunchedEffect(visible) { if (visible) edit = null }

    fun save(draft: RuleEdit, value: Rule?) {
        if (ctx.readOnly) return
        val index = draft.index
        if (index != null && rules.getOrNull(index) != draft.rule) {
            overlays.toast(conflict, FluxToastKind.Error)
        } else {
            val updated = rules.toMutableList()
            when {
                index == null && value != null -> updated += value
                index != null && value != null -> updated[index] = value
                index != null -> updated.removeAt(index)
            }
            ctx.editor.set(RULES_KEY, CaptureDeviceRules.encode(updated), immediate = true)
        }
        edit = null
    }

    val draft = edit
    if (draft == null) {
        val title = str(R.string.captureDeviceRules)
        FluxSheet(
            visible = visible,
            onDismissRequest = onDismiss,
            detent = FluxSheetDetent.Full,
            title = title,
            header = { FluxSheetHeader(title = title, subtitle = str(R.string.captureDeviceRulesDesc), onClose = onDismiss) },
        ) {
            GlassSection(footer = if (targets.isEmpty()) str(R.string.captureDeviceRuleNoDevices) else null) {
                rules.forEachIndexed { index, rule ->
                    row {
                        val target = DeviceRules.resolveTarget(rule.targetId, targets)
                        FluxListRow(
                            title = ".${rule.extension}",
                            subtitle = target?.name ?: str(R.string.captureRoutingTargetUnavailable),
                            chevron = true,
                            enabled = !ctx.readOnly,
                            onClick = { edit = RuleEdit(index, rule) },
                        )
                    }
                }
                row(hasIcon = true) {
                    FluxActionRow(
                        title = str(R.string.captureDeviceRuleAdd),
                        icon = FluxIcons.Plus,
                        enabled = !ctx.readOnly && targets.isNotEmpty(),
                        onClick = { edit = RuleEdit() },
                    )
                }
            }
            SettingFailure(ctx, RULES_KEY)
        }
    } else {
        key(draft) {
            CaptureDeviceRuleEditor(
                rule = draft.rule,
                targets = targets,
                readOnly = ctx.readOnly,
                visible = visible,
                onBack = { edit = null },
                onSave = { save(draft, it) },
                onDelete = draft.rule?.let { { save(draft, null) } },
            )
        }
    }
}

@Composable
private fun CaptureDeviceRuleEditor(
    rule: Rule?,
    targets: List<DispatchTarget>,
    readOnly: Boolean,
    visible: Boolean,
    onBack: () -> Unit,
    onSave: (Rule) -> Unit,
    onDelete: (() -> Unit)?,
) {
    var extension by remember { mutableStateOf(rule?.extension.orEmpty()) }
    var targetId by remember { mutableStateOf(rule?.targetId) }
    val normalized = CaptureDeviceRules.normalizeExtension(extension)
    val selected = targetId?.let { DeviceRules.resolveTarget(it, targets) }
    val value = targetId
    val title = str(if (rule == null) R.string.captureDeviceRuleAdd else R.string.captureDeviceRuleEdit)
    FluxSheet(
        visible = visible,
        onDismissRequest = onBack,
        detent = FluxSheetDetent.Full,
        title = title,
        header = { FluxSheetHeader(title = title, onClose = onBack) },
        footer = {
            FluxSheetFooter {
                if (onDelete != null) {
                    FluxButton(str(R.string.delete), onDelete, modifier = Modifier.weight(1f), enabled = !readOnly)
                }
                FluxButton(
                    str(R.string.confirm),
                    onClick = { if (normalized != null && value != null) onSave(Rule(normalized, value)) },
                    modifier = Modifier.weight(1f),
                    enabled = !readOnly && normalized != null && (selected != null || (value != null && value == rule?.targetId)),
                    variant = ButtonVariant.Primary,
                )
            }
        },
    ) {
        Column(Modifier.padding(top = 2.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            FluxField(
                value = extension,
                onValueChange = { extension = it },
                label = str(R.string.captureDeviceRuleExtension),
                placeholder = ".apk",
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                error = if (extension.isNotEmpty() && normalized == null) str(R.string.captureDeviceRuleExtensionInvalid) else null,
                enabled = !readOnly,
            )
            val footer = when {
                value != null && selected == null -> str(R.string.captureRoutingTargetUnavailable)
                targets.isEmpty() -> str(R.string.captureDeviceRuleNoDevices)
                else -> null
            }
            if (targets.isNotEmpty()) {
                FluxPickList(
                    targets.map { target ->
                        SelectOption(
                            target.id, target.name,
                            hint = when (target.online) {
                                true -> str(R.string.deviceOnline)
                                false -> str(R.string.deviceOffline)
                                null -> str(R.string.devicePresenceUnknown)
                            },
                            icon = if (target.isCloud) FluxIcons.Cloud else FluxIcons.Wifi,
                            enabled = !readOnly,
                        )
                    },
                    selected = selected?.id,
                    onSelect = { targetId = it },
                    title = str(R.string.captureDeviceRuleTarget),
                    footer = footer,
                )
            } else if (footer != null) {
                FluxText(footer, style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
            }
        }
    }
}
