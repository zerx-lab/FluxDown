package com.fluxdown.app.feature.settings.bt

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalLocale
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.feature.settings.CommitTextField
import com.fluxdown.app.feature.settings.PickerSpec
import com.fluxdown.app.feature.settings.SettingFailure
import com.fluxdown.app.feature.settings.SettingRowBox
import com.fluxdown.app.feature.settings.SettingsCtx
import com.fluxdown.app.i18n.fill
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.core.host.HostException
import com.fluxdown.core.protocol.BtDuration
import com.fluxdown.core.protocol.BtDurationUnit
import com.fluxdown.core.protocol.BtRatio
import com.fluxdown.core.protocol.Ed2kIdCheckFormat
import com.fluxdown.core.protocol.Ed2kIdCheckTexts
import com.fluxdown.core.protocol.Ed2kIdKind
import com.fluxdown.core.protocol.Ed2kNetworkDto
import com.fluxdown.core.protocol.Ed2kNodesSubRefreshResponse
import com.fluxdown.core.protocol.Ed2kServerSubRefreshResponse
import com.fluxdown.core.protocol.Ed2kUpnpState
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.SubscriptionKind
import com.fluxdown.core.protocol.SubscriptionListFormat
import com.fluxdown.core.protocol.SubscriptionRefreshOutcome
import com.fluxdown.core.protocol.SubscriptionStatusModel
import com.fluxdown.core.protocol.SubscriptionTime
import com.fluxdown.core.protocol.TrackerSubRefreshResponse
import com.fluxdown.core.protocol.callJson
import com.fluxdown.fluxui.controls.FluxActionRow
import com.fluxdown.fluxui.controls.FluxField
import com.fluxdown.fluxui.controls.FluxFieldRow
import com.fluxdown.fluxui.controls.FluxIconButton
import com.fluxdown.fluxui.controls.FluxListRow
import com.fluxdown.fluxui.controls.FluxSelect
import com.fluxdown.fluxui.controls.GlassSectionScope
import com.fluxdown.fluxui.controls.IconButtonSize
import com.fluxdown.fluxui.controls.SelectOption
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import com.fluxdown.fluxui.theme.FluxTheme
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

/**
 * 多行列表行（Tracker / 订阅地址 / 服务器）：编辑区每行一个条目，失焦 / 离开页面时按 [format] 规整后写回。
 * [enabled] = false 时置灰（订阅关闭时的订阅地址）。
 */
internal fun GlassSectionScope.linesRow(
    ctx: SettingsCtx,
    rowId: String,
    cfgKey: String,
    @StringRes title: Int,
    @StringRes desc: Int?,
    @StringRes placeholder: Int,
    format: SubscriptionListFormat = SubscriptionListFormat.Lines,
    enabled: Boolean = true,
) {
    if (!ctx.form.has(cfgKey)) return
    row {
        SettingRowBox(ctx, rowId, cfgKey) {
            FluxFieldRow(title = str(title), subtitle = desc?.let { str(it) }, cloud = ctx.synced(cfgKey)) {
                CommitTextField(
                    value = format.toEditor(ctx.form.string(cfgKey)),
                    onCommit = { text ->
                        val stored = format.toStored(text)
                        if (stored != ctx.form.string(cfgKey)) ctx.editor.set(cfgKey, stored)
                    },
                    enabled = enabled && !ctx.readOnly,
                    placeholder = str(placeholder),
                    mono = true,
                    multiline = true,
                )
            }
        }
    }
}

