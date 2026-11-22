package com.fluxdown.core.protocol

import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import java.time.OffsetDateTime
import java.time.format.DateTimeParseException

// 跨设备任务（`agent.remote.*`，经 FluxCloud）。镜像 `native/protocol/src/agent.rs::RemoteTaskDto` 与
// iOS `FluxDomain/Protocol/RemoteTasks.swift`；控制矩阵镜像 Web `batchPlan.ts::remoteCan` ↔ GPUI
// `dispatch.rs::remote_action_applies`。

/** `agent.remote.command` 的动作。 */
enum class RemoteCommandAction(val wire: String) {
    Pause("pause"),
    Resume("resume"),

    /** 云端直接置 `canceled`（不依赖目标在线），目标设备删除其本地任务、保留已下载文件。 */
    Cancel("cancel"),

    /** 云端直接删除记录；目标在线时按 `deleteFiles` 决定是否同时删文件。 */
    Delete("delete"),
}

/** `RemoteTaskStatus`：云端新增、本端不认识的状态落入 [Unknown]（不参与控制）。 */
sealed interface RemoteTaskStatus {
    val wire: String

    data object Pending : RemoteTaskStatus {
        override val wire = "pending"
    }

    data object Accepted : RemoteTaskStatus {
        override val wire = "accepted"
    }

    data object Downloading : RemoteTaskStatus {
        override val wire = "downloading"
    }

    data object Paused : RemoteTaskStatus {
        override val wire = "paused"
    }

    data object Completed : RemoteTaskStatus {
        override val wire = "completed"
    }

    data object Failed : RemoteTaskStatus {
        override val wire = "failed"
    }

    data object Canceled : RemoteTaskStatus {
        override val wire = "canceled"
    }

    data class Unknown(override val wire: String) : RemoteTaskStatus

    /** 任务已结束（不再有速度 / 进度变化）。 */
    val isTerminal: Boolean get() = this == Completed || this == Failed || this == Canceled

    /**
     * 云端命令是否适用于该状态：未知一律不可控制（含删除）；暂停 / 取消只对进行中，继续只对已暂停（取消也适用），
     * 删除对任何已知状态成立。
     */
    fun allows(action: RemoteCommandAction): Boolean = when (this) {
        is Unknown -> false
        Pending, Accepted, Downloading ->
            action == RemoteCommandAction.Pause || action == RemoteCommandAction.Cancel || action == RemoteCommandAction.Delete
        Paused ->
            action == RemoteCommandAction.Resume || action == RemoteCommandAction.Cancel || action == RemoteCommandAction.Delete
        Completed, Failed, Canceled -> action == RemoteCommandAction.Delete
    }

    companion object {
        fun fromWire(wire: String): RemoteTaskStatus = when (wire) {
            "pending" -> Pending
            "accepted" -> Accepted
            "downloading" -> Downloading
            "paused" -> Paused
            "completed" -> Completed
            "failed" -> Failed
            "canceled" -> Canceled
            else -> Unknown(wire)
        }

        /** 暂停 / 继续需要目标设备在线才能送达（取消 / 删除在云端直接生效）。 */
        fun needsTargetOnline(action: RemoteCommandAction): Boolean =
            action == RemoteCommandAction.Pause || action == RemoteCommandAction.Resume
    }
}

/** 跨设备任务的 UI 投影（`RemoteTaskDto`）。 */
data class RemoteTaskDto(
    val id: String,
    val fromDevice: String = "",
    /** 目标设备的 `deviceId`。 */
    val toDevice: String = "",
    val url: String = "",
    val saveDir: String? = null,
    /** 云端对未指定文件名的任务回 `null`，按空串处理。 */
    val fileName: String = "",
    val status: RemoteTaskStatus = RemoteTaskStatus.Pending,
    val totalBytes: Long? = null,
    val downloadedBytes: Long = 0,
    val speed: Long = 0,
    /** 0…1。 */
    val progress: Double = 0.0,
    val error: String? = null,
    val createdAt: String = "",
    val updatedAt: String = "",
) {
    /** `createdAt`（ISO-8601，带 / 不带小数秒、`Z` 或偏移）→ Unix 秒；解析失败 0。 */
    val createdAtSeconds: Long
        get() = try {
            OffsetDateTime.parse(createdAt).toEpochSecond()
        } catch (_: DateTimeParseException) {
            0L
        }

    companion object {
        /** `id` 缺失视为非法（null）；其余字段宽松同 serde `#[serde(default)]`。 */
        fun fromJson(v: JsonValue?): RemoteTaskDto? {
            val id = v.strOrNull("id") ?: return null
            return RemoteTaskDto(
                id = id,
                fromDevice = v.str("fromDevice"),
                toDevice = v.str("toDevice"),
                url = v.str("url"),
                saveDir = v.strOrNull("saveDir"),
                fileName = v.str("fileName"),
                status = v.strOrNull("status")?.let(RemoteTaskStatus::fromWire) ?: RemoteTaskStatus.Pending,
                totalBytes = v.longOrNull("totalBytes"),
                downloadedBytes = v.long("downloadedBytes"),
                speed = v.long("speed"),
                progress = v.double("progress"),
                error = v.strOrNull("error"),
                createdAt = v.str("createdAt"),
                updatedAt = v.str("updatedAt"),
            )
        }

        /** `agent.remoteTasks` 分区（`Vec<RemoteTaskDto>`）；缺失 / 非数组为空，非法元素跳过。 */
        fun listFromJson(v: JsonValue?): List<RemoteTaskDto> = v.arrayOrNull.orEmpty().mapNotNull(::fromJson)
    }
}

