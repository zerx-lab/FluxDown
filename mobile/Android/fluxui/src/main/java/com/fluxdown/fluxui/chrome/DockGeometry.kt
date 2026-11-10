package com.fluxdown.fluxui.chrome

import kotlin.math.roundToInt

/** 坞本体宽：容器宽 − 右侧预留（新建球 + 间距），不小于坞高（极窄容器时取容器宽）。 */
internal fun dockWidth(available: Int, reservedEnd: Int, height: Int): Int {
    val floor = height.coerceAtMost(available)
    return (available - reservedEnd).coerceIn(floor, available)
}

/**
 * 标签是否能整体排在图标下方：最高的标签 + 图标 + 间距不超过项高。
 * 任一标签放不下（大字体）时整坞改为仅图标，避免各项高低不一。
 */
internal fun dockLabelsFit(itemHeight: Int, iconSize: Int, gap: Int, labelHeights: IntArray): Boolean =
    labelHeights.all { iconSize + gap + it <= itemHeight }

/**
 * 选择坞动作区槽宽：[natural] 为各按钮自然宽，结果写入 [out]，返回总宽。
 * - 最宽的按钮也放得进等分槽 → 等分 [available]（累计边界取整，不留尾像素）；
 * - 总自然宽放得下、但有按钮超出等分槽 → 自然宽 + 均分剩余；
 * - 放不下 → 自然宽（外层横向滚动）。
 */
internal fun selectionSlotWidths(available: Int, natural: IntArray, out: IntArray): Int {
    val n = natural.size
    if (n == 0) return 0
    var sum = 0L
    var widest = 0
    for (w in natural) {
        sum += w
        if (w > widest) widest = w
    }
    val avail = available.coerceAtLeast(0)
    if (sum >= avail) {
        natural.copyInto(out)
        return sum.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
    }
    if (widest.toLong() * n <= avail) {
        var cursor = 0
        for (i in 0 until n) {
            val end = ((i + 1).toLong() * avail / n).toInt()
            out[i] = end - cursor
            cursor = end
        }
        return avail
    }
    val extra = (avail - sum).toInt()
    var given = 0
    for (i in 0 until n) {
        val add = if (i == n - 1) extra - given else extra / n
        out[i] = natural[i] + add
        given += add
    }
    return avail
}

/** 各项等分内宽；指示器按选中权重在实际项边界间插值。所有坐标相对坞内边距。 */
internal class DockGeometry(itemCount: Int) {
    val starts = IntArray(itemCount)
    val widths = IntArray(itemCount)
    var indicatorStart = 0
        private set
    var indicatorWidth = 0
        private set
    private var innerWidth = 0

    fun left(index: Int, rtl: Boolean): Int =
        if (rtl) innerWidth - starts[index] - widths[index] else starts[index]

    fun indicatorLeft(rtl: Boolean): Int =
        if (rtl) innerWidth - indicatorStart - indicatorWidth else indicatorStart

    fun update(innerWidth: Int, selection: FloatArray, selected: Int) {
        this.innerWidth = innerWidth
        val count = widths.size
        var cursor = 0
        for (i in 0 until count) {
            // 取整累计边界而非逐项宽度：小数密度下也不会留尾像素。
            val end = if (i == count - 1) innerWidth else ((i + 1).toDouble() * innerWidth / count).roundToInt().coerceIn(cursor, innerWidth)
            starts[i] = cursor
            widths[i] = end - cursor
            cursor = end
        }
        var weightSum = 0.0
        for (value in selection) weightSum += value.coerceIn(0f, 1f)
        var indicatorLeft = 0.0
        var indicatorRight = 0.0
        for (i in 0 until count) {
            val weight = if (weightSum > 0.0) selection[i].coerceIn(0f, 1f) / weightSum
                else if (i == selected) 1.0 else 0.0
            indicatorLeft += starts[i] * weight
            indicatorRight += (starts[i] + widths[i]) * weight
        }
        indicatorStart = indicatorLeft.roundToInt().coerceIn(0, innerWidth)
        indicatorWidth = indicatorRight.roundToInt().coerceIn(indicatorStart, innerWidth) - indicatorStart
    }
}