/** 分享率行（0 = 关闭，占位显示「关闭」）。非法输入给出拒绝触感并还原。 */
internal fun GlassSectionScope.ratioRow(
    ctx: SettingsCtx,
    rowId: String,
    cfgKey: String,
    @StringRes title: Int,
) {
    if (!ctx.form.has(cfgKey)) return
    row {
        SettingRowBox(ctx, rowId, cfgKey) {
            val haptics = FluxTheme.haptics
            var tick by remember { mutableIntStateOf(0) }
            FluxFieldRow(title = str(title), cloud = ctx.synced(cfgKey)) {
                key(tick) {
                    CommitTextField(
                        value = BtRatio.text(ctx.form.double(cfgKey, 0.0)),
                        onCommit = { text ->
                            val wire = BtRatio.wire(text)
                            if (wire == null) {
                                haptics.reject()
                                tick++
                            } else {
                                ctx.editor.set(cfgKey, wire)
                            }
                        },
                        enabled = !ctx.readOnly,
                        placeholder = str(R.string.autoRetryOff),
                        keyboardType = KeyboardType.Decimal,
                    )
                }
            }
        }
    }
}

private fun unitLabelRes(unit: BtDurationUnit): Int = when (unit) {
    BtDurationUnit.Minutes -> R.string.timeUnitMinutes
    BtDurationUnit.Hours -> R.string.timeUnitHours
    BtDurationUnit.Days -> R.string.timeUnitDays
}

/**
 * 做种时长行：数字 + 单位选择 + ± 步进。数值键（`*_minutes`，☁︎ 同步）始终以分钟落库，
 * 单位键（仅本机）只记录展示单位；输入框显示 `分钟 ÷ 单位`，提交时换算回分钟；
 * 切换单位保留输入框里的数字并一次写回两个键。0 = 关闭。
 */
internal fun GlassSectionScope.durationRow(
    ctx: SettingsCtx,
    rowId: String,
    minutesKey: String,
    unitKey: String,
    @StringRes title: Int,
    @StringRes unitTitle: Int,
) {
    if (!ctx.form.has(minutesKey)) return
    row {
        SettingRowBox(ctx, rowId, minutesKey) {
            val haptics = FluxTheme.haptics
            val focusManager = LocalFocusManager.current
            val minutes = maxOf(0L, ctx.form.long(minutesKey, 0L))
            val unit = BtDurationUnit.fromWire(ctx.form.value(unitKey))
            var draft by remember(minutes, unit) { mutableStateOf(BtDuration.text(minutes, unit)) }
            var hadFocus by remember { mutableStateOf(false) }
            val latestDraft by rememberUpdatedState(draft)
            val name = str(title)
            val unitName = str(unitTitle)
            val off = str(R.string.autoRetryOff)
            val plus = str(R.string.mobileStepIncrease)
            val minus = str(R.string.mobileStepDecrease)
            val unitOptions = BtDurationUnit.entries.map { SelectOption(it.wire, str(unitLabelRes(it))) }

            fun commit(text: String) {
                if (text == BtDuration.text(minutes, unit)) return
                val next = BtDuration.minutes(text, unit)
                if (next == null) {
                    haptics.reject()
                    draft = BtDuration.text(minutes, unit)
                    return
                }
                if (next != minutes) ctx.editor.set(minutesKey, next.toString())
                draft = BtDuration.text(next, unit)
            }

            fun changeUnit(next: BtDurationUnit) {
                if (next == unit) return
                // 未编辑时保持分钟值不变，只切换展示单位；编辑过则保留输入框里的数字
                val dirty = draft != BtDuration.text(minutes, unit)
                val converted = if (dirty) BtDuration.minutes(draft, next) else minutes
                if (converted == null) {
                    haptics.reject()
                    return
                }
                if (converted == minutes) {
                    ctx.editor.set(unitKey, next.wire)
                } else {
                    ctx.editor.setNow(mapOf(minutesKey to converted.toString(), unitKey to next.wire))
                }
                draft = BtDuration.text(converted, next)
            }

            fun nudge(up: Boolean) {
                val next = BtDuration.stepped(minutes, unit, up)
                focusManager.clearFocus()
                if (next != minutes) ctx.editor.set(minutesKey, next.toString())
                draft = BtDuration.text(next, unit)
            }

            val latestMinutes by rememberUpdatedState(minutes)
            val latestUnit by rememberUpdatedState(unit)
            DisposableEffect(Unit) {
                onDispose {
                    // 只提交用户真正改过的输入（文本按两位小数显示，往返会丢精度）
                    if (latestDraft != BtDuration.text(latestMinutes, latestUnit)) {
                        val parsed = BtDuration.minutes(latestDraft, latestUnit)
                        if (parsed != null && parsed != latestMinutes) ctx.editor.set(minutesKey, parsed.toString())
                    }
                }
            }

            FluxFieldRow(title = name, cloud = ctx.synced(minutesKey)) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
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
                        placeholder = off,
                        mono = true,
                        enabled = !ctx.readOnly,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done),
                        keyboardActions = KeyboardActions(onDone = {
                            commit(draft)
                            focusManager.clearFocus()
                        }),
                        onFocusChange = { focused ->
                            if (focused) {
                                hadFocus = true
                            } else if (hadFocus) {
                                hadFocus = false
                                commit(draft)
                            }
                        },
                    )
                    FluxSelect(
                        value = str(unitLabelRes(unit)),
                        onClick = {
                            ctx.openPicker(
                                PickerSpec(unitName, unitOptions, unit.wire) { picked ->
                                    changeUnit(BtDurationUnit.fromWire(picked))
                                },
                            )
                        },
                        modifier = Modifier.width(96.dp),
                        enabled = !ctx.readOnly,
                    )
                    FluxIconButton(
                        FluxIcons.Minus, minus, { nudge(false) },
                        size = IconButtonSize.Sm, enabled = !ctx.readOnly && minutes > 0,
                    )
                    FluxIconButton(
                        FluxIcons.Plus, plus, { nudge(true) },
                        size = IconButtonSize.Sm, enabled = !ctx.readOnly,
                    )
                }
            }
            SettingFailure(ctx, unitKey)
        }
    }
}

