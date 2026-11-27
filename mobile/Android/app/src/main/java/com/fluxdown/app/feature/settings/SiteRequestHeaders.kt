package com.fluxdown.app.feature.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.i18n.str
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.DaemonConfigListWriter
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxDialogButton
import com.fluxdown.fluxui.overlay.FluxDialogButtonStyle
import com.fluxdown.fluxui.overlay.FluxDialogSpec
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import kotlinx.coroutines.launch

internal const val SITE_REQUEST_HEADERS_KEY = "site_request_headers"
internal const val SITE_REQUEST_HEADERS_ROW = "download.siteRequestHeaders"
private const val REFERER = "Referer"

internal data class SiteHeaderRule(val origin: String, val headers: Map<String, String>) {
    val referer: String? get() = headers.entries.firstOrNull { it.key.equals(REFERER, ignoreCase = true) }?.value
    val extra: String get() = headers.entries.filterNot { it.key.equals(REFERER, ignoreCase = true) }
        .sortedBy { it.key }.joinToString("\n") { "${it.key}: ${it.value}" }

    fun toJson() = jsonObject("origin" to origin, "headers" to headers)
}

internal fun parseSiteHeaderRules(raw: String?): List<SiteHeaderRule>? {
    if (raw == null) return null
    if (raw.isEmpty()) return emptyList()
    val array = Json.parseOrNull(raw) as? JsonValue.Arr ?: return null
    return array.items.map { item ->
        val rule = item as? JsonValue.Obj ?: return null
        val origin = (rule.fields["origin"] as? JsonValue.Str)?.value ?: return null
        val headers = rule.fields["headers"] as? JsonValue.Obj ?: return null
        SiteHeaderRule(origin, headers.fields.mapValues { (_, value) -> (value as? JsonValue.Str)?.value ?: return null })
    }
}

internal fun draftRule(origin: String, referer: String, extra: String): SiteHeaderRule? {
    val headers = linkedMapOf<String, String>()
    if (referer.isNotEmpty()) headers[REFERER] = referer
    for (line in extra.split('\n')) {
        if (line.isBlank()) continue
        val colon = line.indexOf(':')
        if (colon <= 0) return null
        val name = line.substring(0, colon).trim()
        if (name.isEmpty() || name.equals(REFERER, ignoreCase = true) ||
            headers.keys.any { it.equals(name, ignoreCase = true) }
        ) return null
        headers[name] = line.substring(colon + 1).removePrefix(" ")
    }
    return SiteHeaderRule(origin, headers)
}

internal class SiteHeaderTarget(val existing: SiteHeaderRule?, val session: HostSession)

internal fun GlassSectionScope.siteRequestHeadersRows(ctx: SettingsCtx, onEdit: (SiteHeaderRule?) -> Unit) {
    row {
        SettingRowBox(ctx, SITE_REQUEST_HEADERS_ROW, SITE_REQUEST_HEADERS_KEY) {
            SiteRequestHeadersRows(ctx.form.string(SITE_REQUEST_HEADERS_KEY), ctx.readOnly, onEdit)
        }
    }
}

@Composable
private fun SiteRequestHeadersRows(raw: String, readOnly: Boolean, onEdit: (SiteHeaderRule?) -> Unit) {
    val rules = remember(raw) { parseSiteHeaderRules(raw) }
    Column {
        FluxListRow(title = str(R.string.siteRequestHeadersTitle), subtitle = str(R.string.siteRequestHeadersDesc))
        if (rules == null) {
            FluxBanner(str(R.string.siteRequestHeadersInvalid), kind = FluxBannerKind.Error, slim = true)
        } else {
            rules.forEach { rule ->
                FluxListRow(
                    title = rule.origin,
                    chevron = true,
                    enabled = !readOnly,
                    onClick = { onEdit(rule) },
                )
            }
            FluxListRow(
                title = str(R.string.siteRequestHeadersAdd),
                icon = FluxIcons.Plus,
                enabled = !readOnly,
                onClick = { onEdit(null) },
            )
        }
    }
}

