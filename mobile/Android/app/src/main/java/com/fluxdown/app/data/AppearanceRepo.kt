package com.fluxdown.app.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import com.fluxdown.core.format.FileSizeUnits
import com.fluxdown.core.format.Format
import com.fluxdown.fluxui.theme.FluxAccent
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/** 明暗模式：`appearance.theme_mode`。 */
enum class ThemeMode(val wire: String) {
    System("system"), Light("light"), Dark("dark");

    companion object {
        fun of(v: String?): ThemeMode = entries.firstOrNull { it.wire == v } ?: System
    }
}

/** 外观设置快照（docs 01-foundations §11.3）。 */
data class AppearanceState(
    val mode: ThemeMode = ThemeMode.System,
    /** `appearance.color_scheme`：blue / green / violet / rose / custom。 */
    val scheme: String = "blue",
    val customColor: Int = FluxAccent.DEFAULT_CUSTOM,
    /** 设备本地：跟随壁纸取色（只替换强调色槽位）。 */
    val dynamicColor: Boolean = false,
    /** 设备本地，不随主机切换或同步。 */
    val fileSizeUnits: FileSizeUnits = FileSizeUnits.Legacy,
) {
    /** 优先级：壁纸 > 自定义 > 预设。 */
    val accent: FluxAccent
        get() = when {
            dynamicColor -> FluxAccent.Wallpaper
            scheme == "custom" -> FluxAccent.Custom(customColor)
            else -> FluxAccent.Preset(scheme)
        }
}

/**
 * 外观偏好。`theme_mode / color_scheme / custom_color` 属于云同步键（接入 agent 后经
 * `agent.preferences.patch` 同步）；`dynamic_color` 与 `file_size_units` 为设备本地键。
 */
class AppearanceRepo(private val ds: DataStore<Preferences>) {
    private object K {
        val mode = stringPreferencesKey("appearance.theme_mode")
        val scheme = stringPreferencesKey("appearance.color_scheme")
        val custom = intPreferencesKey("appearance.custom_color")
        val dynamic = booleanPreferencesKey("appearance.dynamic_color")
        val fileSizeUnits = stringPreferencesKey("appearance.file_size_units")
    }

    val state: Flow<AppearanceState> = ds.data.map { p ->
        val units = FileSizeUnits.of(p[K.fileSizeUnits])
        Format.setFileSizeUnits(units)
        AppearanceState(
            mode = ThemeMode.of(p[K.mode]),
            scheme = p[K.scheme] ?: "blue",
            customColor = p[K.custom] ?: FluxAccent.DEFAULT_CUSTOM,
            dynamicColor = p[K.dynamic] ?: false,
            fileSizeUnits = units,
        )
    }

    suspend fun setMode(mode: ThemeMode) = ds.edit { it[K.mode] = mode.wire }
    suspend fun setScheme(scheme: String) = ds.edit { it[K.scheme] = scheme }
    suspend fun setCustomColor(argb: Int) = ds.edit {
        it[K.custom] = argb
        it[K.scheme] = "custom"
    }

    /** 主机偏好反向应用：一次写入；`null` = 保持本地不变（自定义色只写色值，方案由 [scheme] 单独给出）。 */
    suspend fun applyHost(mode: ThemeMode?, scheme: String?, customColor: Int?) = ds.edit {
        mode?.let { m -> it[K.mode] = m.wire }
        customColor?.let { c -> it[K.custom] = c }
        scheme?.let { s -> it[K.scheme] = s }
    }
    suspend fun setDynamicColor(enabled: Boolean) = ds.edit { it[K.dynamic] = enabled }
    suspend fun setFileSizeUnits(units: FileSizeUnits) = ds.edit {
        it[K.fileSizeUnits] = units.wire
    }
}
