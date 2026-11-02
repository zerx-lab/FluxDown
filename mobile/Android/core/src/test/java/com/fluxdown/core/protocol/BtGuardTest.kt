package com.fluxdown.core.protocol

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BtGuardTest {
    private fun form(vararg pairs: Pair<String, String>) =
        SettingsForm(host = mapOf("bt_enabled" to "true") + pairs)

    @Test
    fun uiMethodsExcludeAgentOnlyOnes() {
        assertEquals(7, BtGuardMethods.all.size)
        assertFalse(HostMethod.daemonBtGuardCloudApply in BtGuardMethods.all)
        assertFalse(HostMethod.daemonBtGuardEvidenceDrain in BtGuardMethods.all)
        assertTrue(BtGuardMethods.all.all { it.startsWith("daemon.bt.guard.") })
    }

    @Test
    fun decodesStateWithDefaultsForMissingFields() {
        val state = BtGuardState.fromJson(
            Json.parse(
                """{"enabled":true,"sessionActive":false,
                   "stats":{"todayObserved":3,"todayEnforced":1,"todayBlockedConnections":9,"todayRefusedBytes":4096},
                   "cloud":{"sequence":12,"updatedAt":100,"expiresAt":200,"entries":5,"clientRules":2,"stale":true,
                            "rejectedReason":"sequence rollback","attribution":["BTN CC BY 4.0"]},
                   "filterSources":[{"url":"https://x/l.txt","ok":false,"entries":0,"updatedAt":0,"error":"timeout"}],
                   "counts":{"allow":1,"manual":2,"auto":3,"cloud":4,"file":5}}""",
            ),
        )
        assertTrue(state.enabled)
        assertFalse(state.sessionActive)
        assertEquals(4096L, state.stats.todayRefusedBytes)
        assertEquals(12L, state.cloud.sequence)
        assertTrue(state.cloud.stale)
        assertEquals("sequence rollback", state.cloud.rejectedReason)
        assertNull(state.cloud.lastError)
        assertEquals(listOf("BTN CC BY 4.0"), state.cloud.attribution)
        assertEquals("timeout", state.filterSources.single().error)
        assertEquals(15L, state.counts.total)
        assertEquals(4L, state.counts.of(BtGuardLayer.Cloud))

        val empty = BtGuardState.fromJson(Json.parse("{}"))
        assertFalse(empty.enabled)
        assertTrue(empty.filterSources.isEmpty())
        assertEquals(0L, empty.counts.total)
    }

    @Test
    fun decodesEntriesIncludingPermanentAndUnknownLayer() {
        val page = BtGuardEntriesPage.fromJson(
            Json.parse(
                """{"total":3,"items":[
                  {"id":9,"layer":"auto","cidr":"1.2.3.4","action":"ban","rule":"excessive","reason":"r",
                   "evidence":{"deliveredBytes":100,"ratioMilli":3700},"source":"auto","createdAt":10,"expiresAt":500,"remainingSecs":90},
                  {"id":8,"layer":"manual","cidr":"2001:db8::/64","action":"throttle","createdAt":9},
                  {"id":7,"layer":"future","cidr":"9.9.9.9","action":"zap","createdAt":8,"evidence":null}]}""",
            ),
        )
        assertEquals(3L, page.total)
        val auto = page.items[0]
        assertEquals(BtGuardLayer.Auto, auto.layerKind)
        assertEquals(BtGuardAction.Ban, auto.actionKind)
        assertEquals(500L, auto.expiresAt)
        assertEquals(90L, auto.remainingSecs)
        assertEquals(3700L, auto.evidence.longOrNull("ratioMilli"))
        val permanent = page.items[1]
        assertNull(permanent.expiresAt)
        assertNull(permanent.remainingSecs)
        assertEquals("", permanent.rule)
        val unknown = page.items[2]
        assertNull(unknown.layerKind)
        assertNull(unknown.actionKind)
        assertNull(unknown.evidence)
    }

    @Test
    fun decodesEventsAndExport() {
        val events = BtGuardEventsPage.fromJson(
            Json.parse("""{"total":1,"items":[{"id":1,"at":5,"kind":"enforced","ipGroup":"1.2.3.4","rule":"rewind","mode":"enforce","action":"ban","detail":"{\"strikes\":2}"}]}"""),
        )
        val event = events.items.single()
        assertEquals("enforced", event.kind)
        assertEquals("", event.infoHash)
        assertEquals(2L, BtGuardEvidence.factsFromText(event.detail).single().number)
        val export = BtGuardExport.fromJson(Json.parse("""{"text":"1.2.3.4 # auto","lines":1}"""))
        assertEquals(1L, export.lines)
    }

    @Test
    fun requestParamsMatchWire() {
        assertEquals(
            Json.parse("""{"layer":"cloud","rule":"cloud","query":"1.2","offset":50,"limit":50}"""),
            BtGuardEntriesQuery(BtGuardLayer.Cloud, "cloud", " 1.2 ", 50).toJson(),
        )
        assertEquals(
            Json.parse("""{"offset":0,"limit":50}"""),
            BtGuardEntriesQuery(query = "   ", rule = "").toJson(),
        )
        assertEquals(
            Json.parse("""{"rule":"lag","kind":"probe","offset":0,"limit":500}"""),
            BtGuardEventsQuery("lag", "probe", limit = 9999).toJson(),
        )
        assertEquals(
            Json.parse("""{"cidr":"1.2.3.4","action":"ban","durationSecs":604800,"reason":"x"}"""),
            BtGuardParams.addParams(" 1.2.3.4 ", BtGuardAction.Ban, BtGuardDuration.Week, " x "),
        )
        // 永久：不发送 durationSecs
        assertEquals(
            Json.parse("""{"cidr":"1.2.3.0/24","action":"allow","reason":""}"""),
            BtGuardParams.allowParams("1.2.3.0/24"),
        )
        assertEquals(Json.parse("""{"ids":[3,4]}"""), BtGuardParams.removeParams(listOf(3L, 4L)))
        assertEquals(Json.parse("{}"), BtGuardParams.exportParams(null))
        assertEquals(Json.parse("""{"layer":"file"}"""), BtGuardParams.exportParams(BtGuardLayer.File))
    }

    @Test
    fun durationOptionsCoverOneHourToPermanent() {
        assertEquals(
            listOf(3_600L, 86_400L, 604_800L, null),
            BtGuardDuration.entries.map { it.seconds },
        )
    }

    @Test
    fun remainingBucketsRoundUp() {
        assertEquals(BtGuardRemaining.Expired, BtGuardRemaining.of(0))
        assertEquals(BtGuardRemaining.Expired, BtGuardRemaining.of(-5))
        assertEquals(BtGuardRemaining.Minutes(1), BtGuardRemaining.of(1))
        assertEquals(BtGuardRemaining.Minutes(1), BtGuardRemaining.of(60))
        assertEquals(BtGuardRemaining.Minutes(2), BtGuardRemaining.of(61))
        assertEquals(BtGuardRemaining.Minutes(60), BtGuardRemaining.of(3_599))
        assertEquals(BtGuardRemaining.Hours(1), BtGuardRemaining.of(3_600))
        assertEquals(BtGuardRemaining.Hours(24), BtGuardRemaining.of(86_399))
        assertEquals(BtGuardRemaining.Days(1), BtGuardRemaining.of(86_400))
        assertEquals(BtGuardRemaining.Days(8), BtGuardRemaining.of(7 * 86_400 + 1))
    }

    @Test
    fun remainingPrefersServerFieldThenExpiresAtThenPermanent() {
        fun entry(expires: Long?, remaining: Long?) =
            BtGuardEntry(1, "manual", "1.2.3.4", "ban", expiresAt = expires, remainingSecs = remaining)
        assertEquals(BtGuardRemaining.Permanent, BtGuardRemaining.of(entry(null, null), 1_000))
        assertEquals(BtGuardRemaining.Hours(1), BtGuardRemaining.of(entry(9_999_999, 3_600), 1_000))
        assertEquals(BtGuardRemaining.Minutes(5), BtGuardRemaining.of(entry(1_300, null), 1_000))
        assertEquals(BtGuardRemaining.Expired, BtGuardRemaining.of(entry(900, null), 1_000))
    }

    @Test
    fun cloudEntriesAreNotRemovableButAllowable() {
        val cloud = BtGuardEntry(1, "cloud", "1.1.1.1", "ban")
        val manual = BtGuardEntry(2, "manual", "2.2.2.2", "ban")
        val allow = BtGuardEntry(3, "allow", "3.3.3.3", "allow")
        val unknown = BtGuardEntry(4, "wat", "4.4.4.4", "ban")
        assertFalse(BtGuardEntries.canRemove(cloud))
        assertTrue(BtGuardEntries.canRemove(manual))
        assertTrue(BtGuardEntries.canRemove(allow))
        assertFalse(BtGuardEntries.canRemove(unknown))
        assertTrue(BtGuardEntries.canAllow(cloud))
        assertFalse(BtGuardEntries.canAllow(allow))
        assertEquals(listOf(2L, 3L), BtGuardEntries.removableIds(listOf(cloud, manual, allow, unknown)))
    }

    @Test
    fun pagingMergesWithoutDuplicates() {
        fun e(id: Long) = BtGuardEntry(id, "auto", "1.1.1.$id", "ban")
        val merged = BtGuardEntries.merge(listOf(e(5), e(4)), listOf(e(4), e(3)))
        assertEquals(listOf(5L, 4L, 3L), merged.map { it.id })
        assertTrue(BtGuardEntries.hasMore(3, 10))
        assertFalse(BtGuardEntries.hasMore(10, 10))
        assertEquals(listOf(5L), BtGuardEntries.without(merged, setOf(4, 3)).map { it.id })
        assertEquals(3, BtGuardEntriesQuery().nextPage(3).offset)
    }

    @Test
    fun layerChipsStartWithAllAndCarryCounts() {
        val chips = BtGuardEntries.layerChips(BtGuardCounts(allow = 1, manual = 2, auto = 3, cloud = 4, file = 5))
        assertEquals(listOf(null) + BtGuardLayer.entries, chips.map { it.first })
        assertEquals(listOf(15L, 1L, 2L, 3L, 4L, 5L), chips.map { it.second })
    }

    @Test
    fun evidenceFactsAreOrderedAndSkipZeroValues() {
        val facts = BtGuardEvidence.facts(
            Json.parse(
                """{"rule":"excessive","detail":"ratio 3.7","torrentSize":1000,"deliveredBytes":0,"dupBytes":0,
                    "ratioMilli":3700,"rerequests":0,"rewindPermille":125,"strikes":null,"category":"community"}""",
            ),
        )
        assertEquals(
            listOf(
                BtGuardEvidenceField.Detail, BtGuardEvidenceField.TorrentSize, BtGuardEvidenceField.DeliveredBytes,
                BtGuardEvidenceField.Ratio, BtGuardEvidenceField.Rewind, BtGuardEvidenceField.Category,
            ),
            facts.map { it.field },
        )
        assertEquals(0L, facts.first { it.field == BtGuardEvidenceField.DeliveredBytes }.number)
        assertTrue(BtGuardEvidence.facts(null).isEmpty())
        assertTrue(BtGuardEvidence.facts(JsonValue.Str("x")).isEmpty())
        assertTrue(BtGuardEvidence.factsFromText("not json").isEmpty())
        assertEquals("3.7", BtGuardEvidence.ratioText(3_700))
        assertEquals("12.5", BtGuardEvidence.permilleText(125))
    }

    @Test
    fun cloudPhasePrioritisesDisabledThenWaitingThenStale() {
        val fresh = BtGuardCloudStatus(sequence = 3)
        assertEquals(BtGuardCloudPhase.Disabled, BtGuardCloud.phase(false, fresh))
        assertEquals(BtGuardCloudPhase.Waiting, BtGuardCloud.phase(true, BtGuardCloudStatus()))
        assertEquals(BtGuardCloudPhase.Stale, BtGuardCloud.phase(true, fresh.copy(stale = true)))
        assertEquals(BtGuardCloudPhase.Active, BtGuardCloud.phase(true, fresh))
    }

    @Test
    fun reportToggleRequiresLoginThenConsent() {
        assertEquals(BtGuardReport.Toggle.Allowed, BtGuardReport.toggle(turnOn = false, loggedIn = false))
        assertEquals(BtGuardReport.Toggle.NeedsLogin, BtGuardReport.toggle(turnOn = true, loggedIn = false))
        assertEquals(BtGuardReport.Toggle.NeedsConsent, BtGuardReport.toggle(turnOn = true, loggedIn = true))
        assertTrue(BtGuardReport.dormant(enabled = true, loggedIn = false))
        assertFalse(BtGuardReport.dormant(enabled = false, loggedIn = false))
        assertFalse(BtGuardReport.dormant(enabled = true, loggedIn = true))
    }

    @Test
    fun cidrValidation() {
        for (ok in listOf(
            "1.2.3.4", "1.2.3.0/24", "0.0.0.0/0", "255.255.255.255/32", " 8.8.8.8 ",
            "2001:db8::", "2001:db8::/64", "::", "::1", "fe80::1/128", "2001:DB8:0:0:0:0:0:1",
            "::ffff:1.2.3.4", "64:ff9b::1.2.3.4/96", "1:2:3:4:5:6:7::",
        )) {
            assertTrue("expected valid: $ok", BtGuardCidr.isValid(ok))
        }
        for (bad in listOf(
            "1.2.3", "1.2.3.4.5", "256.1.1.1", "1.2.3.04", "1.2.3.4/33", "1.2.3.4/", "1.2.3.4/-1", "1.2.3.4/a",
            "2001:db8::/129", "2001:db8:::1", "1::2::3", "12345::1", "1:2:3:4:5:6:7:8:9", "1:2:3:4:5:6:7", ":1::", "gggg::1",
            "example.com", "1.2.3.4 5.6.7.8", "::1.2.3",
        )) {
            assertFalse("expected invalid: $bad", BtGuardCidr.isValid(bad))
        }
        assertEquals(BtGuardCidr.Result.Empty, BtGuardCidr.parse("   "))
        assertEquals(BtGuardCidr.Result.Invalid, BtGuardCidr.parse("nope"))
        assertEquals(BtGuardCidr.Result.Ok("2001:db8::/64", v6 = true, prefix = 64), BtGuardCidr.parse(" 2001:DB8::/64 "))
        assertEquals(BtGuardCidr.Result.Ok("1.2.3.4", v6 = false, prefix = null), BtGuardCidr.parse("1.2.3.4"))
    }

    @Test
    fun ruleConfigKeysAndModes() {
        assertEquals("bt_guard_rule_lag", BtGuardRules.configKey("lag"))
        assertEquals("bt_guard_cloud_mode", BtGuardRules.configKey("cloud"))
        assertNull(BtGuardRules.configKey("other"))
        assertEquals(5, BtGuardRules.detectionIds.size)
        assertTrue(BtGuardRules.detectionIds.all { SettingsCatalog.field(BtGuardRules.configKey(it)!!) != null })
        assertEquals("observe", BtGuardRules.normalizeMode("bogus"))
        assertEquals("enforce", BtGuardRules.normalizeMode(" enforce "))
        assertEquals(9, BtGuardRules.eventKinds.size)
        assertEquals(listOf("manual", "file"), BtGuardRules.entryRuleIds - BtGuardRules.ids.toSet())
        assertEquals(BtGuardRules.ids + listOf("manual", "file"), BtGuardRules.entryRuleIds)
    }

    @Test
    fun guardTabVisibilityFollowsMasterSwitches() {
        assertEquals(listOf(BtSettingsTab.General), BtSettingsRow.visibleTabs(SettingsForm(host = mapOf("bt_enabled" to "false"))))
        assertTrue(BtSettingsTab.Guard in BtSettingsRow.visibleTabs(form()))

        // 防吸血默认开：全部行可见，云名单模式需云开关
        val defaults = BtSettingsRow.visible(BtSettingsTab.Guard, form())
        assertEquals(BtSettingsRow.entries.count { it.tab == BtSettingsTab.Guard }, defaults.size)

        val off = BtSettingsRow.visible(BtSettingsTab.Guard, form("bt_guard_enabled" to "false"))
        assertEquals(listOf(BtSettingsRow.GuardEnabled), off)

        val noCloud = BtSettingsRow.visible(BtSettingsTab.Guard, form("bt_guard_cloud_enabled" to "false"))
        assertFalse(BtSettingsRow.GuardCloudMode in noCloud)
        assertTrue(BtSettingsRow.GuardCloudEnabled in noCloud)
        assertTrue(BtSettingsRow.GuardReportEnabled in noCloud)

        assertEquals(BtSettingsTab.Guard, BtSettingsRow.tabForRowId("bt.guardReportEnabled"))
        // 每个行的 configKey 都在设置目录里
        assertTrue(BtSettingsRow.entries.all { SettingsCatalog.field(it.configKey) != null })
    }

    @Test
    fun clipKeepsSurrogatePairsAndAddsEllipsis() {
        assertEquals("abc", BtGuardText.clip("abc", 5))
        assertEquals("ab…", BtGuardText.clip("abcdef", 2))
        // 😀 = 两个 UTF-16 单元；在代理对中间截断时整体丢弃
        assertEquals("a…", BtGuardText.clip("a😀b", 2))
        assertEquals("a😀…", BtGuardText.clip("a😀bc", 3))
        assertEquals("ab", BtGuardText.clip("ab", 0))
        assertEquals("日本…", BtGuardText.clip("日本語の理由", 2))
    }

    @Test
    fun subtitleSkipsBlankPartsAndClipsReason() {
        assertEquals("Manual · Ban", BtGuardText.subtitle(listOf("Manual", "Ban", ""), "  "))
        assertEquals("Auto · excessive · aaaa…", BtGuardText.subtitle(listOf("Auto", "", "excessive"), "aaaaaaaa", reasonMax = 4))
    }
}
