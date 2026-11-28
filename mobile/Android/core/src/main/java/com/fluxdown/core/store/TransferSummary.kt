package com.fluxdown.core.store

import com.fluxdown.core.format.Format
import com.fluxdown.core.model.HlsFragmentProgress
import com.fluxdown.core.model.TaskStatus

/** 一个正在准备或下载的任务（进度通知的一行）。 */
data class Transfer(
    val taskId: String,
    val fileName: String,
    val downloadedBytes: Long,
    /** 0 = 未知大小。 */
    val totalBytes: Long,
    val speedBps: Long,
    val status: TaskStatus,
    val hlsFragments: HlsFragmentProgress? = null,
) {
    /** 点播优先按媒体分片计数；无分片计数时按字节。 */
    val fraction: Float? get() = hlsFragments?.fraction ?: fractionOf(downloadedBytes, totalBytes)

    val etaSeconds: Long? get() = Format.etaSeconds(downloadedBytes, totalBytes, speedBps)
}

/**
 * 多个下载的聚合进度（进度通知的标题行与进度条）。
 *
 * - 全部按分片时汇总分片数；分片与字节口径混合时不显示聚合百分比；
 * - 全部按字节时保留已知大小任务的聚合进度，全部未知才为不确定态；
 * - 字节 ETA 只用于纯字节口径且全部大小已知的任务，速度 = 全部任务之和。
 */
data class TransferSummary(
    val count: Int,
    val fraction: Float?,
    val speedBps: Long,
    val etaSeconds: Long?,
) {
    companion object {
        val Empty = TransferSummary(count = 0, fraction = null, speedBps = 0, etaSeconds = null)

        fun of(transfers: List<Transfer>): TransferSummary {
            var down = 0L
            var total = 0L
            var speed = 0L
            var allKnown = true
            var fragmentTasks = 0
            var fragmentsWritten = 0.0
            var fragmentsTotal = 0.0
            for (t in transfers) {
                speed += t.speedBps
                val fragments = t.hlsFragments
                if (fragments?.fraction != null) {
                    fragmentTasks++
                    fragmentsWritten += fragments.written
                    fragmentsTotal += fragments.total
                    continue
                }
                if (t.totalBytes > 0) {
                    down += t.downloadedBytes.coerceIn(0, t.totalBytes)
                    total += t.totalBytes
                } else {
                    allKnown = false
                }
            }
            val fraction = when {
                fragmentTasks == 0 -> fractionOf(down, total)
                fragmentTasks == transfers.size -> (fragmentsWritten / fragmentsTotal).toFloat()
                else -> null
            }
            val eta = if (fragmentTasks == 0 && allKnown) Format.etaSeconds(down, total, speed) else null
            return TransferSummary(transfers.size, fraction, speed, eta)
        }
    }
}

/** 主机上的活跃任务（下载中 / 准备中，按任务列表顺序），速度取自 TaskProgress 实时量；未连上时为空。 */
fun HostState.transfers(): List<Transfer> {
    if (connection != Connection.Live) return emptyList()
    return tasks.mapNotNull { t ->
        if (!t.status.isActive) return@mapNotNull null
        val fragments = runtime[t.taskId]?.hlsFragments?.takeIf { it.fraction != null }
        Transfer(t.taskId, t.fileName, t.downloadedBytes, t.totalBytes, speeds[t.taskId]?.down ?: 0L, t.status, fragments)
    }
}

private fun fractionOf(downloaded: Long, total: Long): Float? =
    if (total > 0) (downloaded.toDouble() / total).coerceIn(0.0, 1.0).toFloat() else null