/** `agent.remote.command` 参数；[commandId] 省略时 agent 生成唯一值。 */
data class RemoteCommandParams(
    val taskId: String,
    val action: RemoteCommandAction,
    val commandId: String? = null,
    /** 仅 [RemoteCommandAction.Delete]：目标设备同时删除已下载文件。 */
    val deleteFiles: Boolean = false,
) {
    fun toJson(): JsonValue = jsonObjectOmitNulls(
        "taskId" to taskId,
        "action" to action.wire,
        "commandId" to commandId,
        "deleteFiles" to deleteFiles,
    )
}

/** 远程任务列表规则。 */
object RemoteTaskRules {
    /** 目标为本机的远程任务在本地已有真实任务，不再作为镜像行显示。 */
    fun visible(tasks: List<RemoteTaskDto>, currentDeviceId: String?): List<RemoteTaskDto> =
        if (currentDeviceId == null) tasks else tasks.filter { it.toDevice != currentDeviceId }

    /** 命令是否应当显示为可点：状态矩阵 + （暂停 / 继续）目标在线。`targetOnline == null` = 在线状态未知，不据此禁用。 */
    fun canIssue(action: RemoteCommandAction, task: RemoteTaskDto, targetOnline: Boolean?): Boolean {
        if (!task.status.allows(action)) return false
        return !(RemoteTaskStatus.needsTargetOnline(action) && targetOnline == false)
    }

    /**
     * 远程命令失败的文案键：`remoteTaskGone`（新主机，已移除该行）与旧主机的裸 `notFound` 都说明云端记录已不存在；
     * 其余走账户错误映射（`targetDeviceOffline` / `taskStateConflict` 等）。
     */
    fun errorKey(e: HostException): String =
        if (e.reason == null && e.code == HostErrorCode.NotFound) "errReasonRemoteTaskGone" else AccountRules.errorKey(e)

    /** 行尾单一主按钮：进行中 → 暂停，已暂停 → 继续，其余无。 */
    fun primaryAction(task: RemoteTaskDto): RemoteCommandAction? = when {
        task.status.allows(RemoteCommandAction.Pause) -> RemoteCommandAction.Pause
        task.status.allows(RemoteCommandAction.Resume) -> RemoteCommandAction.Resume
        else -> null
    }
}

// ───────────────────────────── 批量命令（`agent.remote.commandMany`）─────────────────────────────

/** `agent.remote.commandMany` 单次最多任务数（`REMOTE_COMMAND_MANY_MAX`）；更大的选择由客户端分块。 */
const val REMOTE_COMMAND_MANY_MAX = 1000

/** 逐个 `agent.remote.command` 回退（旧主机）的并发上限。 */
const val REMOTE_FALLBACK_CONCURRENCY = 4

/** `agent.remote.commandMany` 参数。 */
data class RemoteCommandManyParams(
    val taskIds: List<String>,
    val action: RemoteCommandAction,
    /** 仅 [RemoteCommandAction.Delete]：目标设备同时删除已下载文件。 */
    val deleteFiles: Boolean = false,
) {
    fun toJson(): JsonValue = jsonObject(
        "taskIds" to taskIds,
        "action" to action.wire,
        "deleteFiles" to deleteFiles,
    )
}

/** 批量命令中单个任务的失败（`failed[]`；[error] 为标准 `RpcErrorData`）。 */
data class RemoteCommandFailure(
    val taskId: String,
    val code: HostErrorCode,
    val reason: String? = null,
    val retryable: Boolean = false,
) {
    /** 转为与单任务命令失败同形的 [HostException]（文案走 `AccountRules.errorKey`）。 */
    fun toException(): HostException = HostException(code, reason, retryable)

    /** 旧主机的「记录不存在」：`notFound` 且没有 reason（新主机会带 `remoteTaskGone`，且已移除该行）。 */
    val isLegacyNotFound: Boolean get() = code == HostErrorCode.NotFound && reason == null

    companion object {
        fun of(taskId: String, e: HostException) = RemoteCommandFailure(taskId, e.code, e.reason, e.retryable)

        /** `{"taskId": "...", "error": {"code", "retryable", "reason"?}}`；缺 `taskId` 为非法（null），未知 code 落入 Internal。 */
        fun fromJson(v: JsonValue?): RemoteCommandFailure? {
            val id = v.strOrNull("taskId") ?: return null
            val error = v["error"]
            return RemoteCommandFailure(
                taskId = id,
                code = errorCodeFromWire(error.strOrNull("code")),
                reason = error.strOrNull("reason"),
                retryable = error.bool("retryable"),
            )
        }

        private fun errorCodeFromWire(wire: String?): HostErrorCode = when (wire) {
            "protocolIncompatible" -> HostErrorCode.ProtocolIncompatible
            "unauthorized" -> HostErrorCode.Unauthorized
            "invalidArgument" -> HostErrorCode.InvalidArgument
            "notFound" -> HostErrorCode.NotFound
            "conflict" -> HostErrorCode.Conflict
            "unavailable" -> HostErrorCode.Unavailable
            "timeout" -> HostErrorCode.Timeout
            "cancelled" -> HostErrorCode.Cancelled
            "unsupported" -> HostErrorCode.Unsupported
            else -> HostErrorCode.Internal
        }
    }
}