// ───────────────────────────── 订阅状态 ─────────────────────────────

private class SubscriptionTexts(
    @StringRes val status: Int,
    @StringRes val updatedAt: Int,
    @StringRes val never: Int,
    @StringRes val updateNow: Int,
    @StringRes val updating: Int,
    @StringRes val failed: Int,
)

private fun textsFor(kind: SubscriptionKind) = when (kind) {
    SubscriptionKind.BtTrackers -> SubscriptionTexts(
        R.string.btTrackerSubStatus, R.string.btTrackerSubUpdatedAt, R.string.btTrackerSubNeverUpdated,
        R.string.btTrackerSubUpdateNow, R.string.btTrackerSubUpdating, R.string.btTrackerSubUpdateFailed,
    )
    SubscriptionKind.Ed2kServers -> SubscriptionTexts(
        R.string.ed2kServerSubStatus, R.string.ed2kServerSubUpdatedAt, R.string.ed2kServerSubNeverUpdated,
        R.string.ed2kServerSubUpdateNow, R.string.ed2kServerSubUpdating, R.string.ed2kServerSubUpdateFailed,
    )
    SubscriptionKind.Ed2kNodes -> SubscriptionTexts(
        R.string.ed2kNodesSubStatus, R.string.ed2kNodesSubUpdatedAt, R.string.ed2kNodesSubNeverUpdated,
        R.string.ed2kNodesSubUpdateNow, R.string.ed2kNodesSubUpdating, R.string.ed2kNodesSubUpdateFailed,
    )
}

private sealed interface RefreshPhase {
    data object Idle : RefreshPhase
    data object Refreshing : RefreshPhase
    data class Failed(val detail: String) : RefreshPhase
}

