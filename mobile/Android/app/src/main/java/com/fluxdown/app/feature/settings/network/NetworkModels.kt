package com.fluxdown.app.feature.settings.network

import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.fluxdown.core.host.HostException
import com.fluxdown.core.host.HostSession
import com.fluxdown.core.protocol.HostMethod
import com.fluxdown.core.protocol.NetworkProxyTest
import com.fluxdown.core.protocol.ProxyTestRequest
import com.fluxdown.core.protocol.ProxyTestResponse
import com.fluxdown.core.protocol.SiteAuthCredentialDto
import com.fluxdown.core.protocol.SiteAuthDeleteParams
import com.fluxdown.core.protocol.SiteAuthEntryDto
import com.fluxdown.core.protocol.SiteAuthGetParams
import com.fluxdown.core.protocol.SiteAuthSaveRequest
import com.fluxdown.core.protocol.SystemProxyDto
import com.fluxdown.core.protocol.callJson

/*
 * 网络与代理页的异步状态：系统代理检测 + 连通性测试（慢 RPC），站点凭据列表（同 iOS NetworkModels）。
 * 模型不持有会话：每次调用由页面传入当前 [HostSession]（切换主机后会话会变），
 * 并在 await 之后检查主机 / 世代，避免旧主机的迟到结果覆盖新主机。
 */

// ───────────────────────────── 代理检测与测试 ─────────────────────────────

@Stable
internal class NetworkProxyModel {
    sealed interface Detection {
        data object Idle : Detection
        data object Detecting : Detection
        data class Detected(val dto: SystemProxyDto) : Detection
        data class Failed(val message: String) : Detection
    }

    sealed interface Test {
        data object Idle : Test
        data object Running : Test
        data class Success(val latencyMs: Long) : Test

        /** [detail] 已是可直接展示的原因（不含 `proxyTestFailed` 外壳）。 */
        data class Failure(val detail: String) : Test
    }

    /** 手动分组与系统分组各测各的，结果互不覆盖。 */
    enum class TestSource { Manual, System }

    var detection by mutableStateOf<Detection>(Detection.Idle)
        private set
    var manualTest by mutableStateOf<Test>(Test.Idle)
        private set
    var systemTest by mutableStateOf<Test>(Test.Idle)
        private set

    /** 每次重置 / 新测试 +1：丢弃过期（请求参数已变或已切主机）的测试结果。两路世代独立。 */
    private var manualGeneration = 0
    private var systemGeneration = 0
    private var detectGeneration = 0

    /** 已检测到的系统代理（仅 `detected == true`）。 */
    val system: SystemProxyDto?
        get() = (detection as? Detection.Detected)?.dto?.takeIf { it.detected }

    /** `daemon.config.systemProxy`：进入系统代理模式 / 页面出现 / 重试时重新检测。 */
    suspend fun detect(session: HostSession, errorText: (HostException) -> String) {
        val generation = ++detectGeneration
        detection = Detection.Detecting
        val next = try {
            Detection.Detected(SystemProxyDto.fromJson(session.callJson(HostMethod.daemonConfigSystemProxy)))
        } catch (e: HostException) {
            Detection.Failed(errorText(e))
        }
        if (generation == detectGeneration) detection = next
    }

    /** 主机切换后旧检测结果作废。 */
    fun resetDetection() {
        detectGeneration++
        detection = Detection.Idle
    }

    /** 该来源的参数或主机变化后，旧结果不再代表当前配置。另一路不受影响。 */
    fun resetTest(source: TestSource) {
        if (testOf(source) == Test.Idle) return
        bump(source)
        setTest(source, Test.Idle)
    }

    /** 主机切换：两路测试结果都作废。 */
    fun resetTests() {
        resetTest(TestSource.Manual)
        resetTest(TestSource.System)
    }

    /** `daemon.config.proxyTest`（慢方法）：测试期间页面其余部分保持可交互。 */
    suspend fun runTest(
        source: TestSource,
        request: ProxyTestRequest,
        session: HostSession,
        errorText: (HostException) -> String,
        tlsHint: String,
    ) {
        if (testOf(source) == Test.Running) return
        val generation = bump(source)
        setTest(source, Test.Running)
        val next: Test = try {
            Test.Success(ProxyTestResponse.fromJson(session.callJson(HostMethod.daemonConfigProxyTest, request.toJson())).latencyMs)
        } catch (e: HostException) {
            Test.Failure(NetworkProxyTest.failureDetail(e.message.orEmpty(), errorText(e), tlsHint))
        } catch (c: kotlinx.coroutines.CancellationException) {
            if (generation == generationOf(source)) setTest(source, Test.Idle)
            throw c
        }
        if (generation == generationOf(source)) setTest(source, next)
    }

