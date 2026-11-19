package com.fluxdown.core.format

import java.util.Locale
import kotlin.math.ceil
import kotlin.math.floor

/** 数值与单位分离（数字与单位拆成两个 Text，避免换行把单位孤立到下一行）。 */
data class Measure(val value: String, val unit: String) {
    override fun toString(): String = "$value $unit"
}

enum class FileSizeUnits(val wire: String) {
    Legacy("legacy"), Iec("iec");

    companion object {
        fun of(value: String?): FileSizeUnits = entries.firstOrNull { it.wire == value } ?: Legacy
    }
}

object Format {
    @Volatile
    var fileSizeUnits: FileSizeUnits = FileSizeUnits.Legacy
        private set

    fun setFileSizeUnits(units: FileSizeUnits) {
        fileSizeUnits = units
    }

    private val LEGACY_UNITS = arrayOf("B", "KB", "MB", "GB", "TB")
    private val IEC_UNITS = arrayOf("B", "KiB", "MiB", "GiB", "TiB")

    /** 1024 进制；≥100 不保留小数、≥10 保留 1 位、否则 2 位；B 不带小数；≤0 → "0 B"。 */
    fun bytes(n: Long): Measure = bytes(n, fileSizeUnits)

    fun bytes(n: Long, style: FileSizeUnits): Measure {
        if (n <= 0) return Measure("0", "B")
        var v = n.toDouble()
        var i = 0
        val units = if (style == FileSizeUnits.Iec) IEC_UNITS else LEGACY_UNITS
        while (v >= 1024 && i < units.lastIndex) {
            v /= 1024
            i++
        }
        if (i == 0) return Measure(n.toString(), "B")
        val digits = when {
            v >= 100 -> 0
            v >= 10 -> 1
            else -> 2
        }
        return Measure(String.format(Locale.ROOT, "%.${digits}f", v), units[i])
    }

    /** 速度 >0 → "{bytes}/s"；否则 null（UI 显示「—」）。 */
    fun speed(bps: Long): Measure? = speed(bps, fileSizeUnits)

    fun speed(bps: Long, style: FileSizeUnits): Measure? =
        if (bps > 0) bytes(bps, style).let { Measure(it.value, it.unit + "/s") } else null

    /** 速度仪表：空闲显示 0 B/s。 */
    fun speedOrZero(bps: Long): Measure = speed(bps) ?: Measure("0", "B/s")

    /** 百分比 floor 到 0.1。 */
    fun percent(fraction: Float): String = String.format(Locale.ROOT, "%.1f%%", floor(fraction * 1000.0) / 10.0)

    /**
     * ETA 秒数：速度 ≤0、总大小未知、已完成或超过 24h → null（UI 显示「—」）。
     */
    fun etaSeconds(downloaded: Long, total: Long, speed: Long): Long? {
        if (speed <= 0 || total <= 0 || downloaded >= total) return null
        val s = ceil((total - downloaded).toDouble() / speed).toLong()
        return if (s in 1..86_400) s else null
    }
}
