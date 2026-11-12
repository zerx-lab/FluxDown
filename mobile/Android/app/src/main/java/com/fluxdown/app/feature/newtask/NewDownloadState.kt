package com.fluxdown.app.feature.newtask

import androidx.compose.runtime.Stable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.host.CreateTaskRequest
import com.fluxdown.core.model.TaskProtocol
import com.fluxdown.core.protocol.CategoryRules
import com.fluxdown.core.protocol.CustomCategoryDto
import com.fluxdown.core.protocol.DeviceRules
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.protocol.preferences
import com.fluxdown.core.store.HostState

internal enum class ThreadMode { Auto, Preset, Custom }

/** 保存目录的分类模式：自动跟随文件名 / 不分类 / 显式选定某分类。 */
internal sealed interface SaveDirMode {
    data object Auto : SaveDirMode
    data object None : SaveDirMode
    data class Category(val id: String) : SaveDirMode
}

/** 高级面板里已改动的分区（用于 N1 入口副标题与提示点）。 */
internal enum class AdvancedItem { Auth, Proxy, UserAgent, Cookie, Referrer, Checksum, Headers, Tls }

/** 一行自定义请求头；稳定 id 作为列表 key，输入不会因重排丢焦点。 */
@Stable
internal class HeaderDraft(val id: Int) {
    var key by mutableStateOf("")
    var value by mutableStateOf("")
}

/** N2：高级选项草稿。 */
@Stable
internal class AdvancedState {
    var httpUser by mutableStateOf("")
    var httpPassword by mutableStateOf("")
    var saveSiteAuth by mutableStateOf(false)
    var proxyChoice by mutableStateOf(ProxyChoice.Follow)
    var proxyCustom by mutableStateOf("")
    var uaPreset by mutableStateOf(UaDefault)
    var userAgent by mutableStateOf("")
    var cookie by mutableStateOf("")
    var referrer by mutableStateOf("")
    var checksumAlgo by mutableStateOf(DefaultHashAlgorithm)
    var checksumHex by mutableStateOf("")
    val headers = mutableStateListOf<HeaderDraft>()
    var ignoreTls by mutableStateOf(false)
    private var nextHeaderId = 0

    fun addHeader() {
        headers += HeaderDraft(nextHeaderId++)
    }

    /** 按“当前会被提交”的口径列出已改动分区；[single] = 本次只有一条链接（单条专属项才计入）。 */
    fun modified(single: Boolean, singleHttp: Boolean): List<AdvancedItem> = buildList {
        if (singleHttp && (httpUser.isNotBlank() || httpPassword.isNotEmpty())) add(AdvancedItem.Auth)
        if (proxyChoice != ProxyChoice.Follow) add(AdvancedItem.Proxy)
        if (userAgent.isNotBlank()) add(AdvancedItem.UserAgent)
        if (cookie.isNotBlank()) add(AdvancedItem.Cookie)
        if (referrer.isNotBlank()) add(AdvancedItem.Referrer)
        if (single && checksumHex.isNotBlank()) add(AdvancedItem.Checksum)
        if (headers.any { it.key.isNotBlank() }) add(AdvancedItem.Headers)
        if (ignoreTls) add(AdvancedItem.Tls)
    }

    fun reset() {
        httpUser = ""
        httpPassword = ""
        saveSiteAuth = false
        proxyChoice = ProxyChoice.Follow
        proxyCustom = ""
        uaPreset = UaDefault
        userAgent = ""
        cookie = ""
        referrer = ""
        checksumAlgo = DefaultHashAlgorithm
        checksumHex = ""
        headers.clear()
        ignoreTls = false
    }

    fun headerMap(): Map<String, String> =
        headers.filter { it.key.isNotBlank() }.associate { it.key.trim() to it.value.trim() }
}