    private fun testOf(source: TestSource): Test = when (source) {
        TestSource.Manual -> manualTest
        TestSource.System -> systemTest
    }

    private fun setTest(source: TestSource, value: Test) {
        when (source) {
            TestSource.Manual -> manualTest = value
            TestSource.System -> systemTest = value
        }
    }

    private fun bump(source: TestSource): Int = when (source) {
        TestSource.Manual -> ++manualGeneration
        TestSource.System -> ++systemGeneration
    }

    private fun generationOf(source: TestSource): Int = when (source) {
        TestSource.Manual -> manualGeneration
        TestSource.System -> systemGeneration
    }
}

// ───────────────────────────── 站点凭据 ─────────────────────────────

@Stable
internal class SiteAuthModel {
    sealed interface Phase {
        data object Loading : Phase
        data object Loaded : Phase

        /** 首次加载失败（已有数据时的刷新失败保持 [Loaded]，不打断列表）。 */
        data class Failed(val message: String) : Phase
    }

    var entries by mutableStateOf<List<SiteAuthEntryDto>>(emptyList())
        private set
    var phase by mutableStateOf<Phase>(Phase.Loading)
        private set
    var isClearing by mutableStateOf(false)
        private set

    /** 正在删除的站点（行内禁用，避免重复请求）。 */
    var deleting by mutableStateOf<Set<String>>(emptySet())
        private set
    private var loadedHost: String? = null

    /** `daemon.siteAuth.list`。切换主机先清空旧列表；已有数据时刷新失败静默保留旧数据。 */
    suspend fun load(session: HostSession, hostId: String, errorText: (HostException) -> String) {
        if (loadedHost != hostId) {
            loadedHost = hostId
            entries = emptyList()
            deleting = emptySet()
            phase = Phase.Loading
        } else if (entries.isEmpty()) {
            phase = Phase.Loading
        }
        try {
            val list = SiteAuthEntryDto.listFromJson(session.callJson(HostMethod.daemonSiteAuthList))
            if (loadedHost != hostId) return
            entries = list
            phase = Phase.Loaded
        } catch (e: HostException) {
            if (loadedHost != hostId) return
            if (phase != Phase.Loaded) phase = Phase.Failed(errorText(e))
        }
    }

    /** `daemon.siteAuth.delete`：先乐观移除，成功后以服务端返回的新列表为准；失败则恢复并抛出。 */
    suspend fun delete(site: String, session: HostSession) {
        if (site in deleting) return
        val before = entries
        deleting = deleting + site
        entries = entries.filterNot { it.site == site }
        try {
            entries = SiteAuthEntryDto.listFromJson(
                session.callJson(HostMethod.daemonSiteAuthDelete, SiteAuthDeleteParams(site).toJson()),
            )
        } catch (e: HostException) {
            entries = before
            throw e
        } finally {
            deleting = deleting - site
        }
    }

    /** `daemon.siteAuth.clear`：返回清空后的列表。 */
    suspend fun clearAll(session: HostSession) {
        if (isClearing) return
        isClearing = true
        try {
            entries = SiteAuthEntryDto.listFromJson(session.callJson(HostMethod.daemonSiteAuthClear))
        } finally {
            isClearing = false
        }
    }

    /** 编辑表单回填：`daemon.siteAuth.get`（明文密码仅供表单，不进入列表状态）；站点已不存在 → null。 */
    suspend fun credential(site: String, session: HostSession): SiteAuthCredentialDto? =
        SiteAuthCredentialDto.fromJsonOrNull(session.callJson(HostMethod.daemonSiteAuthGet, SiteAuthGetParams(site).toJson()))

    /** `daemon.siteAuth.save` 后重新拉取列表（保存结果只带脱敏条目，且站点可能被服务端归一化）。 */
    suspend fun save(request: SiteAuthSaveRequest, session: HostSession, errorText: (HostException) -> String) {
        session.callJson(HostMethod.daemonSiteAuthSave, request.toJson())
        try {
            entries = SiteAuthEntryDto.listFromJson(session.callJson(HostMethod.daemonSiteAuthList))
            phase = Phase.Loaded
        } catch (e: HostException) {
            // 保存已成功；列表刷新失败不应让表单报错——下次进入页面会重新加载。
            phase = if (entries.isEmpty()) Phase.Failed(errorText(e)) else Phase.Loaded
        }
    }
}