@Composable
private fun relativeTimeText(unix: Long): String {
    val now = System.currentTimeMillis() / 1000
    val locale = LocalLocale.current.platformLocale
    return when (val b = SubscriptionTime.bucket(unix, now)) {
        SubscriptionTime.JustNow -> str(R.string.mobileTimeJustNow)
        is SubscriptionTime.MinutesAgo -> str(R.string.mobileTimeMinutesAgo, "n" to b.n)
        is SubscriptionTime.HoursAgo -> str(R.string.mobileTimeHoursAgo, "n" to b.n)
        is SubscriptionTime.DaysAgo -> str(R.string.mobileTimeDaysAgo, "n" to b.n)
        SubscriptionTime.Absolute ->
            DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT, locale).format(Date(unix * 1000))
    }
}

/**
 * 订阅状态行（BT Tracker / eD2K 服务器 / Kad 节点共用）：已订阅条数 + 更新时间（或失败原因）+「立即更新」。
 * 数据来自只读的缓存 / 更新时间键；刷新调用慢 RPC，行内显示进度，页面其余部分保持可用。
 */
internal fun GlassSectionScope.subscriptionStatusRow(ctx: SettingsCtx, kind: SubscriptionKind) {
    if (!ctx.form.has(kind.cacheKey)) return
    row {
        SettingRowBox(ctx, kind.rowId, null) {
            SubscriptionStatus(ctx, kind)
        }
    }
}

