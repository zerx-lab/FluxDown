package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** BT / eD2K 设置页纯逻辑与订阅协议（对应 iOS `SettingsBtEd2kTests` + `SubscriptionsProtocolTests`）。 */
class BtSettingsTest {
    private fun form(values: Map<String, String>) = SettingsForm(host = values)

    // ── 时长换算 ──

    @Test fun durationTextShowsValueInTheChosenUnit() {
        assertTrue(BtDuration.text(0, BtDurationUnit.Hours).isEmpty())
        assertEquals("90", BtDuration.text(90, BtDurationUnit.Minutes))
        assertEquals("1.5", BtDuration.text(90, BtDurationUnit.Hours))
        assertEquals("2", BtDuration.text(120, BtDurationUnit.Hours))
        assertEquals("2", BtDuration.text(2_880, BtDurationUnit.Days))
    }

    @Test fun durationParsesBackToMinutes() {
        assertEquals(0L, BtDuration.minutes("", BtDurationUnit.Days))
        assertEquals(90L, BtDuration.minutes("1.5", BtDurationUnit.Hours))
        assertEquals(90L, BtDuration.minutes("1,5", BtDurationUnit.Hours))
        assertEquals(2_880L, BtDuration.minutes("2", BtDurationUnit.Days))
        assertNull(BtDuration.minutes("abc", BtDurationUnit.Minutes))
        assertNull(BtDuration.minutes("-1", BtDurationUnit.Minutes))
    }

    @Test fun switchingUnitKeepsTheTypedNumber() {
        assertEquals(3L, BtDuration.minutes("3", BtDurationUnit.Minutes))
        assertEquals(180L, BtDuration.minutes("3", BtDurationUnit.Hours))
    }

    @Test fun durationStepMovesByOneUnitAndStopsAtZero() {
        assertEquals(120L, BtDuration.stepped(60, BtDurationUnit.Hours, up = true))
        assertEquals(0L, BtDuration.stepped(30, BtDurationUnit.Hours, up = false))
        assertEquals(0L, BtDuration.stepped(0, BtDurationUnit.Days, up = false))
    }

    @Test fun unknownUnitFallsBackToMinutes() {
        assertEquals(BtDurationUnit.Minutes, BtDurationUnit.fromWire(null))
        assertEquals(BtDurationUnit.Minutes, BtDurationUnit.fromWire("weeks"))
        assertEquals(BtDurationUnit.Days, BtDurationUnit.fromWire("days"))
    }


    // ── 分享率 ──

    @Test fun ratioTextAndWire() {
        assertEquals("", BtRatio.text(0.0))
        assertEquals("2", BtRatio.text(2.0))
        assertEquals("1.25", BtRatio.text(1.25))
        assertEquals("0", BtRatio.wire(""))
        assertEquals("1.5", BtRatio.wire("1,5"))
        assertEquals("2", BtRatio.wire("2.0"))
        assertNull(BtRatio.wire("-1"))
        assertNull(BtRatio.wire("1f"))
    }
    // ── 可见性 ──

    @Test fun seedingRowsHideWhenSeedingIsOff() {
        val off = form(mapOf("bt_seed_enabled" to "false"))
        assertEquals(listOf(BtSettingsRow.SeedEnabled), BtSettingsRow.visible(BtSettingsTab.Seeding, off))
        val on = form(mapOf("bt_seed_enabled" to "true"))
        assertEquals(9, BtSettingsRow.visible(BtSettingsTab.Seeding, on).size)
    }

    @Test fun disablingBtHidesEverythingButTheMasterSwitch() {
        val off = form(mapOf("bt_enabled" to "false", "bt_seed_enabled" to "true"))
        assertEquals(listOf(BtSettingsRow.Enabled), BtSettingsRow.entries.filter { it.isVisible(off) })
        assertEquals(listOf(BtSettingsTab.General), BtSettingsRow.visibleTabs(off))
        val on = form(mapOf("bt_enabled" to "true"))
        assertEquals(BtSettingsTab.entries, BtSettingsRow.visibleTabs(on))
        assertEquals(BtSettingsRow.Enabled, BtSettingsRow.visible(BtSettingsTab.General, on).first())
    }

