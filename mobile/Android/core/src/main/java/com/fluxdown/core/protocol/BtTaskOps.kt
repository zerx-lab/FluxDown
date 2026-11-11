package com.fluxdown.core.protocol

/*
 * BT 任务级操作的纯逻辑：文件表（`daemon.task.btFiles` / `daemon.task.setFilePriorities`）与任务限速
 * （`daemon.task.setBtLimits`）。wire 形状以 `native/protocol/src/daemon.rs` 为准（`BtFilesDto` /
 * `BtTaskFileDto` / `SetTaskFilePrioritiesParams` / `SetTaskBtLimitsParams`）。无 UI / 无主机依赖。
 */

// ----------------------------- 文件优先级 -----------------------------

/** 文件优先级四档（`BT_FILE_PRIORITY_*`）。[wire] 是下发值。 */
enum class BtFilePriority(val wire: Int) {
    Skip(0),
    Normal(1),
    High(6),
    Maximum(7),
    ;

    companion object {
        /** wire → 档位：0 不下载；1..=5（引擎的中间级）按普通；6 高；≥7 最高；负数按普通。 */
        fun of(wire: Int): BtFilePriority = when {
            wire == 0 -> Skip
            wire >= 7 -> Maximum
            wire == 6 -> High
            else -> Normal
        }
    }
}

/** [BtFiles] 的一行。[path] 为任务目录内的相对路径（`/` 分隔）。 */
data class BtTaskFile(
    val index: Int,
    val path: String,
    val size: Long,
    val downloaded: Long,
    val priority: BtFilePriority,
) {
    /** 末段文件名；路径以 `/` 结尾或为空时回退整串。 */
    val name: String get() = path.substringAfterLast('/').ifEmpty { path }

    /** 所在目录（不含末段）；顶层文件为空串。 */
    val directory: String get() = if ('/' in path) path.substringBeforeLast('/') else ""

    /** 0..1；空文件按已完成（没有要下载的字节）。 */
    val progress: Float
        get() = if (size <= 0) 1f else (downloaded.toDouble() / size).toFloat().coerceIn(0f, 1f)
}

/** `daemon.task.btFiles` 结果。[editable] = 未完成且已取得元数据（可改优先级）。 */
data class BtFiles(val editable: Boolean, val files: List<BtTaskFile>) {
    companion object {
        /** 结果不是对象时返回 null；缺失字段按 serde 默认（旧 / 新 daemon 都能读）。 */
        fun fromJson(v: JsonValue?): BtFiles? {
            if (v !is JsonValue.Obj) return null
            val files = v.list("files").mapNotNull { f ->
                if (f !is JsonValue.Obj) return@mapNotNull null
                BtTaskFile(
                    index = f.int("index", -1).takeIf { it >= 0 } ?: return@mapNotNull null,
                    path = f.str("path"),
                    size = f.long("size").coerceAtLeast(0),
                    downloaded = f.long("downloaded").coerceAtLeast(0),
                    priority = BtFilePriority.of(f.int("priority", BtFilePriority.Normal.wire)),
                )
            }
            return BtFiles(editable = v.bool("editable"), files = files)
        }
    }
}

/** 本地守卫的结论（daemon 同样会以 `invalidArgument` 拒绝，本地先拦住给出明确文案）。 */
sealed interface BtFilePriorityChange {
    /** 已全部是目标档位：不发 RPC。 */
    data object NoChange : BtFilePriorityChange

    /** 会让所有文件都不下载：提示 `btFilesKeepOne`。 */
    data object KeepOne : BtFilePriorityChange

    data class Send(val params: JsonValue.Obj) : BtFilePriorityChange
}

/**
 * 把 [indices] 这些文件设为 [priority] 时要发的 `daemon.task.setFilePriorities` 参数
 * （部分更新，只带档位不同的文件）。
 */
fun btFilePriorityChange(
    taskId: String,
    files: List<BtTaskFile>,
    indices: Set<Int>,
    priority: BtFilePriority,
): BtFilePriorityChange {
    val changed = files.filter { it.index in indices && it.priority != priority }
    if (changed.isEmpty()) return BtFilePriorityChange.NoChange
    if (priority == BtFilePriority.Skip &&
        files.all { it.index in indices || it.priority == BtFilePriority.Skip }
    ) {
        return BtFilePriorityChange.KeepOne
    }
    return BtFilePriorityChange.Send(
        jsonObject(
            "taskId" to taskId,
            "priorities" to changed.map { mapOf("index" to it.index, "priority" to priority.wire) },
        ),
    )
}

// ----------------------------- 任务限速 -----------------------------

/** 任务限速对话框：数字框单位 KiB/s，`0` = 不限；wire 为字节/秒（`downloadLimitBps` / `uploadLimitBps`）。 */
object BtTaskLimits {
    /** 输入上限（KiB/s）：乘 1024 后仍在 i64 内。 */
    const val MAX_KIB: Long = Long.MAX_VALUE / 1024

    /** 字节/秒 → 输入框文本。不足 1 KiB 的非零值向上取整，避免显示成「不限」。 */
    fun kibText(bps: Long): String = if (bps <= 0) "0" else ((bps + 1023) / 1024).toString()

    /** 输入框文本 → KiB/s；空白 = 0（不限）；非数字 / 负数 / 超限 → null。 */
    fun parseKib(text: String): Long? {
        val t = text.trim()
        if (t.isEmpty()) return 0
        if (!t.all { it in '0'..'9' }) return null
        return t.toLongOrNull()?.takeIf { it <= MAX_KIB }
    }

    sealed interface Result {
        data object Invalid : Result
        data object NoChange : Result
        data class Send(val params: JsonValue.Obj) : Result
    }

    /**
     * `daemon.task.setBtLimits` 参数：只发和初值（[currentDownloadBps] / [currentUploadBps] 经 [kibText]
     * 显示的值）不同的字段，未动的字段保持不变（不会因取整把 1500 B/s 改写成 2048 B/s）。
     */
    fun change(
        taskId: String,
        currentDownloadBps: Long,
        currentUploadBps: Long,
        downloadText: String,
        uploadText: String,
    ): Result {
        val down = parseKib(downloadText) ?: return Result.Invalid
        val up = parseKib(uploadText) ?: return Result.Invalid
        val downChanged = down != parseKib(kibText(currentDownloadBps))
        val upChanged = up != parseKib(kibText(currentUploadBps))
        if (!downChanged && !upChanged) return Result.NoChange
        return Result.Send(
            jsonObjectOmitNulls(
                "taskId" to taskId,
                "downloadLimitBps" to if (downChanged) down * 1024 else null,
                "uploadLimitBps" to if (upChanged) up * 1024 else null,
            ),
        )
    }
}