/** N1 表单状态。用 [create] 以主机配置作默认值；每次打开 Sheet 新建一份。 */
@Stable
internal class NewDownloadState(
    prefill: String,
    defaultSaveDir: String,
    /** 表单首次给出的、未经分类预填的保存目录；仅自动路由时随请求提交。 */
    private val offeredSaveDir: String,
    defaultQueueId: String,
    defaultSegments: Int,
    /** 打开时预选的「下载到」目标（[DispatchTarget.id]）；null = 当前主机。 */
    private val initialTargetId: String? = null,
    categories: List<CustomCategoryDto> = emptyList(),
    rememberedMode: String = "auto",
) {
    var urlText by mutableStateOf(prefill)
    var saveDir by mutableStateOf(defaultSaveDir)
        private set
    var rename by mutableStateOf("")
    var queueId by mutableStateOf(defaultQueueId)
    var threadMode by mutableStateOf(
        when {
            defaultSegments <= 0 -> ThreadMode.Auto
            defaultSegments in ThreadPresets -> ThreadMode.Preset
            else -> ThreadMode.Custom
        },
    )
    var presetThreads by mutableIntStateOf(if (defaultSegments in ThreadPresets) defaultSegments else 8)
    var customThreads by mutableIntStateOf(defaultSegments.coerceIn(1, MaxThreads))
    var submitting by mutableStateOf(false)

    /** 点了提交但无有效链接：把空输入也标红。 */
    var showEmptyError by mutableStateOf(false)

    /** 「下载到」所选远端目标的 [DispatchTarget.id]；null = 当前主机。 */
    var targetId by mutableStateOf(initialTargetId)

    /** 远端保存目录（与 [saveDir] 各自保留，来回切换目标不互相覆盖）；空 = 目标设备默认目录。 */
    var remoteSaveDir by mutableStateOf("")

    /** 下发进行中的进度（已完成 to 总数）；null = 未在下发。 */
    var dispatchProgress by mutableStateOf<Pair<Int, Int>?>(null)

    /** 下发失败的内联说明（失败的链接留在文本框里供重试）。 */
    var dispatchFailure by mutableStateOf<String?>(null)
    val advanced = AdvancedState()

    private val initialSaveDir = defaultSaveDir
    /** 表单最近一次由程序写入目录框的值（自动跟随 / 切换模式）；null = 尚未写过。 */
    private var lastWritten: String? = null
    private var manuallySelectedSaveDir = false

    /** 分类选择模式；默认自动。 */
    var mode by mutableStateOf<SaveDirMode>(SaveDirMode.Auto)
        private set
    private val initialQueueId = defaultQueueId

    val entries: List<UrlEntry> by derivedStateOf { parseEntries(urlText).dedupe() }
    val single: Boolean by derivedStateOf { entries.size == 1 }
    val singleHttp: Boolean by derivedStateOf {
        entries.singleOrNull()?.let { protocolOf(it.url).let { p -> p == TaskProtocol.Http || p == TaskProtocol.Hls } } == true
    }

    /** 全部是磁力 / eD2K：线程数不适用。 */
    val threadsApplicable: Boolean by derivedStateOf {
        entries.isEmpty() || entries.any { protocolOf(it.url).let { p -> p != TaskProtocol.Bt && p != TaskProtocol.Ed2k } }
    }

    val segments: Int
        get() = when (threadMode) {
            ThreadMode.Auto -> 0
            ThreadMode.Preset -> presetThreads
            ThreadMode.Custom -> customThreads
        }

    val saveDirValid: Boolean get() = isValidSaveDir(saveDir)

    /** 有未提交内容：关闭前需要确认。 */
    val isDirty: Boolean
        get() = urlText.isNotBlank() || rename.isNotBlank() || (saveDir != initialSaveDir && saveDir != lastWritten) ||
            mode != SaveDirMode.Auto || queueId != initialQueueId ||
            targetId != initialTargetId || remoteSaveDir.isNotBlank() ||
            advanced.modified(single = true, singleHttp = true).isNotEmpty()

    /** 当前选中的远端目标；目标已不在候选里（登出 / 解除配对 / 对方解除）时回落到当前主机；`link:<fp>` 已挂接时解析为云条目。 */
    fun target(targets: List<DispatchTarget>): DispatchTarget? = targetId?.let { id -> DeviceRules.resolveTarget(id, targets) }

    /**
     * 下发条目：单条链接时重命名优先于 `out=`；空名交给目标设备推断。
     * 只带链接 / 文件名，线程、队列与高级选项只对当前主机有意义，不随下发。
     */
    fun dispatchItems(): List<DispatchItem> {
        val list = entries
        val renamed = rename.trim()
        return list.map { e ->
            val name = when {
                list.size == 1 && renamed.isNotEmpty() -> renamed
                e.fileName.isNotEmpty() -> e.fileName
                else -> external[e.url]?.fileName.orEmpty()
            }
            DispatchItem(e, name.ifEmpty { null })
        }
    }

    /** 只保留这些链接（下发部分失败时留下失败项以便重试，成功的不会被重复下发）。 */
    fun retain(kept: List<UrlEntry>) {
        urlText = kept.joinToString("\n") { it.toText() }
    }

    /** 追加文本（粘贴 / 导入）；已有内容逐字保留。 */
    fun appendText(text: String) {
        val t = text.trim()
        if (t.isEmpty()) return
        urlText = if (urlText.isBlank()) t else urlText.trimEnd() + "\n" + t
    }

    /**
     * 外部唤起（浏览器外部下载器 / 分享 / 协议链接）带来的请求上下文，按链接归属：
     * 首条（[primaryUrl]）预填进面板，随面板提交；之后追加的链接沿用各自的
     * Cookie / 来源页 / 请求头，面板对应项不覆盖它们。
     */
    private val external = HashMap<String, ExternalDownload>()
    private var primaryUrl: String? = null

    /** 分类选择器可选项：可见、非「全部」、已配置目录的分类（按 position）；空 = 不显示选择器。 */
    val categoryOptions: List<CustomCategoryDto> =
        categories.filter { it.visible && !it.isAll && it.saveDir.trim().isNotEmpty() }
            .map { it.copy(saveDir = it.saveDir.trim()) }
            .sortedBy { it.position }

    // 必须位于 categoryOptions 之后：Kotlin 按声明顺序初始化，提前读会拿到 null。
    init {
        mode = restoreMode(rememberedMode, categoryOptions)
        when (val restored = mode) {
            SaveDirMode.Auto -> Unit
            SaveDirMode.None -> {
                saveDir = offeredSaveDir
                lastWritten = offeredSaveDir.trim()
            }
            is SaveDirMode.Category -> {
                val dir = categoryOptions.first { it.id == restored.id }.saveDir
                saveDir = dir
                lastWritten = dir
            }
        }
    }

    /** 预测目录：重命名（仅单条）> 第一条链接已识别的文件名 > 空串（交给 URL 末段回退）。 */
    fun predictedDir(): String? {
        val first = entries.firstOrNull()
        val renamed = if (entries.size <= 1) rename.trim() else ""
        val name = renamed.ifEmpty { first?.let { it.fileName.ifEmpty { external[it.url]?.fileName.orEmpty() } }.orEmpty() }
        return CategoryRules.saveDirFor(categoryOptions, name, first?.url.orEmpty())?.trim()?.takeIf { it.isNotEmpty() }
    }

    /** 自动模式命中的分类（目录等于预测目录的第一项）。 */
    fun autoMatched(): CustomCategoryDto? {
        val dir = predictedDir() ?: return null
        return categoryOptions.firstOrNull { it.saveDir == dir }
    }
    fun rememberedMode(): String = when (val selected = mode) {
        SaveDirMode.Auto -> "auto"
        SaveDirMode.None -> "none"
        is SaveDirMode.Category -> "category:${selected.id}"
    }

    /** 目录框未被手改：为空 / 等于最近一次程序写入值；尚未写过则等于 offered。 */
    private fun untouched(): Boolean {
        val current = saveDir.trim()
        if (current.isEmpty()) return true
        val written = lastWritten?.takeIf { it.isNotEmpty() }
        return current == (written ?: offeredSaveDir.trim())
    }

    /**
     * 自动模式下按预测目录改写目录框：仅目录框未被手改（[untouched]）才覆盖；
     * [force] = 从其他模式切回时无条件重算。
     */
    fun syncAuto(force: Boolean = false) {
        if (mode != SaveDirMode.Auto || manuallySelectedSaveDir || categoryOptions.isEmpty()) return
        if (!force && !untouched()) return
        val target = predictedDir() ?: offeredSaveDir.trim()
        if (saveDir != target) saveDir = target
        lastWritten = target
    }

    /** 保存目录由用户键入或浏览选择；其值不再被自动预测覆盖，也不触发自动分类路由。 */
    fun setSaveDirManually(dir: String) {
        saveDir = dir
        manuallySelectedSaveDir = true
    }


    fun selectMode(next: SaveDirMode) {
        when (next) {
            SaveDirMode.Auto -> {
                manuallySelectedSaveDir = false
                mode = next
                syncAuto(force = true)
            }
            SaveDirMode.None -> {
                mode = next
                saveDir = offeredSaveDir
                lastWritten = offeredSaveDir.trim()
            }
            is SaveDirMode.Category -> {
                val dir = categoryOptions.firstOrNull { it.id == next.id }?.saveDir ?: return
                mode = next
                saveDir = dir
                lastWritten = dir
            }
        }
    }

    /** 收下一条外部请求：链接追加进文本框（已存在则忽略）；首条同时预填 Cookie / 来源页 / UA / 请求头。 */
    fun addExternal(request: ExternalDownload) {
        if (request.url.isEmpty() || external.containsKey(request.url)) return
        external[request.url] = request
        if (primaryUrl == null) {
            primaryUrl = request.url
            val adv = advanced
            adv.cookie = request.cookies
            adv.referrer = request.referrer
            for ((key, value) in request.headers) {
                if (key.equals(USER_AGENT, ignoreCase = true)) {
                    adv.userAgent = value
                    adv.uaPreset = detectUaPreset(value)
                } else {
                    adv.addHeader()
                    adv.headers.last().let {
                        it.key = key
                        it.value = value
                    }
                }
            }
        }
        urlText = appendEntries(urlText, listOf(UrlEntry(request.url))).first
        syncAuto()
    }

    /** 为每条链接构造请求。单条：重命名优先于 `out=`、面板校验值优先于 `checksum=`、附带 HTTP 认证。 */
    fun buildRequests(startPaused: Boolean, queue: String, manualProxy: String): List<CreateTaskRequest> {
        val list = entries
        val single = list.size == 1
        val adv = advanced
        val proxy = adv.proxyChoice.wire(manualProxy, adv.proxyCustom)
        val headers = adv.headerMap()
        val panelChecksum = if (single) checksumSpec(adv.checksumAlgo, adv.checksumHex) else ""
        return list.map { e ->
            val authOk = single && singleHttp
            // 追加进来的外部链接带自己的请求上下文；首条与手输链接用面板值
            val own = external[e.url]?.takeIf { e.url != primaryUrl }
            val userAgent = adv.userAgent.trim()
            CreateTaskRequest(
                url = e.url,
                fileName = when {
                    single && rename.isNotBlank() -> rename.trim()
                    e.fileName.isNotEmpty() -> e.fileName
                    else -> external[e.url]?.fileName.orEmpty()
                },
                saveDir = saveDir.trim(),
                segments = if (threadsApplicable) segments else 0,
                queueId = queue,
                startPaused = startPaused,
                cookies = own?.cookies ?: adv.cookie.trim(),
                referrer = own?.referrer ?: adv.referrer.trim(),
                userAgent = userAgent,
                proxyUrl = proxy,
                checksum = panelChecksum.ifEmpty { e.checksum },
                ignoreTlsErrors = adv.ignoreTls,
                headers = own?.headers?.let { h ->
                    if (userAgent.isEmpty()) h else h.filterKeys { !it.equals(USER_AGENT, ignoreCase = true) }
                } ?: headers,
                httpUser = if (authOk) adv.httpUser.trim() else "",
                httpPassword = if (authOk) adv.httpPassword else "",
                saveSiteAuth = authOk && adv.saveSiteAuth,
                // 仅自动且未显式手动选择目录时交给 Rust 路由；手动选择包括等于默认目录。
                offeredSaveDir = offeredSaveDir.takeIf { mode == SaveDirMode.Auto && !manuallySelectedSaveDir && untouched() }
            )
        }
    }

    /** 只保留这些链接（导入部分失败 / 未提交时留下它们以便重试；已创建的不会被重复创建）。 */
    fun keepOnly(urls: Set<String>) = retain(entries.filter { it.url in urls })

    companion object {
        fun restoreMode(value: String, options: List<CustomCategoryDto>): SaveDirMode = when {
            value == "none" -> SaveDirMode.None
            value.startsWith("category:") -> {
                val id = value.removePrefix("category:")
                if (options.any { it.id == id }) SaveDirMode.Category(id) else SaveDirMode.Auto
            }
            else -> SaveDirMode.Auto
        }
        private const val USER_AGENT = "User-Agent"
        const val LAST_CATEGORY_MODE_KEY = "download.last_category_mode"

        /**
         * [external] 非空 = 外部唤起：链接与请求上下文按 [addExternal] 规则预填；保存目录由「自动」分类模式按文件名
         * 跟随（外部捕获命中的分类目录即其首次自动写入值）；自动模式提交时以不含分类的默认目录作 offeredSaveDir，交给 Rust 路由。
         */
        fun create(
            prefill: String,
            state: HostState,
            external: ExternalDownload? = null,
            targetId: String? = null,
            rememberedMode: String = state.preferences.string(LAST_CATEGORY_MODE_KEY, "auto"),
        ): NewDownloadState {
            val cfg = state.config
            val offered = cfg["default_save_dir"].orEmpty().trim()
            return NewDownloadState(
                prefill = prefill.trim(),
                defaultSaveDir = offered,
                offeredSaveDir = offered,
                defaultQueueId = state.defaultQueueId(),
                defaultSegments = cfg["default_segments"]?.toIntOrNull() ?: 0,
                initialTargetId = targetId,
                categories = CustomCategoryDto.fromPreference(state.preferences[CustomCategoryDto.PREFERENCE_KEY]),
                rememberedMode = rememberedMode,
            ).also { form ->
                external?.let(form::addExternal)
                form.syncAuto()
            }
        }
    }
}
