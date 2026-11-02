package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 云同步状态 DTO、阶段优先级、范围分组（同 iOS `LinkProtocolTests` 的同步部分）。 */
class SyncTest {
    @Test
    fun syncStatusDecodesWithDefaults() {
        val full = SyncStatusDto.fromJson(
            Json.parse(
                """{"enabled":true,"revision":42,"dirtyKeys":["ui.show_sidebar_rss"],"lastError":null,"connected":true,"halted":false,"lastSyncedAtUnixMs":1760000000000,"localOnlyKeys":["appearance.theme_mode"]}""",
            ),
        )
        assertTrue(full.enabled)
        assertEquals(42L, full.revision)
        assertEquals(listOf("ui.show_sidebar_rss"), full.dirtyKeys)
        assertEquals(1_760_000_000_000L, full.lastSyncedAtUnixMs)
        assertEquals(listOf("appearance.theme_mode"), full.localOnlyKeys)
        // 旧 agent 不下发的字段取默认。
        assertEquals(SyncStatusDto(), SyncStatusDto.fromJson(Json.parse("""{"enabled":false,"revision":0,"dirtyKeys":[]}""")))
        // 分区缺失 / null 读作未启用。
        assertEquals(SyncStatusDto(), SyncStatusDto.fromJson(null))
        assertEquals(SyncStatusDto(), SyncStatusDto.fromJson(Json.parse("null")))
    }

    @Test
    fun syncPhasePriority() {
        fun phase(s: SyncStatusDto) = SyncRules.phase(s)
        assertEquals(SyncPhase.Off, phase(SyncStatusDto(enabled = false, lastError = "x", halted = true)))
        assertEquals(SyncPhase.Halted, phase(SyncStatusDto(enabled = true, lastError = "x", halted = true)))
        assertEquals(SyncPhase.Error, phase(SyncStatusDto(enabled = true, lastError = "x", connected = true)))
        assertEquals(SyncPhase.Error, phase(SyncStatusDto(enabled = true, lastErrorReason = "cloudUnreachable", connected = true)))
        assertEquals(SyncPhase.Connecting, phase(SyncStatusDto(enabled = true, connected = false)))
        assertEquals(SyncPhase.Syncing, phase(SyncStatusDto(enabled = true, dirtyKeys = listOf("a"), connected = true)))
        assertEquals(SyncPhase.Synced, phase(SyncStatusDto(enabled = true, connected = true)))
    }

    @Test
    fun syncReasonKeyLocalizesByReasonWithGenericFallback() {
        fun key(reason: String?, raw: String? = null) = SyncRules.reasonKey(SyncStatusDto(lastError = raw, lastErrorReason = reason))
        assertEquals("cloudSyncErrorOnlineLimit", key("onlineDeviceLimit"))
        assertEquals("cloudSyncErrorDeviceUntrusted", key("deviceUntrusted"))
        assertEquals("cloudSyncErrorNetwork", key("cloudUnreachable"))
        assertEquals("cloudSyncErrorGeneric", key("mystery"))
        assertEquals("cloudSyncErrorGeneric", key(null, raw = "raw diagnostic text"))
    }

    @Test
    fun scopeGroupStateAndToggleRule() {
        val appearance = SyncRules.groups.first { it.id == SyncGroupId.Appearance }
        assertEquals(SyncGroupState.Sync, appearance.state(emptyList()))
        assertEquals(SyncGroupState.Sync, appearance.state(listOf("download.keep_awake")))
        assertEquals(SyncGroupState.Mixed, appearance.state(listOf("appearance.theme_mode")))
        assertEquals(SyncGroupState.Local, appearance.state(appearance.keys))

        // 只有全部参与同步的分组切为本设备专属；本设备专属与混合都恢复同步。
        assertEquals(SyncLocalOnlyParams(appearance.keys, true), appearance.toggleParams(SyncGroupState.Sync))
        assertFalse(appearance.toggleParams(SyncGroupState.Local).localOnly)
        assertFalse(appearance.toggleParams(SyncGroupState.Mixed).localOnly)
    }

    @Test
    fun setLocalOnlyParamsMatchWireShape() {
        assertEquals(
            Json.parse("""{"keys":["a.b","c"],"localOnly":true}"""),
            SyncLocalOnlyParams(listOf("a.b", "c"), true).toJson(),
        )
    }

    @Test
    fun syncGroupsPartitionTheFullCatalogWithoutOverlap() {
        val all = SyncRules.groups.flatMap { it.keys }
        assertEquals(79, all.size)
        assertEquals(all.size, all.toSet().size)
        assertEquals(SyncGroupId.values().toList(), SyncRules.groups.map { it.id })
        assertTrue("custom_categories" in all)
        assertTrue("bt.seed_max_active" in all)
        assertNull(SyncRules.groups.firstOrNull { it.keys.isEmpty() })
    }

    @Test
    fun syncGroupsCoverExactlyTheSyncedCatalog() {
        val catalog = SettingsCatalog.daemonSyncNames.values.toSet() +
            SettingsCatalog.syncedPreferenceKeys +
            SettingsCatalog.customThemesScopeKey
        assertEquals(catalog, SyncRules.groups.flatMap { it.keys }.toSet())
    }

    @Test
    fun relativeSyncTime() {
        val now = 1_760_000_000_000L
        assertEquals(SyncRules.Ago.JustNow, SyncRules.ago(now - 30_000, now))
        assertEquals(SyncRules.Ago.Minutes(5), SyncRules.ago(now - 5 * 60_000, now))
        assertEquals(SyncRules.Ago.Hours(3), SyncRules.ago(now - 3 * 3_600_000, now))
        assertEquals(SyncRules.Ago.Days(2), SyncRules.ago(now - 2 * 86_400_000, now))
        // 时钟回拨（未来时间）按刚刚处理。
        assertEquals(SyncRules.Ago.JustNow, SyncRules.ago(now + 5_000, now))
    }
}
