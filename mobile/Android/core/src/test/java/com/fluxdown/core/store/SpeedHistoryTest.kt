package com.fluxdown.core.store

import com.fluxdown.core.format.Format
import com.fluxdown.core.format.FileSizeUnits
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SpeedHistoryTest {
    @Test
    fun gapsAreFilledWithPreviousValueInChronologicalOrder() {
        var h = SpeedHistory.Empty.record(10_000, 100, 1)
        h = h.record(13_500, 400, 4) // 3 秒后：11、12 两桶以 100 补齐
        assertEquals(4, h.size)
        assertEquals(listOf(100L, 100L, 100L, 400L), (0 until h.size).map(h::downAt))
        assertEquals(400L, h.latestDown)
    }

    @Test
    fun sameSecondOverwritesAndRingKeepsLastSixtySeconds() {
        var h = SpeedHistory.Empty
        for (s in 0 until 75) h = h.record(s * 1000L, s.toLong(), 0)
        h = h.record(74_900, 999, 0)
        assertEquals(SpeedHistory.CAPACITY, h.size)
        assertEquals(15L, h.downAt(0))
        assertEquals(999L, h.downAt(h.size - 1))
    }

    @Test
    fun bytesFollowPrototypePrecisionRules() {
        assertEquals("0 B", Format.bytes(0, FileSizeUnits.Legacy).toString())
        assertEquals("512 B", Format.bytes(512, FileSizeUnits.Legacy).toString())
        assertEquals("1.50 KB", Format.bytes(1536, FileSizeUnits.Legacy).toString())
        assertEquals("18.6 MB", Format.bytes((18.6 * 1024 * 1024).toLong(), FileSizeUnits.Legacy).toString())
        assertEquals("186 GB", Format.bytes(186L * 1024 * 1024 * 1024, FileSizeUnits.Legacy).toString())
        assertNull(Format.speed(0, FileSizeUnits.Legacy))
    }

    @Test
    fun fileSizeUnitsChangeLabelsWithoutChangingBinaryScalingOrPrecision() {
        assertEquals("1.50 KiB", Format.bytes(1536, FileSizeUnits.Iec).toString())
        assertEquals("18.6 MiB", Format.bytes((18.6 * 1024 * 1024).toLong(), FileSizeUnits.Iec).toString())
        assertEquals("1.50 KiB/s", Format.speed(1536, FileSizeUnits.Iec)?.toString())
    }
    @Test
    fun etaIsUnknownWhenStalledUnknownSizeOrBeyondOneDay() {
        assertNull(Format.etaSeconds(10, 0, 100))
        assertNull(Format.etaSeconds(10, 100, 0))
        assertNull(Format.etaSeconds(0, 100_000_000, 1))
        assertEquals(9L, Format.etaSeconds(10, 100, 10))
    }
}