    @Test fun daemonRowsNeedALoadedConfig() {
        val unloaded = form(emptyMap())
        assertTrue(BtSettingsRow.entries.none { it.isVisible(unloaded) })
        assertTrue(Ed2kSettingsRow.entries.none { it.isVisible(unloaded) })
        val loaded = form(mapOf("bt_enable_dht" to "true"))
        assertEquals(4, BtSettingsRow.visible(BtSettingsTab.General, loaded).size)
        assertEquals(14, BtSettingsRow.visible(BtSettingsTab.Connection, loaded).size)
        assertEquals(4, Ed2kSettingsRow.visible(Ed2kSettingsTab.General, loaded).size)
        assertEquals(4, Ed2kSettingsRow.visible(Ed2kSettingsTab.Servers, loaded).size)
        assertEquals(
            listOf(Ed2kSettingsRow.NodesSubUrls, Ed2kSettingsRow.NodesSubStatus),
            Ed2kSettingsRow.visible(Ed2kSettingsTab.Kad, loaded),
        )
    }

    @Test fun rowKeysResolveInTheCatalogAndIdsAreUnique() {
        for (row in BtSettingsRow.entries) assertNotNull(row.toString(), SettingsCatalog.field(row.configKey))
        for (row in Ed2kSettingsRow.entries) assertNotNull(row.toString(), SettingsCatalog.field(row.configKey))
        val ids = BtSettingsRow.entries.map { it.id } + Ed2kSettingsRow.entries.map { it.id }
        assertEquals(ids.size, ids.toSet().size)
        assertTrue(SettingsCatalog.isSynced(BtSettingsRow.SeedTimeLimit.configKey))
        assertNotNull(SettingsCatalog.field("bt_seed_time_limit_unit"))
        assertFalse(SettingsCatalog.isSynced("bt_seed_time_limit_unit"))
    }

    @Test fun searchTargetsMapToTheirTab() {
        assertEquals(BtSettingsTab.Tracker, BtSettingsRow.tabForRowId("bt.trackerSubUrls"))
        assertEquals(BtSettingsTab.Seeding, BtSettingsRow.tabForRowId("bt.seedThenAction"))
        assertNull(BtSettingsRow.tabForRowId("download.maxConcurrent"))
        assertEquals(Ed2kSettingsTab.Servers, Ed2kSettingsRow.tabForRowId("ed2k.serverSubStatus"))
        assertEquals(Ed2kSettingsTab.Kad, Ed2kSettingsRow.tabForRowId("ed2k.nodesSubUrls"))
        assertEquals(Ed2kSettingsTab.General, Ed2kSettingsRow.tabForRowId("ed2k.idCheck"))
    }

    // ── 连接页签（GPUI `bt.rs` 的五组与条件行） ──

    @Test fun connectionTabMirrorsGpuiOrderAndGroups() {
        val on = form(mapOf("bt_enabled" to "true", "bt_i2p_enabled" to "true"))
        assertEquals(
            listOf(
                BtSettingsRow.PortStart, BtSettingsRow.PortEnd, BtSettingsRow.Upnp, BtSettingsRow.PortMappingStatus,
                BtSettingsRow.Utp, BtSettingsRow.UtpCongestion, BtSettingsRow.MseMode, BtSettingsRow.UploadSlots,
                BtSettingsRow.Dht, BtSettingsRow.Lsd, BtSettingsRow.Webseed,
                BtSettingsRow.MaxConnections, BtSettingsRow.MaxConnectionsPerTorrent,
                BtSettingsRow.I2pEnabled, BtSettingsRow.I2pSamHost, BtSettingsRow.I2pSamPort, BtSettingsRow.I2pMixed,
            ),
            BtSettingsRow.visible(BtSettingsTab.Connection, on),
        )
        assertEquals(
            listOf(
                BtSettingsGroup.Listen, BtSettingsGroup.Transport, BtSettingsGroup.Sources,
                BtSettingsGroup.Limits, BtSettingsGroup.I2p,
            ),
            BtSettingsRow.visible(BtSettingsTab.Connection, on).map { it.group }.distinct(),
        )
        assertEquals(
            listOf(BtSettingsTab.General, BtSettingsTab.Connection, BtSettingsTab.Tracker, BtSettingsTab.Seeding, BtSettingsTab.Guard),
            BtSettingsRow.visibleTabs(on),
        )
    }

