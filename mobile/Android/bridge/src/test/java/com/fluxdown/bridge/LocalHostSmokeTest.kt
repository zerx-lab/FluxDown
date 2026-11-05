package com.fluxdown.bridge

import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostEvent
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession as HostPort
import com.fluxdown.core.host.HostSignal
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.AgentPreferences
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.SettingsWritePlan
import com.fluxdown.core.protocol.patchPreferences
import com.sun.net.httpserver.HttpExchange
import com.sun.net.httpserver.HttpServer
import java.io.File
import java.net.InetAddress
import java.net.InetSocketAddress
import java.nio.file.Files
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/**
 * 经【生成的 Kotlin 绑定 + JNA + 宿主机 libfluxdown_mobile】把真实的进程内 daemon + agent 跑起来：
 * 证明 JNA 加载、UniFFI 契约版本 / 校验和、`FluxBridge` / `RustHostSession` 的映射在真实数据上成立。
 * 离线、确定：下载源是本测试里的 loopback `com.sun.net.httpserver`。
 */
class LocalHostSmokeTest {
    private val step = 90_000L

    private lateinit var root: File
    private lateinit var server: HttpServer
    private val payload = ByteArray(PAYLOAD_SIZE) { i -> ((i * 31) xor (i ushr 8)).toByte() }

    @Before
    fun setUp() {
        root = Files.createTempDirectory("fluxdown_bridge_smoke_").toFile()
        server = HttpServer.create(InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0).apply {
            createContext("/smoke.bin") { exchange -> serve(exchange) }
            executor = Executors.newCachedThreadPool()
            start()
        }
    }

    @After
    fun tearDown() {
        runBlocking { FluxBridge.shutdownLocal() }
        server.stop(0)
        (server.executor as? java.util.concurrent.ExecutorService)?.shutdownNow()
        root.deleteRecursively()
    }

    private fun serve(exchange: HttpExchange) {
        exchange.use {
            val headers = exchange.responseHeaders
            headers.add("Content-Type", "application/octet-stream")
            headers.add("Accept-Ranges", "bytes")
            val range = exchange.requestHeaders.getFirst("Range")
                ?.removePrefix("bytes=")
                ?.split('-')
                ?.let { (from, to) ->
                    val start = from.toInt()
                    val end = if (to.isBlank()) payload.size - 1 else minOf(to.toInt(), payload.size - 1)
                    start..end
                }
            val slice = range ?: 0 until payload.size
            if (range != null) {
                headers.add("Content-Range", "bytes ${range.first}-${range.last}/${payload.size}")
            }
            val status = if (range != null) 206 else 200
            if (exchange.requestMethod.equals("HEAD", ignoreCase = true)) {
                headers.add("Content-Length", slice.count().toString())
                exchange.sendResponseHeaders(status, -1)
            } else {
                exchange.sendResponseHeaders(status, slice.count().toLong())
                exchange.responseBody.write(payload, slice.first, slice.count())
            }
        }
    }

    private fun url() = "http://127.0.0.1:${server.address.port}/smoke.bin"

    private suspend fun openLocal(label: String): HostPort =
        FluxBridge.openLocal(
            dataDir = File(root, "data").absolutePath,
            saveDir = File(root, "downloads-$label").absolutePath,
            platform = "test",
            deviceName = null,
        )

    private fun isCompleted(signal: HostSignal, taskId: String): Boolean = when (signal) {
        is HostSignal.Snapshot ->
            signal.snapshot.tasks.any { it.taskId == taskId && it.status == TaskStatus.Completed }
        is HostSignal.Event -> when (val event = signal.event) {
            is HostEvent.TaskChanged -> event.task.taskId == taskId && event.task.status == TaskStatus.Completed
            is HostEvent.TaskProgress -> event.taskId == taskId && event.status == TaskStatus.Completed.wire
            else -> false
        }
        is HostSignal.Fatal -> fail("session went fatal: ${signal.error}").let { false }
        HostSignal.Stale -> false
    }

    @Test
    fun localHostDownloadsAFileThroughTheGeneratedBindings() = runBlocking {
        val host = openLocal("a")
        try {
            val first = withTimeout(step) { host.signals.first() }
            val snapshot = (first as? HostSignal.Snapshot)?.snapshot
                ?: fail("first signal must be a Snapshot, got $first").let { error("unreachable") }
            assertTrue("daemon must be connected", snapshot.daemonConnected)
            assertTrue("categories must be present", snapshot.categories.isNotEmpty())
            assertTrue(snapshot.tasks.isEmpty())
            assertEquals(
                File(root, "downloads-a").absolutePath,
                snapshot.config["default_save_dir"],
            )

            val taskId = host.createTask(CreateTaskRequest(url = url()))
            assertFalse(taskId.isEmpty())
            withTimeout(step) { host.signals.first { isCompleted(it, taskId) } }

            val downloaded = File(root, "downloads-a/smoke.bin")
            assertTrue("downloaded file exists: $downloaded", downloaded.isFile)
            assertEquals(payload.size.toLong(), downloaded.length())
            assertArrayEquals(payload, downloaded.readBytes())
        } finally {
            host.close()
        }
    }

