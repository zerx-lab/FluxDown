package com.fluxdown.app.data

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * 设备本地设置（`SharedPreferences`；永远是这台手机，不随主机切换、不上云）。
 * 键名同 iOS `DeviceSettings`（03-settings §14 的 `mobile.*`）。
 *
 * 值是 Compose 快照状态：设置页 / 首页读数在组合内读取即订阅；通知服务在后台协程里读取同一份最新值。
 * 写入即落盘（`apply`）。
 */
class DeviceSettings private constructor(private val prefs: SharedPreferences) {
    /** 任务失败时通知（默认开）。 */
    var notifyOnFailure by mutableStateOf(prefs.getBoolean(KEY_NOTIFY_ON_FAIL, true))
        private set

    /** 完成通知附「打开」「分享」操作按钮（默认开）。 */
    var notifyActions by mutableStateOf(prefs.getBoolean(KEY_NOTIFY_ACTIONS, true))
        private set

    fun updateNotifyOnFailure(on: Boolean) {
        prefs.edit().putBoolean(KEY_NOTIFY_ON_FAIL, on).apply()
        notifyOnFailure = on
    }

    fun updateNotifyActions(on: Boolean) {
        prefs.edit().putBoolean(KEY_NOTIFY_ACTIONS, on).apply()
        notifyActions = on
    }

    var downloadFolderGuideDone by mutableStateOf(prefs.getBoolean("mobile.android.downloadFolderGuideDone", false))
        private set

    fun finishDownloadFolderGuide() {
        downloadFolderGuideDone = true
        prefs.edit().putBoolean("mobile.android.downloadFolderGuideDone", true).apply()
    }

    // ── 应用自更新（设备本地，不进主机偏好）──

    /** 自动检查更新（默认开）。 */
    var updateAutoCheck by mutableStateOf(prefs.getBoolean(KEY_UPDATE_AUTO_CHECK, true))
        private set

    /** 仅在非计量网络自动下载更新（默认开）。 */
    var updateWifiOnly by mutableStateOf(prefs.getBoolean(KEY_UPDATE_WIFI_ONLY, true))
        private set

    /** 用户选择的更新渠道；null = 未选择（按当前版本取默认，见 `UpdatePolicy.defaultChannel`）。 */
    var updateChannelChoice by mutableStateOf(prefs.getString(KEY_UPDATE_CHANNEL, null))
        private set

    /** 上次成功检查时刻（epoch ms；0 = 从未）。 */
    var updateLastCheckMs: Long
        get() = prefs.getLong(KEY_UPDATE_LAST_CHECK, 0L)
        set(value) = prefs.edit().putLong(KEY_UPDATE_LAST_CHECK, value).apply()

    /** 用户在下载中取消过的版本：不再自动下载。 */
    var updateDeclinedVersion: String
        get() = prefs.getString(KEY_UPDATE_DECLINED, "").orEmpty()
        set(value) = prefs.edit().putString(KEY_UPDATE_DECLINED, value).apply()

    /** 用户选择「跳过此版本」的版本：不再提示。 */
    var updateSkippedVersion: String
        get() = prefs.getString(KEY_UPDATE_SKIPPED, "").orEmpty()
        set(value) = prefs.edit().putString(KEY_UPDATE_SKIPPED, value).apply()

    /** 已提示过（通知 / 应用内 toast）的最新版本：同一版本只提示一次。 */
    var updateNotifiedVersion: String
        get() = prefs.getString(KEY_UPDATE_NOTIFIED, "").orEmpty()
        set(value) = prefs.edit().putString(KEY_UPDATE_NOTIFIED, value).apply()

    fun updateUpdateAutoCheck(on: Boolean) {
        prefs.edit().putBoolean(KEY_UPDATE_AUTO_CHECK, on).apply()
        updateAutoCheck = on
    }

    fun updateUpdateWifiOnly(on: Boolean) {
        prefs.edit().putBoolean(KEY_UPDATE_WIFI_ONLY, on).apply()
        updateWifiOnly = on
    }

    fun updateUpdateChannel(channel: String) {
        prefs.edit().putString(KEY_UPDATE_CHANNEL, channel).apply()
        updateChannelChoice = channel
    }

    companion object {
        private const val FILE = "fluxdown_device"
        private const val KEY_NOTIFY_ON_FAIL = "mobile.notify_on_fail"
        private const val KEY_NOTIFY_ACTIONS = "mobile.notify_actions"
        private const val KEY_UPDATE_AUTO_CHECK = "mobile.update_auto_check"
        private const val KEY_UPDATE_WIFI_ONLY = "mobile.update_wifi_only"
        private const val KEY_UPDATE_CHANNEL = "mobile.update_channel"
        private const val KEY_UPDATE_LAST_CHECK = "mobile.update_last_check_ms"
        private const val KEY_UPDATE_DECLINED = "mobile.update_declined_version"
        private const val KEY_UPDATE_SKIPPED = "mobile.update_skipped_version"
        private const val KEY_UPDATE_NOTIFIED = "mobile.update_notified_version"

        @Volatile
        private var instance: DeviceSettings? = null

        /** 进程内单例（应用 Context）。 */
        fun of(context: Context): DeviceSettings =
            instance ?: synchronized(this) {
                instance ?: DeviceSettings(context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE))
                    .also { instance = it }
            }

        /** 已创建的单例（尚未创建为 null）；无 Context 的读数函数用。 */
        fun peek(): DeviceSettings? = instance
    }
}