    @Test fun connectionConditionalRows() {
        val off = form(
            mapOf("bt_enable_upnp" to "false", "bt_enable_utp" to "false", "bt_i2p_enabled" to "false"),
        )
        val rows = BtSettingsRow.visible(BtSettingsTab.Connection, off)
        assertFalse(BtSettingsRow.PortMappingStatus in rows)
        assertFalse(BtSettingsRow.UtpCongestion in rows)
        assertFalse(BtSettingsRow.I2pSamHost in rows)
        assertFalse(BtSettingsRow.I2pSamPort in rows)
        assertFalse(BtSettingsRow.I2pMixed in rows)
        assertTrue(BtSettingsRow.I2pEnabled in rows)
        assertTrue(BtSettingsRow.Upnp in rows && BtSettingsRow.Utp in rows)
    }

    @Test fun trackerTabHasAnnouncePolicyLast() {
        val rows = BtSettingsRow.visible(BtSettingsTab.Tracker, form(mapOf("bt_enabled" to "true")))
        assertEquals(listOf(BtSettingsRow.AnnounceAllTiers, BtSettingsRow.AnnounceAllTrackers), rows.takeLast(2))
        assertEquals(BtSettingsTab.Connection, BtSettingsRow.tabForRowId("bt.i2pSamPort"))
        assertEquals(BtSettingsTab.Tracker, BtSettingsRow.tabForRowId("bt.announceAllTiers"))
    }

    @Test fun newConnectionKeysMirrorDaemonConfigAndStayDeviceLocal() {
        val expected = mapOf(
            "bt_enable_lsd" to "true",
            "bt_utp_congestion" to "ledbat",
            "bt_max_connections" to "0",
            "bt_max_connections_per_torrent" to "512",
            "bt_enable_webseed" to "true",
            "bt_announce_all_tiers" to "true",
            "bt_announce_all_trackers" to "true",
            "bt_proxy_enabled" to "false",
            "bt_proxy_source" to "manual",
            "bt_proxy_peers" to "true",
            "bt_i2p_enabled" to "false",
            "bt_i2p_sam_host" to "127.0.0.1",
            "bt_i2p_sam_port" to "7656",
            "bt_i2p_mixed" to "false",
        )
        for ((key, def) in expected) {
            assertEquals(key, def, SettingsCatalog.defaultWire(key))
            assertEquals(key, SettingField.Store.Daemon, SettingsCatalog.field(key)?.store)
            assertFalse(key, SettingsCatalog.isSynced(key))
        }
        assertEquals(SettingField.Kind.Integer(0, 65_535), SettingsCatalog.field("bt_max_connections")?.kind)
        assertEquals(SettingField.Kind.Integer(1, 4096), SettingsCatalog.field("bt_max_connections_per_torrent")?.kind)
        assertEquals(SettingField.Kind.Integer(1, 65_535), SettingsCatalog.field("bt_i2p_sam_port")?.kind)
        assertEquals(listOf("ledbat", "cubic"), BtUtpCongestion.all)
        assertEquals("ledbat", BtUtpCongestion.normalize("bogus"))
        assertEquals("cubic", BtUtpCongestion.normalize(" cubic "))
    }

