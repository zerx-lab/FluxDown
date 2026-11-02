package com.fluxdown.app.feature.settings.bt

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalLocale
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.feature.settings.rememberLastNonNull
import com.fluxdown.app.i18n.str
import com.fluxdown.core.format.Format
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.BtGuardAction
import com.fluxdown.core.protocol.BtGuardCidr
import com.fluxdown.core.protocol.BtGuardDuration
import com.fluxdown.core.protocol.BtGuardEntry
import com.fluxdown.core.protocol.BtGuardEvent
import com.fluxdown.core.protocol.BtGuardEvidence
import com.fluxdown.core.protocol.BtGuardEvidenceFact
import com.fluxdown.core.protocol.BtGuardEvidenceField
import com.fluxdown.core.protocol.BtGuardLayer
import com.fluxdown.core.protocol.BtGuardRemaining
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldRow
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSegmented
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.SegOption
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

/** 防吸血页的浮层。每次打开都是新实例（身份比较），表单状态随实例重置。 */
internal sealed interface GuardSheet {
    /** 手动添加封禁 / 限速 / 白名单条目；[cidr] 预填。 */
    class Add(val cidr: String = "", val action: BtGuardAction = BtGuardAction.Ban) : GuardSheet

    /** 证据上报的显式同意（确认后才写入 `bt_guard_report_enabled`）。 */
    class Consent(val onConfirm: () -> Unit) : GuardSheet

    class EntryDetail(val entry: BtGuardEntry) : GuardSheet

    class EventDetail(val event: BtGuardEvent) : GuardSheet
}

// ───────────────────────────── 标签映射 ─────────────────────────────

@StringRes
internal fun layerLabelRes(layer: BtGuardLayer?): Int = when (layer) {
    null -> R.string.btGuardLayerAll
    BtGuardLayer.Allow -> R.string.btGuardLayerAllow
    BtGuardLayer.Manual -> R.string.btGuardLayerManual
    BtGuardLayer.Auto -> R.string.btGuardLayerAuto
    BtGuardLayer.Cloud -> R.string.btGuardLayerCloud
    BtGuardLayer.File -> R.string.btGuardLayerFile
}

@StringRes
internal fun actionLabelRes(action: BtGuardAction): Int = when (action) {
    BtGuardAction.Allow -> R.string.btGuardActionAllow
    BtGuardAction.Ban -> R.string.btGuardActionBan
    BtGuardAction.Choke -> R.string.btGuardActionChoke
    BtGuardAction.Throttle -> R.string.btGuardActionThrottle
}

@StringRes
internal fun ruleLabelRes(rule: String): Int? = when (rule) {
    "excessive" -> R.string.btGuardRuleExcessive
    "rerequest" -> R.string.btGuardRuleRerequest
    "rewind" -> R.string.btGuardRuleRewind
    "lag" -> R.string.btGuardRuleLag
    "fingerprint" -> R.string.btGuardRuleFingerprint
    "cloud" -> R.string.btGuardRuleCloud
    "manual" -> R.string.btGuardRuleManual
    "file" -> R.string.btGuardRuleFile
    else -> null
}

@StringRes
internal fun eventKindLabelRes(kind: String): Int? = when (kind) {
    "observed" -> R.string.btGuardEventKindObserved
    "enforced" -> R.string.btGuardEventKindEnforced
    "probe" -> R.string.btGuardEventKindProbe
    "probe_cleared" -> R.string.btGuardEventKindProbeCleared
    "entry_added" -> R.string.btGuardEventKindEntryAdded
    "entry_removed" -> R.string.btGuardEventKindEntryRemoved
    "cloud_applied" -> R.string.btGuardEventKindCloudApplied
    "cloud_rejected" -> R.string.btGuardEventKindCloudRejected
    "filter_applied" -> R.string.btGuardEventKindFilterApplied
    else -> null
}

