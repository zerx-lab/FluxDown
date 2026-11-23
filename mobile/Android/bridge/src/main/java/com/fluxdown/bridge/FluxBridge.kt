package com.fluxdown.bridge

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import com.fluxdown.core.host.HostSession as HostPort
import com.fluxdown.core.update.AppUpdateConfig
import com.fluxdown.core.update.AppUpdatePort

/**
 * 进程内唯一的 Rust 入口：持有唯一一个 [FluxCore]（一个 tokio 运行时 + 至多一个本机 daemon/agent）。
 *
 * 生成的 UniFFI 类型（[FluxCore]、`HostSession`、`*Dto`）不外泄：对外只暴露 `:core` 的 [HostPort] 端口，
 * 所有 [FluxException] 在这里归一为 `HostException`。
 */
object FluxBridge {
    // 首次访问才加载 libfluxdown_mobile.so（JNA）并建 tokio 运行时：下面的入口都切到 IO，避免卡主线程。
    private val coreState = lazy(LazyThreadSafetyMode.SYNCHRONIZED) { FluxCore() }
    private val core by coreState

    /** Read the active native sink without starting the core or waiting for the host. */
    fun androidLogDirectory(engine: Boolean): String? =
        if (coreState.isInitialized()) core.androidLogDirectory(engine) else null

    /**
     * 启动（幂等）进程内 daemon + 内嵌 agent 并返回会话。[dataDir] = 引擎数据目录，[saveDir] = 默认下载目录，
     * [deviceName] = 系统设备名（本机设备名缺失或仍是占位名时用作云端默认名；`null` 回落主机名探测）。
     */
    suspend fun openLocal(
        dataDir: String, saveDir: String, platform: String, deviceName: String?, externalLogDir: String? = null,
    ): HostPort =
        withContext(Dispatchers.IO) {
            guarded {
                RustHostSession(
                    core.openLocal(
                        LocalHostConfig(
                            dataDir = dataDir, externalLogDir = externalLogDir, saveDir = saveDir,
                            platform = platform, deviceName = deviceName,
                        ),
                    ),
                )
            }
        }

    /** 连接远端 `fluxdown-agent --server`（完成连接 + 鉴权 + 握手 + 首个快照才返回）；[endpoint] 接受 `http(s)://` / `ws(s)://`。 */
    suspend fun openRemote(endpoint: String, accessKey: String): HostPort = withContext(Dispatchers.IO) {
        guarded { RustHostSession(core.openRemote(endpoint, accessKey)) }
    }

    /** 应用自更新器（进程内唯一，首次 [config] 生效）；与本机 / 远端主机无关。 */
    suspend fun appUpdater(config: AppUpdateConfig): AppUpdatePort = withContext(Dispatchers.IO) {
        guarded { RustAppUpdater(core.appUpdater(config.toDto())) }
    }

    /** 停止进程内本机引擎（进程即将退出 / 用户关闭本机引擎时）。 */
    suspend fun shutdownLocal() = withContext(Dispatchers.IO) { guarded { core.shutdownLocal() } }
}
