package com.fluxdown.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.arrayOrNull
import com.fluxdown.core.protocol.boolOrNull
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.protocol.stringOrNull
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxIconButton
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxPickList
import com.fluxdown.fluxui.controls.FluxSelect
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.IconButtonSize
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme

internal const val SITE_USER_AGENT_RULES_KEY = "site_user_agent_rules"
private const val MAX_RULES = 200

private data class SiteUaRule(
    val domain: String = "",
    val includeSubdomains: Boolean = false,
    val userAgent: String = "",
) {
    fun encode() = jsonObject("domain" to domain, "includeSubdomains" to includeSubdomains, "userAgent" to userAgent)
}

// Only decode the wire shape here. The host validates domains and header values.
private fun decodeRules(wire: String): List<SiteUaRule>? {
    if (wire.isEmpty()) return emptyList()
    return Json.parseOrNull(wire).arrayOrNull?.map { value ->
        val obj = value as? JsonValue.Obj ?: return null
        if (obj.fields.size != 3) return null
        SiteUaRule(
            domain = obj["domain"].stringOrNull ?: return null,
            includeSubdomains = obj["includeSubdomains"].boolOrNull ?: return null,
            userAgent = obj["userAgent"].stringOrNull ?: return null,
        )
    }
}

private data class RuleEdit(val base: List<SiteUaRule>, val index: Int?, val draft: SiteUaRule)

@Composable
internal fun SiteUserAgentRulesSheet(
    ctx: SettingsCtx,
    visible: Boolean,
    presets: List<SelectOption<String>>,
    onDismiss: () -> Unit,
) {
    val wire = ctx.form.string(SITE_USER_AGENT_RULES_KEY)
    val rules = remember(wire) { decodeRules(wire) }
    var edit by remember { mutableStateOf<RuleEdit?>(null) }
    val current = edit
    val canEdit = rules != null && !ctx.readOnly
    val canSave = canEdit && current != null && current.base == rules
    val title = str(when {
        current == null -> R.string.siteUserAgentRules
        current.index == null -> R.string.siteUserAgentAdd
        else -> R.string.siteUserAgentEdit
    })

    fun dismiss() {
        if (current != null) edit = null else onDismiss()
    }

    fun save(next: List<SiteUaRule>) {
        ctx.editor.set(SITE_USER_AGENT_RULES_KEY, JsonValue.Arr(next.map(SiteUaRule::encode)).toJson())
        edit = null
    }

    FluxPortal {
        FluxSheet(
            visible = visible,
            onDismissRequest = ::dismiss,
            detent = FluxSheetDetent.Full,
            title = title,
            header = { FluxSheetHeader(title = title, onClose = ::dismiss) },
            footer = if (current == null) null else {
                {
                    FluxSheetFooter {
                        FluxButton(
                            text = str(R.string.cancel),
                            onClick = ::dismiss,
                            variant = ButtonVariant.Secondary,
                            modifier = Modifier.weight(1f),
                        )
                        FluxButton(
                            text = str(R.string.proxyRuleSave),
                            onClick = {
                                save(current.base.toMutableList().apply {
                                    if (current.index == null) add(current.draft) else set(current.index, current.draft)
                                })
                            },
                            enabled = canSave && current.draft.domain.isNotBlank() && current.draft.userAgent.isNotBlank(),
                            modifier = Modifier.weight(1f),
                        )
                    }
                }
            },
        ) {
            SettingFailure(ctx, SITE_USER_AGENT_RULES_KEY)
            if (current == null) {
                if (rules == null) {
                    FluxText(str(R.string.siteUserAgentInvalid), color = FluxTheme.colors.coralText)
                } else {
                    RuleList(
                        rules = rules,
                        enabled = canEdit,
                        onAdd = { edit = RuleEdit(rules, null, SiteUaRule()) },
                        onEdit = { index -> edit = RuleEdit(rules, index, rules[index]) },
                        onMove = { index, to ->
                            save(rules.toMutableList().apply { add(to, removeAt(index)) })
                        },
                    )
                }
            } else {
                if (current.base != rules) {
                    FluxText(str(R.string.localServiceConflict), color = FluxTheme.colors.coralText)
                }
                RuleFields(
                    draft = current.draft,
                    enabled = canSave,
                    presets = presets,
                    onChange = { edit = current.copy(draft = it) },
                    onDelete = current.index?.let { index ->
                        { save(current.base.toMutableList().apply { removeAt(index) }) }
                    },
                )
            }
        }
    }
}