    // ── 端口映射状态（同 GPUI `port_mapping_segments`） ──

    @Test fun portStatusIdleWhenNotRunning() {
        val status = BtPortStatus.fromJson(Json.parse("""{"running":false}"""))
        assertEquals(listOf(BtPortSegment.Idle), BtPortMappingView.segments(status))
    }

    @Test fun portStatusSegmentsPerMappingAndExternalIp() {
        val status = BtPortStatus.fromJson(
            Json.parse(
                """{"running":true,"listenPort":6881,"mappings":[
                {"protocol":"tcp","internalPort":6881,"externalPort":50123,"method":"natPmp","externalIp":" 203.0.113.7 ","state":"mapped"},
                {"protocol":"udp","internalPort":6881,"externalPort":6881,"method":"upnp","state":"mapped"},
                {"protocol":"tcp","internalPort":6882,"state":"pending"},
                {"protocol":"udp","internalPort":6882,"state":"failed","message":"  router\n  said no  "},
                {"protocol":"udp","internalPort":6883,"state":"weird"}
                ]}""",
            ),
        )
        assertEquals(6881L, status.listenPort)
        assertEquals(
            listOf(
                BtPortSegment.Mapping("TCP 6881 → 50123", BtPortSegmentState.Mapped("NAT-PMP")),
                BtPortSegment.Mapping("UDP 6881", BtPortSegmentState.Mapped("UPnP")),
                BtPortSegment.Mapping("TCP 6882", BtPortSegmentState.Pending),
                BtPortSegment.Mapping("UDP 6882", BtPortSegmentState.Failed("router\n  said no")),
                BtPortSegment.Mapping("UDP 6883", BtPortSegmentState.Other("weird")),
                BtPortSegment.ExternalIp("203.0.113.7"),
            ),
            BtPortMappingView.segments(status),
        )
        assertEquals("router said no", BtPortMappingView.singleLine("router\n  said no"))
        assertEquals("PCP", BtPortMappingView.methodLabel("pcp"))
        assertEquals("future", BtPortMappingView.methodLabel("future"))
    }

    @Test fun portStatusRunningWithoutMappingsIsEmpty() {
        // 旧 daemon / 空字段不下发：缺省即 0 / 空
        val status = BtPortStatus.fromJson(Json.parse("""{"running":true}"""))
        assertEquals(0L, status.listenPort)
        assertTrue(BtPortMappingView.segments(status).isEmpty())
    }

    @Test fun portRangeRequiresEndAtLeastStart() {
        assertTrue(BtPortRange.isValid(6881, 6891))
        assertTrue(BtPortRange.isValid(7000, 7000))
        assertFalse(BtPortRange.isValid(7000, 6999))
    }

    // ── 订阅状态 ──

    @Test fun statusCountsCacheEntriesAndStoredTime() {
        val bt = form(mapOf("bt_tracker_sub_cache" to "udp://a:1\n\nudp://b:2\n", "bt_tracker_sub_updated_at" to "1759622400"))
        assertEquals(SubscriptionStatusModel(2, 1_759_622_400), SubscriptionStatusModel.make(SubscriptionKind.BtTrackers, bt, null))
        val ed2k = form(mapOf("ed2k_server_sub_cache" to "1.1.1.1:1,2.2.2.2:2,3.3.3.3:3"))
        assertEquals(SubscriptionStatusModel(3, 0), SubscriptionStatusModel.make(SubscriptionKind.Ed2kServers, ed2k, null))
        val nodes = form(mapOf("ed2k_nodes_dat_count" to "321", "ed2k_nodes_dat_updated_at" to "1759622400"))
        assertEquals(SubscriptionStatusModel(321, 1_759_622_400), SubscriptionStatusModel.make(SubscriptionKind.Ed2kNodes, nodes, null))
        val garbage = form(mapOf("ed2k_nodes_dat_count" to "-5"))
        assertEquals(SubscriptionStatusModel(0, 0), SubscriptionStatusModel.make(SubscriptionKind.Ed2kNodes, garbage, null))
    }

