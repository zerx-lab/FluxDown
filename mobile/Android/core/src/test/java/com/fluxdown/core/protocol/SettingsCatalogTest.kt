package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SettingsCatalogTest {
    private fun ok(key: String, value: String) = (SettingsCatalog.normalize(key, value) as Normalized.Ok).wire
    private fun rejected(key: String, value: String) = SettingsCatalog.normalize(key, value) is Normalized.Rejected

    @Test
    fun syncCatalogMatchesRustSpecCount() {
        // SYNC_SETTING_SPECS（native/protocol/src/settings.rs）：49 个 daemon 键（含 19 个 bt.guard.*）+ 29 个偏好键 + 1 个集合范围键
        assertEquals(49, SettingsCatalog.daemonSyncNames.size)
        assertEquals(29, SettingsCatalog.syncedPreferenceKeys.size)
        assertEquals(SettingsCatalog.daemonSyncNames.size, SettingsCatalog.syncNameToDaemonKey.size)
    }

    @Test
    fun btGuardKeysMirrorContract() {
        val guard = SettingsCatalog.daemonFields.filter { it.key.startsWith("bt_guard_") }
        // 20 个 daemon 键：19 个同步 + 设备本地上报开关
        assertEquals(20, guard.size)
        val synced = SettingsCatalog.daemonSyncNames.filterKeys { it.startsWith("bt_guard_") }
        assertEquals(19, synced.size)
        synced.forEach { (daemonKey, syncName) ->
            assertEquals("bt.guard." + daemonKey.removePrefix("bt_guard_"), syncName)
        }
        assertFalse(SettingsCatalog.isSynced("bt_guard_report_enabled"))
        assertNull(SettingsCatalog.field("bt_guard_cloud_cache"))
        assertTrue(rejected("bt_guard_cloud_cache", "{}"))
        assertEquals("false", SettingsCatalog.defaultWire("bt_guard_report_enabled"))
        assertEquals("observe", SettingsCatalog.defaultWire("bt_guard_rule_lag"))
        assertEquals("200", SettingsCatalog.defaultWire("bt_guard_excess_ratio_pct"))
        assertEquals("throttle", SettingsCatalog.defaultWire("bt_guard_downloading_max_action"))
    }

    @Test
    fun btGuardValueRangesMatchDaemon() {
        assertEquals("enforce", ok("bt_guard_rule_rewind", " enforce "))
        assertTrue(rejected("bt_guard_rule_rewind", "ban"))
        assertTrue(rejected("bt_guard_cloud_mode", ""))
        assertEquals("ban", ok("bt_guard_downloading_max_action", "ban"))
        assertTrue(rejected("bt_guard_downloading_max_action", "off"))
        assertEquals("110", ok("bt_guard_excess_ratio_pct", "110"))
        assertTrue(rejected("bt_guard_excess_ratio_pct", "109"))
        assertTrue(rejected("bt_guard_excess_ratio_pct", "1001"))
        assertEquals("0", ok("bt_guard_min_torrent_mib", "0"))
        assertTrue(rejected("bt_guard_min_torrent_mib", "1048577"))
        assertTrue(rejected("bt_guard_rewind_tolerance_pct", "101"))
        assertTrue(rejected("bt_guard_throttle_kib", "0"))
        assertTrue(rejected("bt_guard_ban_base_hours", "721"))
        assertTrue(rejected("bt_guard_ban_max_days", "366"))
        assertEquals("48", ok("bt_guard_ipv6_prefix", "48"))
        assertTrue(rejected("bt_guard_ipv6_prefix", "47"))
        assertTrue(rejected("bt_guard_ipv6_prefix", "129"))
    }

    @Test
    fun btGuardSyncedKeyRoutesThroughPreferencesButReportFlagStaysOnDaemon() {
        val result = SettingsWritePlan.make(
            mapOf("bt_guard_rule_lag" to "enforce", "bt_guard_report_enabled" to "true"),
        )
        val plan = (result as SettingsWritePlan.Result.Ok).plan
        assertEquals(mapOf("bt_guard_report_enabled" to "true"), plan.daemon)
        assertEquals(setOf("bt.guard.rule_lag"), plan.syncedPreferences.keys)
    }

    @Test
    fun normalizesLikeDaemon() {
        assertEquals("true", ok("bt_enable_dht", " 1 "))
        assertTrue(rejected("bt_enable_dht", "yes"))
        assertEquals("512", ok("default_segments", "+512"))
        assertTrue(rejected("default_segments", "513"))
        assertTrue(rejected("default_segments", "1.0"))
        assertTrue(rejected("default_segments", " "))
        assertEquals("-1", ok("max_auto_retries", "-1"))
        assertEquals("1.5", ok("bt_seed_ratio_limit", "1.50"))
        assertEquals("2", ok("bt_seed_ratio_limit", "2.0"))
        assertTrue(rejected("bt_seed_ratio_limit", "-0.1"))
        assertTrue(rejected("bt_seed_ratio_limit", "NaN"))
        assertEquals("socks5", ok("proxy_type", " socks5 "))
        assertTrue(rejected("proxy_type", "ftp"))
        assertEquals("ask", ok("file_exists_behavior", " ask "))
        assertTrue(rejected("file_exists_behavior", "prompt"))
        assertTrue(rejected("domain_conn_caps", ""))
        assertTrue(rejected("no_such_key", "x"))
        assertEquals("a b", ok("proxy_host", " a b "))
    }

    @Test
    fun userAgentRejectsControlCharactersButAllowsTab() {
        assertEquals("UA\tx", ok("global_user_agent", "  UA\tx "))
        assertTrue(rejected("global_user_agent", "UA\nx"))
    }

    @Test
    fun mirrorBaseRequiresHttpsHost() {
        assertEquals("", ok("component_mirror_base", "  /"))
        assertEquals("https://m.example.com/gh", ok("component_mirror_base", "https://m.example.com/gh//"))
        assertTrue(rejected("component_mirror_base", "http://m.example.com"))
        assertTrue(rejected("component_mirror_base", "https://m.example.com/?q"))
        assertTrue(rejected("component_mirror_base", "https:///path"))
    }

    @Test
    fun writePlanRoutesSyncedDaemonKeysThroughPreferences() {
        val result = SettingsWritePlan.make(
            mapOf(
                "max_concurrent_tasks" to "8",
                "bt_seed_ratio_limit" to "1.5",
                "proxy_mode" to "manual",
                "download.keep_awake" to "true",
                "download.silent_skip_selection" to "1",
            ),
        )
        val plan = (result as SettingsWritePlan.Result.Ok).plan
        assertEquals(mapOf("proxy_mode" to "manual"), plan.daemon)
        assertEquals(JsonValue.of(8L), plan.syncedPreferences["download.max_concurrent_tasks"])
        assertEquals(JsonValue.of(1.5), plan.syncedPreferences["bt.seed_ratio_limit"])
        assertEquals(JsonValue.True, plan.syncedPreferences["download.keep_awake"])
        assertEquals("max_concurrent_tasks", plan.syncedDaemonKeys["download.max_concurrent_tasks"])
        // 目录外的偏好只写本机
        assertEquals(mapOf("download.silent_skip_selection" to JsonValue.True), plan.localPreferences)
        assertFalse(plan.isEmpty)
    }

    @Test
    fun writePlanRejectsWholeBatchOnAnyInvalidValue() {
        val result = SettingsWritePlan.make(mapOf("proxy_mode" to "manual", "bt_port_start" to "0"))
        assertTrue(result is SettingsWritePlan.Result.Rejected)
        assertNotNull(SettingsWritePlan.validatePreferenceKey(SettingsCatalog.customThemesScopeKey))
        assertTrue(SettingsCatalog.isSynced("appearance.custom_themes.my-theme"))
        assertFalse(SettingsCatalog.isSynced(SettingsCatalog.customThemesScopeKey))
    }

    @Test
    fun wireFromJsonCoversScalarsOnly() {
        assertEquals("true", SettingsCatalog.wireFromJson(JsonValue.True))
        assertEquals("3", SettingsCatalog.wireFromJson(JsonValue.Num("3")))
        assertEquals("1.5", SettingsCatalog.wireFromJson(JsonValue.Num("1.50")))
        assertEquals(null, SettingsCatalog.wireFromJson(JsonValue.Arr(emptyList())))
    }
}
