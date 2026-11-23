package com.fluxdown.core.format

import java.util.Locale
import kotlin.math.ceil
import kotlin.math.floor

/** 数值与单位分离（数字与单位拆成两个 Text，避免换行把单位孤立到下一行）。 */
data class Measure(val value: String, val unit: String) {
    override fun toString(): String = "$value $unit"
}

enum class FileSizeUnits(
    val wire: String,
    private val labels: Array<String>,
) {
    Legacy("legacy", arrayOf("B", "KB", "MB", "GB", "TB")),
    Iec("iec", arrayOf("B", "KiB", "MiB", "GiB", "TiB"));

    private val rateLabels = Array(labels.size) { "${labels[it]}/s" }
    internal val maxPower: Int get() = labels.lastIndex

    fun byteLabel(power: Int): String = labels[power]
    fun rateLabel(power: Int): String = rateLabels[power]

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

    /** 1024 进制；≥100 不保留小数、≥10 保留 1 位、否则 2 位；B 不带小数；≤0 → "0 B"。 */
    fun bytes(n: Long): Measure = bytes(n, fileSizeUnits)

    fun bytes(n: Long, style: FileSizeUnits): Measure = measure(n, style, rate = false)

    private fun measure(n: Long, style: FileSizeUnits, rate: Boolean): Measure {
        if (n <= 0) return Measure("0", "B")
        var v = n.toDouble()
        var i = 0
        while (v >= 1024 && i < style.maxPower) {
            v /= 1024
            i++
        }
        val label = if (rate) style.rateLabel(i) else style.byteLabel(i)
        if (i == 0) return Measure(n.toString(), label)
        val digits = when {
            v >= 100 -> 0
            v >= 10 -> 1
            else -> 2
        }
        return Measure(String.format(Locale.ROOT, "%.${digits}f", v), label)
    }

    /** 速度 >0 → "{bytes}/s"；否则 null（UI 显示「—」）。 */
    fun speed(bps: Long): Measure? = speed(bps, fileSizeUnits)

    fun speed(bps: Long, style: FileSizeUnits): Measure? =
        if (bps > 0) measure(bps, style, rate = true) else null

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