@Composable
private fun SubscriptionStatus(ctx: SettingsCtx, kind: SubscriptionKind) {
    val container = LocalAppContainer.current
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val haptics = FluxTheme.haptics
    val scope = rememberCoroutineScope()
    val texts = remember(kind) { textsFor(kind) }
    var phase by remember { mutableStateOf<RefreshPhase>(RefreshPhase.Idle) }
    var fresh by remember { mutableStateOf<SubscriptionRefreshOutcome?>(null) }
    val model = SubscriptionStatusModel.make(kind, ctx.form, fresh)
    val failedText = str(texts.failed)
    val statusLine = when (val p = phase) {
        is RefreshPhase.Failed -> if (p.detail.isEmpty()) failedText else "$failedText: ${p.detail}"
        else -> if (model.updatedAt > 0) {
            str(texts.updatedAt, "time" to relativeTimeText(model.updatedAt))
        } else {
            str(texts.never)
        }
    }

    fun refresh() {
        if (phase == RefreshPhase.Refreshing) return
        phase = RefreshPhase.Refreshing
        scope.launch {
            try {
                val raw = container.session.callJson(
                    when (kind) {
                        SubscriptionKind.BtTrackers -> HostMethod.daemonBtTrackerSubscriptionRefresh
                        SubscriptionKind.Ed2kServers -> HostMethod.daemonEd2kServerSubscriptionRefresh
                        SubscriptionKind.Ed2kNodes -> HostMethod.daemonEd2kNodesSubscriptionRefresh
                    },
                )
                val outcome = when (kind) {
                    SubscriptionKind.BtTrackers -> TrackerSubRefreshResponse.fromJson(raw).outcome
                    SubscriptionKind.Ed2kServers -> Ed2kServerSubRefreshResponse.fromJson(raw).outcome
                    SubscriptionKind.Ed2kNodes -> Ed2kNodesSubRefreshResponse.fromJson(raw).outcome
                }
                fresh = outcome
                if (outcome.success) {
                    phase = RefreshPhase.Idle
                } else {
                    phase = RefreshPhase.Failed(outcome.error)
                    haptics.reject()
                    overlays.toast(
                        if (outcome.error.isEmpty()) failedText else "$failedText: ${outcome.error}",
                        FluxToastKind.Error,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: HostException) {
                val detail = actions.errorText(e)
                phase = RefreshPhase.Failed(detail)
                overlays.toast("$failedText: $detail", FluxToastKind.Error)
            }
        }
    }

    Column {
        FluxListRow(
            title = str(texts.status, "n" to model.count),
            subtitle = statusLine,
        )
        FluxActionRow(
            title = str(if (phase == RefreshPhase.Refreshing) texts.updating else texts.updateNow),
            onClick = { refresh() },
            icon = FluxIcons.RefreshCw,
            loading = phase == RefreshPhase.Refreshing,
            enabled = !ctx.readOnly,
        )
    }
}

// ───────────────────────────── eD2K ID 检测 ─────────────────────────────

private sealed interface IdCheckPhase {
    data object Idle : IdCheckPhase
    data object Running : IdCheckPhase
    data class Done(val net: Ed2kNetworkDto) : IdCheckPhase
    data class Failed(val detail: String) : IdCheckPhase
}

/**
 * HighID / LowID 检测行：调慢方法 `daemon.ed2k.idCheck`，结果只存本行状态（不落配置，离开页面即丢）。
 * 结果文本由 [Ed2kIdCheckFormat] 统一格式化。
 */
internal fun GlassSectionScope.ed2kIdCheckRow(ctx: SettingsCtx, rowId: String) {
    row {
        SettingRowBox(ctx, rowId, null) {
            Ed2kIdCheck(ctx)
        }
    }
}

@Composable
private fun Ed2kIdCheck(ctx: SettingsCtx) {
    val container = LocalAppContainer.current
    val actions = LocalTaskActions.current
    val scope = rememberCoroutineScope()
    var phase by remember { mutableStateOf<IdCheckPhase>(IdCheckPhase.Idle) }
    val high = str(R.string.ed2kIdHigh)
    val low = str(R.string.ed2kIdLow)
    val none = str(R.string.ed2kIdNone)
    val upnpOff = str(R.string.ed2kUpnpDisabled)
    val upnpPending = str(R.string.ed2kUpnpPending)
    val upnpMapped = str(R.string.ed2kUpnpMapped)
    val upnpFailed = str(R.string.ed2kUpnpFailed)
    val resultTemplate = str(R.string.ed2kIdCheckResult)
    val texts = Ed2kIdCheckTexts(
        idLabel = {
            when (it) {
                Ed2kIdKind.High -> high
                Ed2kIdKind.Low -> low
                Ed2kIdKind.None -> none
            }
        },
        upnpLabel = {
            when (it) {
                Ed2kUpnpState.Disabled -> upnpOff
                Ed2kUpnpState.Pending -> upnpPending
                Ed2kUpnpState.Mapped -> upnpMapped
                Ed2kUpnpState.Failed -> upnpFailed
            }
        },
        result = { id, server, port, upnp ->
            resultTemplate.fill("id" to id, "server" to server, "port" to port, "upnp" to upnp)
        },
        lowHint = str(R.string.ed2kIdLowHint),
        failed = str(R.string.ed2kIdCheckFailed),
    )
    val result = when (val p = phase) {
        IdCheckPhase.Idle, IdCheckPhase.Running -> null
        is IdCheckPhase.Done -> Ed2kIdCheckFormat.text(p.net, texts)
        is IdCheckPhase.Failed -> Ed2kIdCheckFormat.failed(p.detail, texts)
    }
    val running = phase == IdCheckPhase.Running

    fun run() {
        if (running) return
        phase = IdCheckPhase.Running
        scope.launch {
            phase = try {
                IdCheckPhase.Done(Ed2kNetworkDto.fromJson(container.session.callJson(HostMethod.daemonEd2kIdCheck)))
            } catch (e: CancellationException) {
                throw e
            } catch (e: HostException) {
                IdCheckPhase.Failed(actions.errorText(e))
            }
        }
    }

    Column {
        FluxListRow(
            title = str(R.string.ed2kIdCheck),
            subtitle = result ?: str(R.string.ed2kIdCheckDesc),
        )
        FluxActionRow(
            title = str(if (running) R.string.ed2kIdCheckRunning else R.string.ed2kIdCheckRun),
            onClick = { run() },
            icon = FluxIcons.RefreshCw,
            loading = running,
            enabled = !ctx.readOnly && !running,
        )
    }
}
