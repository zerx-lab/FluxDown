package com.fluxdown.app.feature.settings.extensions

import android.os.Build
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.R
import com.fluxdown.app.feature.settings.appVersionName
import com.fluxdown.app.feature.settings.openLink
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.callJson
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxKeyValue
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxPortal
import com.fluxdown.fluxui.overlay.FluxSheet
import com.fluxdown.fluxui.overlay.FluxSheetDetent
import com.fluxdown.fluxui.overlay.FluxSheetFooter
import com.fluxdown.fluxui.overlay.FluxSheetHeader
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxText
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

private data class ReviewedAudit(val counts: PluginFeedbackAudit?)

@Composable
internal fun PluginFeedbackSheet(sheet: PluginSheet.Feedback, visible: Boolean, plugin: PluginDto, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val container = LocalAppContainer.current
    val host by container.host.collectAsStateWithLifecycle()
    val isLocal = host is HostRef.Local
    val session = container.session
    val scope = rememberCoroutineScope()
    val focusManager = LocalFocusManager.current
    val target = remember(plugin) { PluginFeedbackTarget.forPlugin(plugin) }
    val appVersion = remember(context) { context.appVersionName() }
    var title by remember(sheet) { mutableStateOf("") }
    var description by remember(sheet) { mutableStateOf("") }
    var includeDiagnostics by remember(sheet) { mutableStateOf(false) }
    var lookup by remember(sheet, target) { mutableStateOf<PluginFeedbackResult<String>?>(null) }
    var lookupAttempt by remember(sheet, target) { mutableIntStateOf(0) }
    var reviewedAudit by remember(sheet, target, session, includeDiagnostics) { mutableStateOf<ReviewedAudit?>(null) }
    var sending by remember(sheet) { mutableStateOf(false) }
    var submission by remember(sheet) { mutableStateOf<PluginFeedbackResult<Long>?>(null) }
    val issue = (submission as? PluginFeedbackResult.Ok)?.value
    val repository = (lookup as? PluginFeedbackResult.Ok)?.value

    LaunchedEffect(sheet, visible, target, lookupAttempt) {
        if (visible && target != null) lookup = PluginFeedbackApi.destination(target)
    }
    LaunchedEffect(sheet, visible, includeDiagnostics, target, session) {
        if (!visible || !includeDiagnostics || target == null) return@LaunchedEffect
        reviewedAudit = null
        val audit = withTimeoutOrNull(5_000) {
            try {
                val response = session.callJson(
                    HostMethod.daemonPluginAuditList,
                    jsonObject("identity" to target.identity, "limit" to FEEDBACK_AUDIT_LIMIT),
                )
                PluginFeedbackAudit.fromJson(response, target.identity)
            } catch (_: HostException) {
                null
            }
        }
        reviewedAudit = ReviewedAudit(audit)
    }

    val environment = if (includeDiagnostics && reviewedAudit != null) {
        val loadStatus = when (plugin.loadStatus) {
            "Loaded" -> "loaded"
            "Failed" -> "failed"
            else -> "unknown"
        }
        val status = "Plugin load: $loadStatus"
        val platform = if (isLocal) "Android API ${Build.VERSION.SDK_INT}; $status" else status
        val counts = reviewedAudit?.counts
        if (counts == null) platform else "$platform; Recent audit: ${counts.errors} errors, ${counts.denied} denied"
    } else null

    val canSend = target != null && repository != null && !sending && issue == null &&
        (!includeDiagnostics || environment != null) && validFeedbackDraft(title, description)

    fun submit() {
        val confirmedRepository = (lookup as? PluginFeedbackResult.Ok)?.value ?: return
        if (sending || submission is PluginFeedbackResult.Ok || target == null ||
            (includeDiagnostics && (reviewedAudit == null || environment == null)) || !validFeedbackDraft(title, description)) return
        focusManager.clearFocus()
        val report = PluginFeedbackReport(
            target, confirmedRepository, title.trim(), description.trim(), appVersion,
            environment ?: if (isLocal) "Android" else null,
        )
        sending = true
        submission = null
        scope.launch {
            try {
                val result = PluginFeedbackApi.submit(report)
                submission = result
                when (result) {
                    PluginFeedbackResult.Failure.RouteChanged -> {
                        lookup = null
                        lookupAttempt++
                    }
                    PluginFeedbackResult.Failure.Unavailable -> lookup = PluginFeedbackResult.Failure.Unavailable
                    else -> Unit
                }
            } finally {
                sending = false
            }
        }
    }

    val sheetTitle = str(R.string.plugFeedbackTitle)
    val noBrowser = str(R.string.mobileNoBrowser)
    val lookupFailure = lookup as? PluginFeedbackResult.Failure
    val submissionFailure = submission as? PluginFeedbackResult.Failure
    val canFallback = target == null || lookupFailure != null ||
        (submissionFailure != null && submissionFailure != PluginFeedbackResult.Failure.RouteChanged)
    val dismiss = { if (!sending) onDismiss() }
    FluxPortal {
        FluxSheet(
            visible = visible,
            onDismissRequest = dismiss,
            dismissible = !sending,
            detent = FluxSheetDetent.Full,
            title = sheetTitle,
            header = { FluxSheetHeader(title = sheetTitle, onClose = if (sending) null else dismiss) },
            footer = {
                FluxSheetFooter {
                    FluxButton(
                        str(if (issue == null && repository != null) R.string.cancel else R.string.close),
                        onClick = dismiss,
                        enabled = !sending,
                        modifier = Modifier.weight(1f),
                    )
                    if (repository != null && issue == null) {
                        FluxButton(
                            str(R.string.plugFeedbackSend), onClick = ::submit, enabled = canSend || sending,
                            loading = sending, variant = ButtonVariant.Primary, modifier = Modifier.weight(1f),
                        )
                    }
                }
            },
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                if (issue != null) {
                    FluxBanner(str(R.string.plugFeedbackSent, "number" to issue), kind = FluxBannerKind.Success)
                } else {
                    if (target != null) {
                        FluxText(str(R.string.plugFeedbackLookupNotice), style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
                    }
                    val failure = submissionFailure ?: lookupFailure
                    if (failure != null) FeedbackFailureBanner(failure)
                    if (lookupFailure != null && lookupFailure != failure) FeedbackFailureBanner(lookupFailure)
                    if (target == null) FeedbackFailureBanner(PluginFeedbackResult.Failure.Unavailable)
                    if (target != null && lookup == null) {
                        FluxText(str(R.string.pluginCommonLoading), style = FluxTheme.type.sm, color = FluxTheme.colors.inkMuted)
                    }
                    if (lookupFailure != null && lookupFailure != PluginFeedbackResult.Failure.Unavailable) {
                        FluxButton(str(R.string.mobileRetry), onClick = { lookup = null; lookupAttempt++ })
                    }
                    if (canFallback) {
                        FluxButton(str(R.string.plugFeedbackFallback), variant = ButtonVariant.Ghost, enabled = !sending,
                            onClick = { openLink(context, overlays, GENERAL_FEEDBACK_URL, noBrowser) })
                    }
                    if (target != null && repository != null) {
                        FeedbackDestination(target, repository, appVersion, isLocal)
                        FeedbackFields(title, { title = it }, description, { description = it }, enabled = !sending)
                        FeedbackDiagnostics(
                            includeDiagnostics, { includeDiagnostics = it }, environment,
                            auditUnavailable = reviewedAudit != null && reviewedAudit?.counts == null,
                            enabled = !sending,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun FeedbackFields(
    title: String,
    onTitleChange: (String) -> Unit,
    description: String,
    onDescriptionChange: (String) -> Unit,
    enabled: Boolean,
) {
    FluxField(
        title, onTitleChange, Modifier.fillMaxWidth(),
        label = str(R.string.feedbackTitleLabel), placeholder = str(R.string.feedbackTitlePlaceholder),
        count = str(R.string.feedbackTitleCount, "n" to title.length), enabled = enabled,
    )
    FluxField(
        description, onDescriptionChange, Modifier.fillMaxWidth(),
        label = str(R.string.feedbackDescLabel), placeholder = str(R.string.plugFeedbackDescription),
        count = str(R.string.feedbackDescCount, "n" to description.length),
        singleLine = false, rows = 4, enabled = enabled,
    )
}

@Composable
private fun FeedbackDiagnostics(
    included: Boolean,
    onIncludedChange: (Boolean) -> Unit,
    environment: String?,
    auditUnavailable: Boolean,
    enabled: Boolean,
) {
    GlassSection {
        row {
            FluxSwitchRow(str(R.string.plugFeedbackDiagnostics), checked = included,
                onCheckedChange = onIncludedChange, enabled = enabled)
        }
        if (included) {
            row {
                FluxKeyValue(str(R.string.feedbackSysInfoLabel), environment ?: str(R.string.pluginCommonLoading))
            }
        }
    }
    if (included && auditUnavailable) {
        FluxBanner(str(R.string.plugFeedbackAuditUnavailable), slim = true)
    }
}

private fun validFeedbackDraft(title: String, description: String): Boolean =
    title.length <= FEEDBACK_TITLE_LIMIT && title.isNotBlank() &&
        description.length <= FEEDBACK_DESCRIPTION_LIMIT && description.isNotBlank()

@Composable
private fun FeedbackDestination(target: PluginFeedbackTarget, repository: String, appVersion: String, isLocal: Boolean) {
    FluxBanner(str(R.string.plugFeedbackPublic), kind = FluxBannerKind.Warn)
    GlassSection {
        row { FluxKeyValue(str(R.string.plugFeedbackDestination), repository, mono = true) }
        row { FluxKeyValue(str(R.string.pluginDetailIdentity), target.identity, mono = true) }
        row { FluxKeyValue(str(R.string.plugFeedbackPluginVersion), target.version, mono = true) }
        row { FluxKeyValue(str(R.string.feedbackVersionLabel), appVersion, mono = true) }
        if (isLocal) row { FluxKeyValue(str(R.string.feedbackSysInfoSystem), "Android") }
        row { FluxKeyValue(str(R.string.feedbackTypeLabel), str(R.string.feedbackTypeBug)) }
    }
}

@Composable
private fun FeedbackFailureBanner(failure: PluginFeedbackResult.Failure) {
    val text = when (failure) {
        PluginFeedbackResult.Failure.Unavailable -> R.string.plugFeedbackUnavailable
        PluginFeedbackResult.Failure.RouteChanged -> R.string.plugFeedbackRouteChanged
        PluginFeedbackResult.Failure.RateLimited -> R.string.feedbackRateLimited
        PluginFeedbackResult.Failure.Failed -> R.string.plugFeedbackFailed
        PluginFeedbackResult.Failure.Unconfirmed -> R.string.plugFeedbackUnconfirmed
    }
    FluxBanner(str(text), kind = FluxBannerKind.Warn)
}
