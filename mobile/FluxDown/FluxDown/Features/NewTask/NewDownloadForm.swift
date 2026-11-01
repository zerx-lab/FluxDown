import FluxDomain
import Foundation
import Observation

nonisolated enum ThreadMode: Hashable { case auto, preset, custom }

/// 高级面板里已改动的分区（用于 N1 入口副标题与提示）。
nonisolated enum AdvancedItem: Hashable {
    case auth, proxy, userAgent, cookie, referrer, checksum, headers, tls
}

/// 一行自定义请求头；稳定 id 作为列表 key，输入不会因重排丢焦点。
nonisolated struct HeaderDraft: Identifiable, Hashable {
    let id: Int
    var key = ""
    var value = ""
}

/// N2：高级选项草稿。
@Observable
final class AdvancedState {
    var httpUser = ""
    var httpPassword = ""
    var saveSiteAuth = false
    var proxyChoice: ProxyChoice = .follow
    var proxyCustom = ""
    var uaPreset = taskUaDefault
    var userAgent = ""
    var cookie = ""
    var referrer = ""
    var checksumAlgo = defaultHashAlgorithm
    var checksumHex = ""
    var headers: [HeaderDraft] = []
    var ignoreTls = false
    @ObservationIgnored private var nextHeaderId = 0

    func addHeader() {
        headers.append(HeaderDraft(id: nextHeaderId))
        nextHeaderId += 1
    }

    /// 按“当前会被提交”的口径列出已改动分区；`single` = 本次只有一条链接（单条专属项才计入）。
    func modified(single: Bool, singleHttp: Bool) -> [AdvancedItem] {
        var items: [AdvancedItem] = []
        func blank(_ s: String) -> Bool { s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        if singleHttp, !blank(httpUser) || !httpPassword.isEmpty { items.append(.auth) }
        if proxyChoice != .follow { items.append(.proxy) }
        if !blank(userAgent) { items.append(.userAgent) }
        if !blank(cookie) { items.append(.cookie) }
        if !blank(referrer) { items.append(.referrer) }
        if single, !blank(checksumHex) { items.append(.checksum) }
        if headers.contains(where: { !blank($0.key) }) { items.append(.headers) }
        if ignoreTls { items.append(.tls) }
        return items
    }

    func reset() {
        httpUser = ""
        httpPassword = ""
        saveSiteAuth = false
        proxyChoice = .follow
        proxyCustom = ""
        uaPreset = taskUaDefault
        userAgent = ""
        cookie = ""
        referrer = ""
        checksumAlgo = defaultHashAlgorithm
        checksumHex = ""
        headers = []
        ignoreTls = false
    }

    func headerMap() -> [String: String] {
        var map: [String: String] = [:]
        for h in headers {
            let key = h.key.trimmingCharacters(in: .whitespacesAndNewlines)
            if !key.isEmpty { map[key] = h.value.trimmingCharacters(in: .whitespacesAndNewlines) }
        }
        return map
    }
}

/// N1 表单状态。用 `init(prefill:state:)` 以主机配置作默认值；每次打开 Sheet 新建一份。
@Observable
final class NewDownloadForm {
    var urlText: String
    var saveDir: String
    var rename = ""
    var queueId: String
    var threadMode: ThreadMode
    var presetThreads: Int
    var customThreads: Int
    var submitting = false
    /// 点了提交但无有效链接：把空输入也标红。
    var showEmptyError = false
    /// 进行中的清单预解析（N5）：非 nil 时禁用提交，关闭 Sheet 时取消。
    var probe: ManifestProbe?
    /// 「下载到」所选远端目标的 ``DispatchTarget/id``；nil = 当前主机。
    var targetId: String?
    /// 远端保存目录（与 `saveDir` 各自保留，来回切换目标不互相覆盖）；空 = 目标设备默认目录。
    var remoteSaveDir = ""
    let advanced = AdvancedState()
    /// 「分类」选择器状态（自动 / 不分类 / 固定到某分类）；转移规则见 ``SaveDirCategoryState``。
    private(set) var categoryState: SaveDirCategoryState
    /// 第一条链接预测出的分类目录（nil = 未命中）；只用于触发器的「已匹配」文案。
    private(set) var predictedCategoryDir: String?