    @Test
    fun configConflictsAndRestartPersistenceCrossTheBindings() = runBlocking {
        var host = openLocal("b")
        try {
            val first = withTimeout(step) { host.signals.first() } as HostSignal.Snapshot
            val revision = first.snapshot.configRevision

            host.patchConfig(revision, mapOf("max_auto_retries" to "5"))
            val changed = withTimeout(step) {
                host.signals.first { signal ->
                    (signal is HostSignal.Event && signal.event is HostEvent.ConfigChanged &&
                        (signal.event as HostEvent.ConfigChanged).values["max_auto_retries"] == "5") ||
                        (signal is HostSignal.Snapshot && signal.snapshot.config["max_auto_retries"] == "5")
                }
            }
            assertNotNull(changed)

            // 过期 revision：FluxException.Rpc(CONFLICT) → HostException(Conflict)，经真实 FFI 往返。
            try {
                host.patchConfig(revision, mapOf("max_auto_retries" to "6"))
                fail("a stale config revision must be rejected")
            } catch (error: HostException) {
                assertEquals(HostErrorCode.Conflict, error.code)
            }

            // 停止本机主机后再开：配置落库，快照仍带着修改。
            host.close()
            FluxBridge.shutdownLocal()
            host = openLocal("b")
            val again = withTimeout(step) { host.signals.first() } as HostSignal.Snapshot
            assertTrue(again.snapshot.daemonConnected)
            assertEquals("5", again.snapshot.config["max_auto_retries"])
        } finally {
            host.close()
        }
    }

    /**
     * 设置写入路由（`ConfigEditor` / `SettingsWritePlan`）依赖的主机语义：云同步目录内的 daemon 键改走
     * `agent.preferences.patch`（同步键名 + JSON 类型值）后必须落到 daemon 配置；`sync: false` 的本机偏好
     * 进 `agent.preferences` 分区，且返回的 revision 不低于分区里的版本。
     */
    @Test
    fun preferencesChannelWritesSyncedDaemonKeysAndLocalPreferences() = runBlocking {
        val host = openLocal("c")
        try {
            withTimeout(step) { host.signals.first() } as HostSignal.Snapshot
            val plan = (SettingsWritePlan.make(mapOf("max_concurrent_tasks" to "7", "download.silent_skip_selection" to "true"))
                as SettingsWritePlan.Result.Ok).plan
            assertEquals(mapOf("download.max_concurrent_tasks" to JsonValue.of(7L)), plan.syncedPreferences.toMap())
            host.patchPreferences(plan.syncedPreferences, sync = true)
            val revision = host.patchPreferences(plan.localPreferences, sync = false)

            // 两个条件在同一次收集里累积判定：信号流是单队列拉取，`first {}` 会丢弃不匹配的信号。
            // 本机偏好的 SectionChanged 常先于 daemon 的 ConfigChanged 到达（后者经 daemon→agent
            // 异步转发），分两次 `first {}` 等待会让前一次把它丢掉，后一次永远等不到。
            var sawConfig = false
            var section: String? = null
            withTimeout(step) {
                host.signals.first { signal ->
                    if (!sawConfig) {
                        sawConfig = (signal is HostSignal.Event && signal.event is HostEvent.ConfigChanged &&
                            (signal.event as HostEvent.ConfigChanged).values["max_concurrent_tasks"] == "7") ||
                            (signal is HostSignal.Snapshot && signal.snapshot.config["max_concurrent_tasks"] == "7")
                    }
                    if (section == null) {
                        val json = when (signal) {
                            is HostSignal.Event -> (signal.event as? HostEvent.SectionChanged)
                                ?.takeIf { it.name == HostSection.agentPreferences }?.json
                            is HostSignal.Snapshot -> signal.snapshot.sections[HostSection.agentPreferences]
                            else -> null
                        }
                        if (AgentPreferences.parse(json).bool("download.silent_skip_selection", false)) {
                            section = json
                        }
                    }
                    sawConfig && section != null
                }
            }
            assertTrue(AgentPreferences.parse(section).revision >= revision)
        } finally {
            host.close()
        }
    }

    private companion object {
        const val PAYLOAD_SIZE = 2 * 1024 * 1024
    }
}
