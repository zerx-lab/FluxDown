package com.fluxdown.app.feature.settings.general

import android.Manifest
import android.content.Context
import android.content.Intent
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import com.fluxdown.app.FeatureFlags
import com.fluxdown.app.R
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.feature.settings.SettingRowBox
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.feature.settings.SettingsEntry
import com.fluxdown.app.feature.settings.SettingsPageFrame
import com.fluxdown.app.feature.settings.SettingsSearchContext
import com.fluxdown.app.feature.settings.flowItem
import com.fluxdown.app.feature.settings.rememberSettingsCtx
import com.fluxdown.app.feature.settings.settingSwitch
import com.fluxdown.app.feature.settings.startSettingsIntent
import com.fluxdown.app.i18n.str
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.app.nav.Route
import com.fluxdown.app.nav.SettingsPage
import com.fluxdown.app.service.DownloadNotifier
import com.fluxdown.app.service.DownloadServiceController
import com.fluxdown.app.service.NotifyAuthorization
import com.fluxdown.core.protocol.SettingsForm
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSectionFoot
import com.fluxdown.fluxui.controls.FluxSwitchRow
import com.fluxdown.fluxui.controls.GlassSection
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.controls.Tone
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.material.rememberFlowInGate
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays

private const val COMPLETE_KEY = "download.notify_on_complete"
private const val SILENT_KEY = "download.silent_download"
private const val SILENT_AUTO_CATEGORY_KEY = "download.silent_auto_category"
private const val SILENT_SKIP_KEY = "download.silent_skip_selection"

/** 通知页的行 id（同 iOS `NotifyPage` 的常量）与所在 `LazyColumn` 项 key。 */
private object NotifyIds {
    const val PERMISSION = "notify.permission"
    const val COMPLETE = "notify.onComplete"
    const val ACTIONS = "notify.actions"
    const val FAILURE = "notify.onFailure"
    const val SELECTION = "notify.selection"
    const val WEBHOOK = "notify.webhook"
    const val SILENT = "download.silentDownload"
    const val SILENT_AUTO_CATEGORY = "download.silentAutoCategory"
    const val SILENT_SKIP = "download.silentSkipSelection"

    const val ITEM_PERMISSION = "permission"
    const val ITEM_SYSTEM = "system"
    const val ITEM_SILENT = "silent"
    const val ITEM_WEBHOOK = "webhook"
}

/**
 * S10 · 通知：系统授权卡、完成 / 失败 / 选择请求通知、静默下载、Webhook 入口。
 *
 * 通知由客户端按主机状态本地发出（[DownloadNotifier]）；授权**懒申请**——进入本页看到说明后由用户点「允许通知」，
 * 或首次打开某个通知开关时申请（Android 13+ 的 `POST_NOTIFICATIONS`）；拒绝后引导到系统通知设置。
 * 完成通知开关 `download.notify_on_complete` 是云同步偏好；其余（失败 / 操作按钮）是设备本地设置。
 */