/** `agent.remote.commandMany` 结果：成功 ∪ 失败 = 去重后的输入 id。缺失数组按空处理（同 serde `default`）。 */
data class RemoteCommandManyResult(
    val succeeded: List<String> = emptyList(),
    val failed: List<RemoteCommandFailure> = emptyList(),
) {
    operator fun plus(other: RemoteCommandManyResult) = RemoteCommandManyResult(succeeded + other.succeeded, failed + other.failed)

    companion object {
        fun fromJson(v: JsonValue?): RemoteCommandManyResult = RemoteCommandManyResult(
            succeeded = v.strings("succeeded"),
            failed = v.list("failed").mapNotNull(RemoteCommandFailure::fromJson),
        )
    }
}

/**
 * 批量结果。[legacyNotFound] = 回退路径遇到旧主机的裸 `notFound`（记录已不在云端，但旧主机不会移除该行）：
 * 调用方应再请求一次 `agent.remote.list` 重新同步。
 */
data class RemoteBatchOutcome(
    val succeeded: List<String>,
    val failed: List<RemoteCommandFailure>,
    val legacyNotFound: Boolean = false,
)

/** 批量远程命令的拆分 / 分块 / 回退（纯逻辑；网络调用由调用方以 lambda 注入）。 */
object RemoteBatch {
    /** 去重（保持顺序）后按 [REMOTE_COMMAND_MANY_MAX] 分块。 */
    fun chunks(ids: List<String>, max: Int = REMOTE_COMMAND_MANY_MAX): List<List<String>> =
        ids.distinct().chunked(max.coerceAtLeast(1))

    /** 控制矩阵（状态 + 目标在线）过滤：只保留 [action] 对其适用的任务（按 id 去重，保持顺序）。 */
    fun eligible(
        tasks: List<RemoteTaskDto>,
        action: RemoteCommandAction,
        targetOnline: (RemoteTaskDto) -> Boolean?,
    ): List<RemoteTaskDto> = tasks.filter { RemoteTaskRules.canIssue(action, it, targetOnline(it)) }.distinctBy { it.id }

    /**
     * 执行批量命令。[batchSupported]（主机声明 `agent.remoteBatch`）时每块一次 [many]；块整体失败（抛 [HostException]）
     * 把该块全部记为失败。否则回退为逐个 [single]（最多 [concurrency] 并发）：取消 / 删除遇到旧主机的裸 `notFound`
     * 视为成功（记录本就不在云端，意图已达成）并置 [RemoteBatchOutcome.legacyNotFound]。
     */
    suspend fun run(
        ids: List<String>,
        action: RemoteCommandAction,
        deleteFiles: Boolean,
        batchSupported: Boolean,
        many: suspend (RemoteCommandManyParams) -> RemoteCommandManyResult,
        single: suspend (RemoteCommandParams) -> Unit,
        concurrency: Int = REMOTE_FALLBACK_CONCURRENCY,
    ): RemoteBatchOutcome {
        val unique = ids.distinct()
        if (unique.isEmpty()) return RemoteBatchOutcome(emptyList(), emptyList())
        if (batchSupported) {
            var acc = RemoteCommandManyResult()
            for (chunk in chunks(unique)) {
                acc += try {
                    many(RemoteCommandManyParams(chunk, action, deleteFiles))
                } catch (e: HostException) {
                    RemoteCommandManyResult(failed = chunk.map { RemoteCommandFailure.of(it, e) })
                }
            }
            return RemoteBatchOutcome(acc.succeeded, acc.failed)
        }
        val gate = Semaphore(concurrency.coerceAtLeast(1))
        val results: List<RemoteCommandFailure?> = coroutineScope {
            unique.map { id ->
                async {
                    gate.withPermit {
                        try {
                            single(RemoteCommandParams(id, action, deleteFiles = deleteFiles))
                            null
                        } catch (e: HostException) {
                            RemoteCommandFailure.of(id, e)
                        }
                    }
                }
            }.awaitAll()
        }
        val succeeded = ArrayList<String>(unique.size)
        val failed = ArrayList<RemoteCommandFailure>()
        var legacy = false
        unique.forEachIndexed { i, id ->
            val f = results[i]
            when {
                f == null -> succeeded += id
                f.isLegacyNotFound -> {
                    legacy = true
                    if (action == RemoteCommandAction.Cancel || action == RemoteCommandAction.Delete) succeeded += id else failed += f
                }
                else -> failed += f
            }
        }
        return RemoteBatchOutcome(succeeded, failed, legacy)
    }
}