    @ObservationIgnored private let initialSaveDir: String
    @ObservationIgnored private let initialQueueId: String

    init(prefill: String, state: HostState) {
        let cfg = state.config
        let queue = cfg["default_queue_id"].flatMap { id in state.queues.contains { $0.queueId == id } ? id : nil }
            ?? state.queues.first { $0.queueId == TaskQueue.main }?.queueId
            ?? state.queues.first?.queueId
            ?? ""
        let dir = (cfg["default_save_dir"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let segments = Int(cfg["default_segments"] ?? "") ?? 0
        urlText = prefill.trimmingCharacters(in: .whitespacesAndNewlines)
        saveDir = dir
        queueId = queue
        initialSaveDir = dir
        categoryState = SaveDirCategoryState(initialDir: dir)
        initialQueueId = queue
        threadMode = segments <= 0 ? .auto : (threadPresets.contains(segments) ? .preset : .custom)
        presetThreads = threadPresets.contains(segments) ? segments : 8
        customThreads = min(max(segments, 1), maxThreads)
    }

    var entries: [UrlEntry] { parseEntries(urlText).dedupe() }

    var segments: Int {
        switch threadMode {
        case .auto: 0
        case .preset: presetThreads
        case .custom: customThreads
        }
    }

    var saveDirValid: Bool { isValidSaveDir(saveDir) }

    /// 分类预测的输入签名（第一条链接的文件名 + 链接）；变化时重新预测并按「自动」规则跟随目录。
    var categoryPredictionKey: String {
        guard let first = entries.first else { return "" }
        return SaveDirCategoryState.predictionName(rename: rename, entryFileName: first.fileName) + "\n" + first.url
    }

    /// 预测第一条链接命中的分类目录（Rust `route_save_dir`，与建任务时同一套匹配）。
    private func predictCategoryDir(session: any HostSession) -> String? {
        guard let first = entries.first else { return nil }
        let name = SaveDirCategoryState.predictionName(rename: rename, entryFileName: first.fileName)
        return session.categorySaveDir(fileName: name, url: first.url)
    }

    /// 预测输入变化（含打开表单）：重算预测并按「自动」规则决定是否改写目录框。
    func followCategory(session: any HostSession) {
        let predicted = predictCategoryDir(session: session)
        predictedCategoryDir = predicted
        if let dir = categoryState.follow(dir: saveDir, offered: initialSaveDir, predicted: predicted) {
            saveDir = dir
        }
    }

    /// 用户在下拉里选了新模式：目录框立即改写（切回「自动」按当前输入重算）。
    func selectCategory(_ mode: SaveDirCategoryMode, options: [CustomCategoryDto], session: any HostSession) {
        let predicted = predictCategoryDir(session: session)
        predictedCategoryDir = predicted
        if let dir = categoryState.select(mode, offered: initialSaveDir, options: options, predicted: predicted) {
            saveDir = dir
        }
    }

    /// 有未提交内容：关闭前需要确认。
    var isDirty: Bool {
        !urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !rename.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || saveDir != initialSaveDir
            || queueId != initialQueueId
            || targetId != nil
            || !remoteSaveDir.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !advanced.modified(single: true, singleHttp: true).isEmpty
    }

    /// 当前选中的远端目标；目标已不在候选里（登出 / 解除配对）时回落到当前主机。
    func target(in targets: [DispatchTarget]) -> DispatchTarget? {
        guard let targetId else { return nil }
        return DeviceRules.resolveTarget(id: targetId, in: targets)
    }

    /// 下发条目：单条链接时重命名优先于 `out=`；空名交给目标设备推断。
    /// 只带链接 / 文件名，线程、队列与高级选项只对当前主机有意义，不随下发。
    func dispatchItems() -> [DispatchItem] {
        let list = entries
        let renamed = rename.trimmingCharacters(in: .whitespacesAndNewlines)
        return list.map { entry in
            let name = list.count == 1 && !renamed.isEmpty ? renamed : entry.fileName
            return DispatchItem(entry: entry, fileName: name.isEmpty ? nil : name)
        }
    }

    /// 只保留这些链接（下发部分失败时留下失败项以便重试，成功的不会被重复下发）。
    func retain(_ entries: [UrlEntry]) {
        urlText = entries.map { $0.toText() }.joined(separator: "\n")
    }

    /// 追加文本（粘贴 / 导入）；已有内容逐字保留。
    func appendText(_ text: String) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        if urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            urlText = t
        } else {
            var base = urlText
            while let last = base.last, last.isWhitespace { base.removeLast() }
            urlText = base + "\n" + t
        }
    }