@Composable
internal fun NotifyPage() {
    val nav = LocalNavigator.current
    val context = LocalContext.current
    val overlays = LocalFluxOverlays.current
    val ctx = rememberSettingsCtx(openPicker = {})
    val gate = rememberFlowInGate()
    val device = DeviceSettings.of(context)
    val authorization = DownloadNotifier.authorization
    val completeOn = ctx.form.bool(COMPLETE_KEY)
    val noApp = str(R.string.mobileNoSettingsApp)

    val requestPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        // 无论同意与否都记为已询问：之后不再弹框，拒绝则引导到系统通知设置
        DownloadServiceController.markNotificationPermissionAsked(context)
        DownloadNotifier.refreshAuthorization(context)
    }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { DownloadNotifier.refreshAuthorization(context) }

    fun requestIfUndetermined() {
        if (authorization == NotifyAuthorization.NotDetermined && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    SettingsPageFrame(title = str(R.string.settingsCatNotify), onBack = { nav.pop() }) {
        var index = 0
        if (ctx.readOnly) {
            flowItem(index++, gate, "banner") {
                FluxBanner(str(R.string.localServiceDisconnected), kind = FluxBannerKind.Warn, slim = true)
            }
        }
        flowItem(index++, gate, NotifyIds.ITEM_PERMISSION) {
            GlassSection {
                row(hasIcon = true) {
                    SettingRowBox(ctx, NotifyIds.PERMISSION, null) { PermissionCard(authorization) }
                }
                row(hasIcon = true) {
                    when (authorization) {
                        NotifyAuthorization.NotDetermined -> FluxActionRow(
                            title = str(R.string.mobileNotifAllow),
                            onClick = { requestIfUndetermined() },
                            icon = FluxIcons.Bell,
                        )
                        NotifyAuthorization.Denied -> FluxActionRow(
                            title = str(R.string.doctorActionOpenSettings),
                            onClick = { startSettingsIntent(context, overlays, notificationSettingsIntent(context), noApp) },
                            icon = FluxIcons.Settings,
                        )
                        NotifyAuthorization.Authorized -> {
                            val sentText = str(R.string.doctorTestNotificationSent)
                            val failedText = str(R.string.mobileNotifTestFailed)
                            FluxActionRow(
                                title = str(R.string.doctorActionTestNotification),
                                onClick = {
                                    val sent = DownloadNotifier.sendTest(context)
                                    overlays.toast(if (sent) sentText else failedText, if (sent) FluxToastKind.Success else FluxToastKind.Error)
                                },
                                icon = FluxIcons.Send,
                            )
                        }
                    }
                }
            }
        }
        flowItem(index++, gate, NotifyIds.ITEM_SYSTEM) {
            GlassSection(title = str(R.string.notifyGroupSystem), footer = str(R.string.mobileNotifSystemFooterAndroid)) {
                settingSwitch(
                    ctx, COMPLETE_KEY, R.string.notifyOnComplete, R.string.notifyOnCompleteDesc, id = NotifyIds.COMPLETE,
                    onChange = { on ->
                        ctx.editor.set(COMPLETE_KEY, SettingsForm.wire(on))
                        if (on) requestIfUndetermined()
                    },
                )
                deviceSwitch(
                    ctx, NotifyIds.ACTIONS, R.string.mobileNotifActions, R.string.mobileNotifActionsDesc,
                    checked = device.notifyActions, enabled = completeOn,
                ) { device.updateNotifyActions(it) }
                deviceSwitch(
                    ctx, NotifyIds.FAILURE, R.string.mobileNotifOnFailure, R.string.mobileNotifOnFailureDesc,
                    checked = device.notifyOnFailure, enabled = true,
                ) {
                    device.updateNotifyOnFailure(it)
                    if (it) requestIfUndetermined()
                }
                row {
                    SettingRowBox(ctx, NotifyIds.SELECTION, null) {
                        FluxListRow(
                            title = str(R.string.mobileNotifSelection),
                            subtitle = str(R.string.mobileNotifSelectionDescAndroid),
                        )
                    }
                }
            }
        }
        flowItem(index++, gate, NotifyIds.ITEM_SILENT) {
            GlassSection(title = str(R.string.silentDownload)) {
                settingSwitch(ctx, SILENT_KEY, R.string.silentDownload, R.string.silentDownloadDesc, id = NotifyIds.SILENT)
                if (ctx.form.bool(SILENT_KEY)) {
                    settingSwitch(ctx, SILENT_AUTO_CATEGORY_KEY, R.string.silentAutoCategory, R.string.silentAutoCategoryDesc, id = NotifyIds.SILENT_AUTO_CATEGORY)
                    settingSwitch(ctx, SILENT_SKIP_KEY, R.string.silentSkipSelection, R.string.silentSkipSelectionDesc, id = NotifyIds.SILENT_SKIP)
                }
            }
        }
        if (FeatureFlags.WEBHOOKS) {
            flowItem(index++, gate, NotifyIds.ITEM_WEBHOOK) {
                GlassSection(title = str(R.string.notifyGroupWebhook)) {
                    row(hasIcon = true) {
                        SettingRowBox(ctx, NotifyIds.WEBHOOK, null) {
                            FluxListRow(
                                title = str(R.string.webhookNavTitle),
                                subtitle = str(R.string.mobileNotifWebhookDesc),
                                icon = FluxIcons.Webhook,
                                chevron = true,
                                onClick = { nav.push(Route.Settings(SettingsPage.Webhook)) },
                            )
                        }
                    }
                }
            }
        }
        flowItem(index++, gate, "legend") {
            FluxSectionFoot(str(R.string.settingsSyncLegend))
        }
    }
}

/** 设备本地开关行（不上云、不受主机只读影响）。 */
private fun GlassSectionScope.deviceSwitch(
    ctx: SettingsCtx,
    id: String,
    titleRes: Int,
    descRes: Int,
    checked: Boolean,
    enabled: Boolean,
    onChange: (Boolean) -> Unit,
) {
    row {
        SettingRowBox(ctx, id, null) {
            FluxSwitchRow(
                title = str(titleRes),
                subtitle = str(descRes),
                checked = checked,
                onCheckedChange = onChange,
                enabled = enabled,
            )
        }
    }
}

/** 系统授权卡：当前状态 + 说明。 */
@Composable
private fun PermissionCard(authorization: NotifyAuthorization) {
    val (icon, tone, title, detail) = when (authorization) {
        NotifyAuthorization.Authorized -> PermissionLook(FluxIcons.BellRing, Tone.Mint, R.string.mobileNotifPermOnTitle, R.string.mobileNotifPermOnDetail)
        NotifyAuthorization.Denied -> PermissionLook(FluxIcons.BellOff, Tone.Amber, R.string.mobileNotifPermDeniedTitle, R.string.mobileNotifPermDeniedDetail)
        NotifyAuthorization.NotDetermined -> PermissionLook(FluxIcons.Bell, Tone.Neutral, R.string.mobileNotifPermAskTitle, R.string.mobileNotifPermAskDetail)
    }
    FluxListRow(title = str(title), subtitle = str(detail), icon = icon, iconTone = tone, modifier = Modifier)
}

private data class PermissionLook(
    val icon: androidx.compose.ui.graphics.vector.ImageVector,
    val tone: Tone,
    val title: Int,
    val detail: Int,
)

/** 本应用的系统通知设置页。 */
private fun notificationSettingsIntent(context: Context): Intent =
    Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)