@StringRes
private fun evidenceLabelRes(field: BtGuardEvidenceField): Int = when (field) {
    BtGuardEvidenceField.Detail -> R.string.btGuardEvDetail
    BtGuardEvidenceField.TorrentSize -> R.string.btGuardEvTorrentSize
    BtGuardEvidenceField.DeliveredBytes -> R.string.btGuardEvDelivered
    BtGuardEvidenceField.DupBytes -> R.string.btGuardEvDup
    BtGuardEvidenceField.Ratio -> R.string.btGuardEvRatio
    BtGuardEvidenceField.Rerequests -> R.string.btGuardEvRerequests
    BtGuardEvidenceField.Rewind -> R.string.btGuardEvRewind
    BtGuardEvidenceField.Strikes -> R.string.btGuardEvStrikes
    BtGuardEvidenceField.Category -> R.string.btGuardEvCategory
    BtGuardEvidenceField.Url -> R.string.btGuardEvUrl
    BtGuardEvidenceField.Entries -> R.string.btGuardEvEntries
}

/** 未知值（新版主机新增的层 / 动作 / 规则）原样显示，不丢信息。 */
@Composable
internal fun ruleLabel(rule: String): String = ruleLabelRes(rule)?.let { str(it) } ?: rule

@Composable
internal fun actionLabel(action: String): String =
    BtGuardAction.fromWire(action)?.let { str(actionLabelRes(it)) } ?: action

@Composable
internal fun layerLabel(layer: String): String =
    BtGuardLayer.fromWire(layer)?.let { str(layerLabelRes(it)) } ?: layer

@Composable
internal fun eventKindLabel(kind: String): String = eventKindLabelRes(kind)?.let { str(it) } ?: kind

/** Unix 秒 → 本地化日期时间；0 / 负值视为未知（空串）。 */
@Composable
internal fun guardTime(unix: Long): String {
    if (unix <= 0) return ""
    val locale = LocalLocale.current.platformLocale
    return DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT, locale).format(Date(unix * 1000))
}

/** 剩余时长文案：永久 / `{time} left`，不足一分钟按 1 分钟，已过期按 0 分钟。 */
@Composable
internal fun remainingText(entry: BtGuardEntry): String {
    val now = System.currentTimeMillis() / 1000
    val time = when (val r = BtGuardRemaining.of(entry, now)) {
        BtGuardRemaining.Permanent -> return str(R.string.btGuardPermanent)
        BtGuardRemaining.Expired -> str(R.string.btGuardDurationM, "n" to 0)
        is BtGuardRemaining.Minutes -> str(R.string.btGuardDurationM, "n" to r.n)
        is BtGuardRemaining.Hours -> str(R.string.btGuardDurationH, "n" to r.n)
        is BtGuardRemaining.Days -> str(R.string.btGuardDurationD, "n" to r.n)
    }
    return str(R.string.btGuardRemaining, "time" to time)
}

@Composable
private fun evidenceValue(fact: BtGuardEvidenceFact): String = when (fact.field) {
    BtGuardEvidenceField.TorrentSize, BtGuardEvidenceField.DeliveredBytes, BtGuardEvidenceField.DupBytes ->
        Format.bytes(fact.number ?: 0).toString()
    BtGuardEvidenceField.Ratio -> BtGuardEvidence.ratioText(fact.number ?: 0) + "×"
    BtGuardEvidenceField.Rewind -> BtGuardEvidence.permilleText(fact.number ?: 0) + "%"
    else -> fact.text ?: (fact.number ?: 0).toString()
}

// ───────────────────────────── 浮层宿主 ─────────────────────────────

@Composable
internal fun GuardSheetHost(sheet: GuardSheet?, onDismiss: () -> Unit, model: BtGuardModel) {
    val shown = rememberLastNonNull(sheet)
    val visible = sheet != null
    FluxPortal {
        when (shown) {
            null -> Unit
            is GuardSheet.Add -> key(shown) { AddEntrySheet(shown, visible, onDismiss, model) }
            is GuardSheet.Consent -> ConsentSheet(shown, visible, onDismiss)
            is GuardSheet.EntryDetail -> DetailSheet(
                visible = visible,
                onDismiss = onDismiss,
                title = shown.entry.cidr,
                subtitle = null,
            ) { EntryDetailBody(shown.entry) }
            is GuardSheet.EventDetail -> DetailSheet(
                visible = visible,
                onDismiss = onDismiss,
                title = shown.event.ipGroup.ifBlank { eventKindLabel(shown.event.kind) },
                subtitle = eventKindLabel(shown.event.kind),
            ) { EventDetailBody(shown.event) }
        }
    }
}