    @Test fun nodesRefreshResponseIsLenient() {
        val full = Json.parse("""{"success":true,"nodeCount":120,"okSources":1,"totalSources":2,"updatedAt":99,"error":""}""")
        assertEquals(SubscriptionRefreshOutcome(true, 120, 1, 2, 99, ""), Ed2kNodesSubRefreshResponse.fromJson(full).outcome)
        val empty = Json.parse("{}")
        assertEquals(SubscriptionRefreshOutcome(false, 0, 0, 0, 0, ""), Ed2kNodesSubRefreshResponse.fromJson(empty).outcome)
    }

    @Test fun freshSuccessWinsOnlyWhenNewerThanTheSnapshot() {
        val stored = form(mapOf("bt_tracker_sub_cache" to "udp://a:1", "bt_tracker_sub_updated_at" to "100"))
        val newer = SubscriptionRefreshOutcome(true, 50, 2, 2, 200, "")
        assertEquals(SubscriptionStatusModel(50, 200), SubscriptionStatusModel.make(SubscriptionKind.BtTrackers, stored, newer))
        val caughtUp = form(mapOf("bt_tracker_sub_cache" to "a\nb", "bt_tracker_sub_updated_at" to "200"))
        assertEquals(SubscriptionStatusModel(2, 200), SubscriptionStatusModel.make(SubscriptionKind.BtTrackers, caughtUp, newer))
        val failed = SubscriptionRefreshOutcome(false, 0, 0, 2, 300, "x")
        assertEquals(SubscriptionStatusModel(1, 100), SubscriptionStatusModel.make(SubscriptionKind.BtTrackers, stored, failed))
    }

    @Test fun subscriptionTimeIsRelativeWithinAWeekAndAbsoluteBeyond() {
        val now = 1_759_700_000L
        assertEquals(SubscriptionTime.HoursAgo(3), SubscriptionTime.bucket(now - 3 * 3600, now))
        assertEquals(SubscriptionTime.MinutesAgo(5), SubscriptionTime.bucket(now - 5 * 60, now))
        assertEquals(SubscriptionTime.DaysAgo(2), SubscriptionTime.bucket(now - 2 * 86_400, now))
        assertEquals(SubscriptionTime.Absolute, SubscriptionTime.bucket(now - 30 * 86_400, now))
    }

    // ── 读数 ──

    @Test fun btReadoutListsActiveFeatures() {
        assertNull(BtReadout.text(form(emptyMap()), "Seeding"))
        val f = form(mapOf("bt_enable_dht" to "true", "bt_seed_enabled" to "false", "bt_port_start" to "7000", "bt_port_end" to "7000"))
        assertEquals("DHT · 7000", BtReadout.text(f, "Seeding"))
    }

    @Test fun ed2kReadoutCountsDedupedServers() {
        val f = form(mapOf("ed2k_enable_kad" to "true", "ed2k_server_list" to "A:1,b:2", "ed2k_server_sub_cache" to "a:1,c:3"))
        assertEquals(3, Ed2kReadout.serverCount(f))
        assertTrue(Ed2kReadout.text(f) { "$it servers" }!!.startsWith("Kad · "))
        val none = form(mapOf("ed2k_enable_kad" to "false", "ed2k_server_list" to ""))
        assertNull(Ed2kReadout.text(none) { "$it servers" })
    }

    // ── 订阅 DTO ──

    @Test fun trackerRefreshDecodesWireShape() {
        val dto = TrackerSubRefreshResponse.fromJson(
            Json.parse("""{"success":true,"trackerCount":128,"okSources":2,"totalSources":3,"updatedAt":1759622400,"error":""}"""),
        )
        assertTrue(dto.success)
        assertEquals(128L, dto.trackerCount)
        assertEquals(2L, dto.okSources)
        assertEquals(3L, dto.totalSources)
        assertEquals(1_759_622_400L, dto.updatedAt)
        assertTrue(dto.error.isEmpty())
        assertEquals(128L, dto.outcome.count)
    }

