package com.fluxdown.app

import android.util.Log
import com.fluxdown.app.data.HostRepo
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.model.HostRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.IOException

/** 应用作用域内的主机选择；复用容器的切换锁，与删除和凭据更新串行。 */
internal class HostSelection(
    private val appScope: CoroutineScope,
    private val switchLock: Mutex,
    private val hostRepo: HostRepo,
    private val currentHost: () -> HostRef,
    private val hasSession: () -> Boolean,
    private val open: suspend (HostRef) -> Result<HostSession>,
    private val adopt: (HostRef, HostSession) -> Unit,
) {
    private val startupRestore = appScope.async(start = CoroutineStart.LAZY) { performHostRestore() }
    private var restoreFailureShown = false
    private var restoreSuperseded = false

    fun supersedeHostRestore() {
        restoreSuperseded = true
    }

    fun hostForNewDownloadDraft(): HostRef {
        supersedeHostRestore()
        return currentHost()
    }

    fun prioritizeNotification(hostId: String) {
        if (hostId.isNotEmpty()) supersedeHostRestore()
    }

    /** 已选主机可用时立即阻止本次自动恢复。 */
    fun keepCurrentHost(ref: HostRef): Boolean {
        if (ref.id != currentHost().id || !hasSession()) return false
        supersedeHostRestore()
        return true
    }

    suspend fun rememberConfirmedRemote(ref: HostRef.Remote) = appScope.async {
        switchLock.withLock { persistRemoteChoice(ref.id) }
    }.await()

    /** 界面重建只取消等待；恢复属于进程，并且失败只提示一次。 */
    suspend fun restoreLastHost(): String? {
        val name = startupRestore.await() ?: return null
        if (restoreSuperseded || restoreFailureShown) return null
        restoreFailureShown = true
        return name
    }

    /** 首次前台显示时尝试恢复；远端 I/O 不占用切换锁。 */
    private suspend fun performHostRestore(): String? {
        if (restoreSuperseded) return null
        val ref = try {
            hostRepo.lastRemote()
        } catch (e: IOException) {
            Log.w(TAG, "read last host failed", e)
            null
        } ?: return null
        if (restoreSuperseded) return null
        val next = open(ref).getOrNull()
        var adopted = false
        try {
            return switchLock.withLock {
                if (restoreSuperseded) return@withLock null
                if (next == null) return@withLock ref.displayName
                adopt(ref, next)
                adopted = true
                null
            }
        } finally {
            if (!adopted) next?.close()
        }
    }

    /** 打开失败保留前台主机；界面销毁只取消等待，不取消切换或持久化。 */
    suspend fun switchHost(ref: HostRef, remember: Boolean = true): Result<Unit> = appScope.async {
        supersedeHostRestore()
        switchLock.withLock {
            val changedHost = ref.id != currentHost().id
            val result = if (!changedHost && hasSession()) {
                Result.success(Unit)
            } else {
                open(ref).map { adopt(ref, it) }
            }
            if (remember && (changedHost || ref is HostRef.Remote) && result.isSuccess) {
                persistRemoteChoice((ref as? HostRef.Remote)?.id)
            }
            result
        }
    }.await()

    private suspend fun persistRemoteChoice(id: String?) {
        try {
            hostRepo.rememberRemote(id)
        } catch (e: IOException) {
            Log.w(TAG, "remember last host failed", e)
        }
    }

    private companion object {
        const val TAG = "HostSelection"
    }
}