@Composable
private fun AddEntrySheet(sheet: GuardSheet.Add, visible: Boolean, onDismiss: () -> Unit, model: BtGuardModel) {
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val scope = rememberCoroutineScope()
    var cidr by remember { mutableStateOf(sheet.cidr) }
    var action by remember { mutableStateOf(sheet.action) }
    var duration by remember { mutableStateOf(if (sheet.action == BtGuardAction.Allow) BtGuardDuration.Permanent else BtGuardDuration.Day) }
    var reason by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var touched by remember { mutableStateOf(false) }

    val parsed = BtGuardCidr.parse(cidr)
    val error = when {
        parsed is BtGuardCidr.Result.Invalid -> str(R.string.btGuardErrCidrInvalid)
        parsed is BtGuardCidr.Result.Empty && touched -> str(R.string.btGuardErrCidrEmpty)
        else -> null
    }
    val failedText = str(R.string.btGuardAddFailed)
    val addedTemplate = R.string.btGuardAdded
    val title = str(R.string.btGuardAddTitle)

    val actionOptions = listOf(BtGuardAction.Ban, BtGuardAction.Choke, BtGuardAction.Throttle, BtGuardAction.Allow)
        .map { SegOption(it, str(actionLabelRes(it))) }
    val durationOptions = listOf(
        SegOption(BtGuardDuration.Hour, str(R.string.btGuardDuration1h)),
        SegOption(BtGuardDuration.Day, str(R.string.btGuardDuration1d)),
        SegOption(BtGuardDuration.Week, str(R.string.btGuardDuration7d)),
        SegOption(BtGuardDuration.Permanent, str(R.string.btGuardDurationForever)),
    )
    val addedText = str(addedTemplate, "cidr" to ((parsed as? BtGuardCidr.Result.Ok)?.normalized ?: cidr.trim()))

    FluxSheet(
        visible = visible,
        onDismissRequest = onDismiss,
        title = title,
        header = { FluxSheetHeader(title = title, onClose = onDismiss) },
        footer = {
            FluxSheetFooter {
                FluxButton(str(R.string.cancel), onClick = onDismiss, modifier = Modifier.weight(1f))
                FluxButton(
                    str(R.string.btGuardAddConfirm),
                    onClick = {
                        touched = true
                        val ok = parsed as? BtGuardCidr.Result.Ok ?: return@FluxButton
                        busy = true
                        scope.launch {
                            try {
                                model.add(ok.normalized, action, duration, reason)
                                overlays.toast(addedText, FluxToastKind.Success)
                                onDismiss()
                            } catch (e: CancellationException) {
                                throw e
                            } catch (e: HostException) {
                                overlays.toast("$failedText: ${actions.errorText(e)}", FluxToastKind.Error)
                            } finally {
                                busy = false
                            }
                        }
                    },
                    modifier = Modifier.weight(1f),
                    variant = ButtonVariant.Primary,
                    enabled = parsed is BtGuardCidr.Result.Ok,
                    loading = busy,
                )
            }
        },
    ) {
        Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            FluxField(
                value = cidr,
                onValueChange = { cidr = it.take(MAX_CIDR_CHARS) },
                label = str(R.string.btGuardAddCidr),
                placeholder = str(R.string.btGuardAddCidrPlaceholder),
                error = error,
                mono = true,
                onFocusChange = { focused -> if (!focused) touched = true },
            )
            FluxFieldRow(title = str(R.string.btGuardAddAction)) {
                FluxSegmented(actionOptions, action, {
                    action = it
                    if (it == BtGuardAction.Allow) duration = BtGuardDuration.Permanent
                })
            }
            FluxFieldRow(title = str(R.string.btGuardAddDuration)) {
                FluxSegmented(durationOptions, duration, { duration = it })
            }
            FluxField(
                value = reason,
                onValueChange = { reason = it.take(MAX_REASON_CHARS) },
                label = str(R.string.btGuardAddReason),
            )
        }
    }
}

private const val MAX_CIDR_CHARS = 64
private const val MAX_REASON_CHARS = 200