@Composable
private fun RuleList(
    rules: List<SiteUaRule>,
    enabled: Boolean,
    onAdd: () -> Unit,
    onEdit: (Int) -> Unit,
    onMove: (Int, Int) -> Unit,
) {
    GlassSection(footer = str(R.string.siteUserAgentRulesDesc)) {
        rules.forEachIndexed { index, rule ->
            row {
                val subtitle = if (rule.includeSubdomains) {
                    "${str(R.string.siteUserAgentSubdomains)} · ${rule.userAgent}"
                } else rule.userAgent
                FluxListRow(
                    title = rule.domain,
                    subtitle = subtitle,
                    enabled = enabled,
                    onClick = { onEdit(index) },
                    trailing = if (rules.size <= 1) null else {
                        {
                            Row {
                                FluxIconButton(
                                    FluxIcons.ArrowUp, str(R.string.siteUserAgentMoveUp), { onMove(index, index - 1) },
                                    size = IconButtonSize.Sm, enabled = enabled && index > 0,
                                )
                                FluxIconButton(
                                    FluxIcons.ArrowDown, str(R.string.siteUserAgentMoveDown), { onMove(index, index + 1) },
                                    size = IconButtonSize.Sm, enabled = enabled && index < rules.lastIndex,
                                )
                            }
                        }
                    },
                )
            }
        }
        row(hasIcon = true) {
            FluxActionRow(
                title = str(R.string.siteUserAgentAdd),
                icon = FluxIcons.Plus,
                onClick = onAdd,
                enabled = enabled && rules.size < MAX_RULES,
            )
        }
    }
}

@Composable
private fun RuleFields(
    draft: SiteUaRule,
    enabled: Boolean,
    presets: List<SelectOption<String>>,
    onChange: (SiteUaRule) -> Unit,
    onDelete: (() -> Unit)?,
) {
    var showPresets by remember { mutableStateOf(false) }
    val selected = presets.firstOrNull { it.value == draft.userAgent }
    val custom = str(R.string.userAgentPresetCustom)
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        FluxField(
            value = draft.domain,
            onValueChange = { onChange(draft.copy(domain = it)) },
            label = str(R.string.siteUserAgentDomain),
            enabled = enabled,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
        )
        GlassSection {
            row {
                FluxSwitchRow(
                    title = str(R.string.siteUserAgentSubdomains),
                    checked = draft.includeSubdomains,
                    onCheckedChange = { onChange(draft.copy(includeSubdomains = it)) },
                    enabled = enabled,
                )
            }
        }
        FluxSelect(
            label = str(R.string.userAgent),
            value = selected?.label ?: custom,
            enabled = enabled,
            onClick = { showPresets = !showPresets },
        )
        if (showPresets && enabled) {
            FluxPickList(presets + SelectOption("", custom), selected?.value.orEmpty(), onSelect = { ua ->
                if (ua.isNotEmpty() || selected != null) onChange(draft.copy(userAgent = ua))
                showPresets = false
            })
        }
        FluxField(
            value = draft.userAgent,
            onValueChange = { onChange(draft.copy(userAgent = it)) },
            placeholder = str(R.string.userAgentPlaceholder),
            enabled = enabled,
            keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        )
        if (onDelete != null) {
            FluxButton(
                text = str(R.string.delete),
                onClick = onDelete,
                enabled = enabled,
                variant = ButtonVariant.Danger,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