@Composable
internal fun SiteHeaderSheet(target: SiteHeaderTarget, readOnly: Boolean, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val scope = rememberCoroutineScope()
    val existing = target.existing
    val writer = remember(target) {
        DaemonConfigListWriter(
            ContainerDaemonConfigPort { target.session }, SITE_REQUEST_HEADERS_KEY,
            { parseSiteHeaderRules(it) ?: throw HostException(HostErrorCode.InvalidArgument, message = context.str(R.string.siteRequestHeadersInvalid)) },
            { rules -> if (rules.isEmpty()) "" else JsonValue.Arr(rules.map { it.toJson() }).toJson() },
        )
    }
    var origin by remember(target) { mutableStateOf(existing?.origin.orEmpty()) }
    var referer by remember(target) { mutableStateOf(existing?.referer.orEmpty()) }
    var extra by remember(target) { mutableStateOf(existing?.extra.orEmpty()) }
    var error by remember(target) { mutableStateOf<String?>(null) }
    var busy by remember(target) { mutableStateOf(false) }
    val enabled = !busy && !readOnly

    fun write(replacement: SiteHeaderRule?) {
        if (!enabled || busy) return
        busy = true
        error = null
        scope.launch {
            try {
                writer.mutate { rules ->
                    if (existing == null) {
                        replacement?.let { rules + it }
                    } else {
                        val index = rules.indexOf(existing)
                        if (index < 0) throw HostException(HostErrorCode.Conflict)
                        rules.toMutableList().apply {
                            if (replacement == null) removeAt(index) else set(index, replacement)
                        }
                    }
                }
                onDismiss()
            } catch (e: HostException) {
                error = daemonWriteErrorText(context, e)
            } finally {
                busy = false
            }
        }
    }

    val title = str(if (existing == null) R.string.siteRequestHeadersAdd else R.string.siteRequestHeadersTitle)
    val delete = str(R.string.delete)
    val cancel = str(R.string.cancel)
    val invalid = str(R.string.siteRequestHeadersInvalid)
    FluxSheet(
        visible = true,
        onDismissRequest = onDismiss,
        detent = FluxSheetDetent.Full,
        dismissible = !busy,
        title = title,
        header = { FluxSheetHeader(title = title, onClose = if (busy) null else onDismiss) },
        footer = {
            FluxSheetFooter {
                if (existing != null) {
                    FluxButton(delete, onClick = {
                        overlays.showDialog(FluxDialogSpec(title = delete, message = existing.origin, buttons = listOf(
                            FluxDialogButton(cancel, FluxDialogButtonStyle.Secondary),
                            FluxDialogButton(delete, FluxDialogButtonStyle.Destructive) { write(null) },
                        )))
                    }, variant = ButtonVariant.Danger, enabled = enabled, modifier = Modifier.weight(1f))
                }
                FluxButton(str(R.string.proxyRuleSave), onClick = {
                    val draft = draftRule(origin, referer, extra)
                    if (draft == null) error = invalid else write(draft)
                }, variant = ButtonVariant.Primary, enabled = enabled, loading = busy, modifier = Modifier.weight(1f))
            }
        },
    ) {
        val urlKeyboard = KeyboardOptions(autoCorrectEnabled = false, keyboardType = KeyboardType.Uri)
        Column(Modifier.padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            FluxField(origin, { origin = it; error = null }, label = str(R.string.siteRequestHeadersOrigin),
                keyboardOptions = urlKeyboard, enabled = enabled, mono = true)
            FluxField(referer, { referer = it; error = null }, label = str(R.string.siteRequestHeadersReferer),
                keyboardOptions = urlKeyboard, enabled = enabled, mono = true)
            FluxField(extra, { extra = it; error = null }, label = str(R.string.siteRequestHeadersExtra),
                hint = str(R.string.siteRequestHeadersExtraHint), singleLine = false, rows = 4,
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false), enabled = enabled, mono = true)
            error?.let { FluxBanner(it, kind = FluxBannerKind.Error, slim = true) }
        }
    }
}