    /// 为每条链接构造请求。单条：重命名优先于 `out=`、面板校验值优先于 `checksum=`、附带 HTTP 认证。
    func buildRequests(startPaused: Bool, queue: String, manualProxy: String) -> [CreateTaskRequest] {
        let list = entries
        let single = list.count == 1
        let adv = advanced
        let proxy = adv.proxyChoice.wire(manualUrl: manualProxy, customUrl: adv.proxyCustom)
        let headers = adv.headerMap()
        let panelChecksum = single ? checksumSpec(algorithm: adv.checksumAlgo, hash: adv.checksumHex) : ""
        let authOk = single && Self.isHttpLike(list[0].url)
        let renamed = rename.trimmingCharacters(in: .whitespacesAndNewlines)
        let threadsOk = Self.threadsApplicable(list)
        return list.map { e in
            CreateTaskRequest(
                url: e.url,
                fileName: single && !renamed.isEmpty ? renamed : e.fileName,
                saveDir: saveDir.trimmingCharacters(in: .whitespacesAndNewlines),
                segments: threadsOk ? Int32(segments) : 0,
                queueId: queue,
                startPaused: startPaused,
                cookies: adv.cookie.trimmingCharacters(in: .whitespacesAndNewlines),
                referrer: adv.referrer.trimmingCharacters(in: .whitespacesAndNewlines),
                userAgent: adv.userAgent.trimmingCharacters(in: .whitespacesAndNewlines),
                proxyUrl: proxy,
                checksum: panelChecksum.isEmpty ? e.checksum : panelChecksum,
                ignoreTlsErrors: adv.ignoreTls,
                headers: headers,
                httpUser: authOk ? adv.httpUser.trimmingCharacters(in: .whitespacesAndNewlines) : "",
                httpPassword: authOk ? adv.httpPassword : "",
                saveSiteAuth: authOk && adv.saveSiteAuth,
                offeredSaveDir: categoryState.offeredSaveDir(dir: saveDir, initial: initialSaveDir)
            )
        }
    }

    #if FULL_FLAVOR
    /// 清单（任务组）的组级选项：沿用表单里的线程 / Cookie / UA / 代理 / 请求头 / TLS 设置。
    func manifestBase(queue: String, manualProxy: String) -> ManifestBaseOptions {
        let adv = advanced
        return ManifestBaseOptions(
            saveDir: saveDir.trimmingCharacters(in: .whitespacesAndNewlines),
            queueId: queue,
            segments: Int32(segments),
            cookies: adv.cookie.trimmingCharacters(in: .whitespacesAndNewlines),
            referrer: adv.referrer.trimmingCharacters(in: .whitespacesAndNewlines),
            userAgent: adv.userAgent.trimmingCharacters(in: .whitespacesAndNewlines),
            proxyUrl: adv.proxyChoice.wire(manualUrl: manualProxy, customUrl: adv.proxyCustom),
            headers: adv.headerMap(),
            ignoreTls: adv.ignoreTls
        )
    }
    #endif

    /// 放弃进行中的清单预解析（用户取消 / 关闭 Sheet）。
    func cancelProbe() {
        probe?.cancel()
        probe = nil
    }

    /// 已成功创建的链接从文本框移除（部分失败时保留失败项以便重试，避免重复创建）。
    func removeUrls(_ done: Set<String>) {
        guard !done.isEmpty else { return }
        urlText = entries.filter { !done.contains($0.url) }.map { $0.toText() }.joined(separator: "\n")
    }

    static func isHttpLike(_ url: String) -> Bool {
        let p = protocolOf(url)
        return p == .http || p == .hls
    }

    /// 全是磁力 / eD2K：线程数不适用。
    static func threadsApplicable(_ entries: [UrlEntry]) -> Bool {
        entries.isEmpty || entries.contains { e in
            let p = protocolOf(e.url)
            return p != .bt && p != .ed2k
        }
    }
}