// ───────────────────────────── 搜索 / 读数 ─────────────────────────────

/** 设置搜索索引：本页所有可见行（与页面渲染共用可见性判定）。 */
internal fun notifySearchEntries(ctx: SettingsSearchContext): List<SettingsEntry> {
    val page = SettingsPage.Notify
    val icon = FluxIcons.BellRing
    val name = ctx.crumb(R.string.settingsCatNotify)
    val system = ctx.crumb(R.string.settingsCatNotify, R.string.notifyGroupSystem)
    val silent = ctx.crumb(R.string.settingsCatNotify, R.string.silentDownload)
    return buildList {
        add(ctx.entry(NotifyIds.PERMISSION, page, NotifyIds.ITEM_PERMISSION, R.string.mobileNotifPermTitle, R.string.mobileNotifPermAskDetail, name, icon))
        add(ctx.entry(NotifyIds.COMPLETE, page, NotifyIds.ITEM_SYSTEM, R.string.notifyOnComplete, R.string.notifyOnCompleteDesc, system, icon))
        add(ctx.entry(NotifyIds.ACTIONS, page, NotifyIds.ITEM_SYSTEM, R.string.mobileNotifActions, R.string.mobileNotifActionsDesc, system, icon))
        add(ctx.entry(NotifyIds.FAILURE, page, NotifyIds.ITEM_SYSTEM, R.string.mobileNotifOnFailure, R.string.mobileNotifOnFailureDesc, system, icon))
        add(ctx.entry(NotifyIds.SELECTION, page, NotifyIds.ITEM_SYSTEM, R.string.mobileNotifSelection, R.string.mobileNotifSelectionDescAndroid, system, icon))
        if (ctx.form.has(SILENT_KEY)) {
            add(ctx.entry(NotifyIds.SILENT, page, NotifyIds.ITEM_SILENT, R.string.silentDownload, R.string.silentDownloadDesc, silent, icon))
            if (ctx.form.bool(SILENT_KEY) && ctx.form.has(SILENT_SKIP_KEY)) {
                add(ctx.entry(NotifyIds.SILENT_SKIP, page, NotifyIds.ITEM_SILENT, R.string.silentSkipSelection, R.string.silentSkipSelectionDesc, silent, icon))
            }
        }
        if (FeatureFlags.WEBHOOKS) {
            add(
                ctx.entry(
                    NotifyIds.WEBHOOK, page, NotifyIds.ITEM_WEBHOOK, R.string.webhookNavTitle, R.string.mobileNotifWebhookDesc,
                    ctx.crumb(R.string.settingsCatNotify, R.string.notifyGroupWebhook), FluxIcons.Webhook,
                ),
            )
        }
    }
}

/** 设置首页读数：授权被拒 / 待授权优先，其次按完成 / 失败通知开关给出「开 / 仅失败 / 关」。 */
internal fun notifyReadout(ctx: SettingsSearchContext): String? {
    val notifyOnFailure = DeviceSettings.peek()?.notifyOnFailure ?: true
    return notifyReadoutText(ctx, DownloadNotifier.authorization, ctx.form.bool(COMPLETE_KEY), notifyOnFailure)
}

private fun notifyReadoutText(
    ctx: SettingsSearchContext,
    authorization: NotifyAuthorization,
    notifyOnComplete: Boolean,
    notifyOnFailure: Boolean,
): String = when {
    authorization == NotifyAuthorization.Denied -> ctx.str(R.string.mobileNotifReadoutBlocked)
    !notifyOnComplete && !notifyOnFailure -> ctx.str(R.string.mobileNotifReadoutOff)
    authorization == NotifyAuthorization.NotDetermined -> ctx.str(R.string.mobileNotifReadoutNeedsPermission)
    notifyOnComplete -> ctx.str(R.string.mobileNotifReadoutOn)
    else -> ctx.str(R.string.mobileNotifReadoutFailuresOnly)
}