    @Test fun trackerRefreshFailureCarriesErrorSummary() {
        val dto = TrackerSubRefreshResponse.fromJson(
            Json.parse("""{"success":false,"trackerCount":0,"okSources":0,"totalSources":2,"updatedAt":1759622400,"error":"all sources failed"}"""),
        )
        assertFalse(dto.success)
        assertEquals("all sources failed", dto.error)
        assertEquals(1_759_622_400L, dto.outcome.updatedAt)
    }

    @Test fun ed2kRefreshDecodesWireShape() {
        val dto = Ed2kServerSubRefreshResponse.fromJson(
            Json.parse("""{"success":true,"serverCount":42,"okSources":1,"totalSources":1,"updatedAt":1759622500,"error":""}"""),
        )
        assertTrue(dto.success)
        assertEquals(42L, dto.serverCount)
        assertEquals(1L, dto.totalSources)
        assertEquals(42L, dto.outcome.count)
    }

    @Test fun refreshDecodingIsLenient() {
        assertEquals(TrackerSubRefreshResponse(), TrackerSubRefreshResponse.fromJson(Json.parse("{}")))
        val nulls = Ed2kServerSubRefreshResponse.fromJson(Json.parse("""{"success":true,"error":null,"updatedAt":null,"unknown":1}"""))
        assertTrue(nulls.success)
        assertTrue(nulls.error.isEmpty())
        assertEquals(0L, nulls.updatedAt)
    }

    // ── 列表格式 ──

    @Test fun commaListIsEditedOnePerLine() {
        val stored = "176.123.5.89:4725,45.82.80.155:5687, 85.121.5.137:4232"
        assertEquals("176.123.5.89:4725\n45.82.80.155:5687\n85.121.5.137:4232", SubscriptionListFormat.Comma.toEditor(stored))
    }

    @Test fun commaListReadsLegacyLineSeparatedValue() {
        val legacy = "1.2.3.4:4661\r\n5.6.7.8:80\n"
        assertEquals("1.2.3.4:4661\n5.6.7.8:80", SubscriptionListFormat.Comma.toEditor(legacy))
        assertEquals(2, SubscriptionListFormat.Comma.count(legacy))
    }

    @Test fun commaListSavesTrimmedDedupedCsv() {
        val text = " 1.2.3.4:4661 \n\nExample.org:4242\n1.2.3.4:4661\nexample.org:4242\n"
        assertEquals("1.2.3.4:4661,Example.org:4242", SubscriptionListFormat.Comma.toStored(text))
    }

    @Test fun trailingNewlineDoesNotChangeStoredValue() {
        for (format in SubscriptionListFormat.entries) {
            assertEquals(format.toStored("a:1"), format.toStored("a:1\n"))
        }
    }

    @Test fun cacheCountsFollowStorageSeparator() {
        assertEquals(3, SubscriptionListFormat.Comma.count("1.1.1.1:1,2.2.2.2:2,3.3.3.3:3"))
        assertEquals(2, SubscriptionListFormat.Lines.count("udp://a:1\n\nudp://b:2\n"))
        assertEquals(0, SubscriptionListFormat.Comma.count(""))
        assertEquals(2, SubscriptionListFormat.Lines.count("udp://a:1\r\nudp://b:2\r\n"))
    }

    @Test fun linesFormatKeepsInnerTextVerbatim() {
        val text = "# comment\nhttps://a.example/list.txt\n"
        assertEquals("# comment\nhttps://a.example/list.txt", SubscriptionListFormat.Lines.toStored(text))
        assertEquals(text, SubscriptionListFormat.Lines.toEditor(text))
    }
}