@Composable
private fun ConsentSheet(sheet: GuardSheet.Consent, visible: Boolean, onDismiss: () -> Unit) {
    val title = str(R.string.btGuardReportConsentTitle)
    FluxSheet(
        visible = visible,
        onDismissRequest = onDismiss,
        title = title,
        header = { FluxSheetHeader(title = title, onClose = onDismiss) },
        footer = {
            FluxSheetFooter {
                FluxButton(str(R.string.cancel), onClick = onDismiss, modifier = Modifier.weight(1f))
                FluxButton(
                    str(R.string.btGuardReportConsentConfirm),
                    onClick = {
                        sheet.onConfirm()
                        onDismiss()
                    },
                    modifier = Modifier.weight(1f),
                    variant = ButtonVariant.Primary,
                )
            }
        },
    ) {
        FluxText(
            str(R.string.btGuardReportConsentBody),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 22.dp, vertical = 4.dp),
            style = FluxTheme.type.body,
            color = FluxTheme.colors.inkMuted,
        )
    }
}

@Composable
private fun DetailSheet(
    visible: Boolean,
    onDismiss: () -> Unit,
    title: String,
    subtitle: String?,
    body: @Composable () -> Unit,
) {
    FluxSheet(
        visible = visible,
        onDismissRequest = onDismiss,
        title = title,
        header = { FluxSheetHeader(title = title, subtitle = subtitle, onClose = onDismiss) },
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            body()
        }
    }
}

/** 一行「标签 / 值」；值多行可换行（原因、详情可能很长）。 */
private fun com.fluxdown.fluxui.controls.GlassSectionScope.detailRow(title: String, value: String) {
    if (value.isBlank()) return
    row { FluxListRow(title = title, subtitle = value) }
}

@Composable
private fun EntryDetailBody(entry: BtGuardEntry) {
    val rows = listOf(
        str(R.string.btGuardColLayer) to layerLabel(entry.layer),
        str(R.string.btGuardColAction) to actionLabel(entry.action),
        str(R.string.btGuardColRule) to if (entry.rule.isEmpty()) "" else ruleLabel(entry.rule),
        str(R.string.btGuardColReason) to entry.reason,
        str(R.string.btGuardColRemaining) to remainingText(entry),
        str(R.string.btGuardColTime) to guardTime(entry.createdAt),
        str(R.string.btGuardEvUrl) to entry.source,
    )
    val facts = BtGuardEvidence.facts(entry.evidence).map { str(evidenceLabelRes(it.field)) to evidenceValue(it) }
    DetailSections(rows, facts, fallback = "")
}

@Composable
private fun EventDetailBody(event: BtGuardEvent) {
    val rows = listOf(
        str(R.string.btGuardColTime) to guardTime(event.at),
        str(R.string.btGuardColRule) to if (event.rule.isEmpty()) "" else ruleLabel(event.rule),
        str(R.string.btGuardColMode) to event.mode,
        str(R.string.btGuardColAction) to if (event.action.isEmpty()) "" else actionLabel(event.action),
        str(R.string.btGuardColAddress) to event.ipGroup,
        str(R.string.btGuardEvInfoHash) to event.infoHash,
    )
    val facts = BtGuardEvidence.factsFromText(event.detail).map { str(evidenceLabelRes(it.field)) to evidenceValue(it) }
    // detail 不是证据 JSON 时原文显示（可能是诊断文本）
    DetailSections(rows, facts, fallback = if (facts.isEmpty()) event.detail else "")
}

/** 上：基础字段；下：证据摘要（无证据且无原文时显示「暂无证据」）。空值行跳过。 */
@Composable
private fun DetailSections(rows: List<Pair<String, String>>, facts: List<Pair<String, String>>, fallback: String) {
    val noneText = str(R.string.btGuardEvidenceNone)
    val detailTitle = str(R.string.btGuardColDetail)
    GlassSection {
        rows.forEach { (label, value) -> detailRow(label, value) }
    }
    GlassSection(title = str(R.string.btGuardEvidenceSummary)) {
        when {
            facts.isNotEmpty() -> facts.forEach { (label, value) -> detailRow(label, value) }
            fallback.isNotBlank() -> detailRow(detailTitle, fallback)
            else -> custom(padded = true) {
                FluxText(noneText, style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
            }
        }
    }
}
