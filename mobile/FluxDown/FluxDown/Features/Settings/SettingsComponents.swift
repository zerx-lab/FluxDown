import FluxDomain
import FluxUI
import SwiftUI
import UIKit

// 设置行构件（03-settings §2.1）：全部是系统控件在 `Form` 里的内容层拼装，不使用玻璃。
// 行读写的键来自 `SettingsItem`（daemon 配置键或 agent 偏好键）；写入路由由 `ConfigEditor` 按目录处理。

// MARK: - 图标方块

/// 30×30 圆角方块图标（01-foundations §5.1 `tile`：半径 8、白色符号 17pt medium）：系统「设置」式纯色底，不叠渐变 / 高光。
/// 随 Dynamic Type 轻度放大（封顶 56）；纯装饰，对 VoiceOver 隐藏。
struct SettingsTile: View {
    let symbol: String
    let color: Color

    @ScaledMetric(relativeTo: .body) private var scaled: CGFloat = 30

    var body: some View {
        let side = min(scaled, 56)
        Image(systemName: symbol)
            .font(.system(size: side * 17 / 30, weight: .medium))
            .foregroundStyle(.white)
            .frame(width: side, height: side)
            .background(color, in: .rect(cornerRadius: side * 8 / 30, style: .continuous))
            .accessibilityHidden(true)
    }
}

/// 图标方块 + 标题（+ 可选次行）。
struct SettingsTileLabel: View {
    let title: String
    var subtitle: String?
    let symbol: String
    let color: Color

    var body: some View {
        HStack(spacing: 12) {
            SettingsTile(symbol: symbol, color: color)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - 定位与高亮（§2.5）

/// 搜索结果 → 目标页 → 滚动并脉冲高亮目标行。设置首页搜索与全局搜索共用同一个实例（`shared`）。
@MainActor
@Observable
final class SettingsFocus {
    static let shared = SettingsFocus()

    /// 待定位的行 id。目标页出现后由 `reveal` 在布局等待结束时消费；带分页签的页面（BT / eD2K）
    /// 在此之前观察它并先切到目标行所在的页签。
    var target: String?
    /// 每次 `request` 加一：`SettingsPage` 用它重启 `reveal`（目标页已在屏幕上时再次定位同样有效）。
    private(set) var revision = 0
    /// 正在高亮的行 id。
    private(set) var pulsing: String?

    /// 请求定位到某一行（分类入口 `category.*` 不对应具体行，忽略）。
    func request(_ id: String) {
        guard !id.hasPrefix("category.") else { return }
        target = id
        revision += 1
    }

    /// 目标页出现后调用：滚到目标行并高亮 1.2s。
    func reveal(using proxy: ScrollViewProxy, reduceMotion: Bool) async {
        guard let id = target else { return }
        do {
            try await Task.sleep(for: .milliseconds(350)) // 等推入动画与行布局完成
        } catch {
            if target == id { target = nil } // 页面已离开：不留陈旧目标（否则下次打开会误切页签）
            return
        }
        if target == id { target = nil }
        withFluxAnimation(.smooth, reduceMotion: reduceMotion) { proxy.scrollTo(id, anchor: .center) }
        withFluxAnimation(.smooth, reduceMotion: reduceMotion) { pulsing = id }
        do {
            try await Task.sleep(for: .milliseconds(1200))
        } catch {
            pulsing = nil
            return
        }
        // 减弱动态效果：静态高亮 1.2s 后直接撤掉；否则用 1.2s 的无过冲弹簧淡出。
        if reduceMotion {
            pulsing = nil
        } else {
            withAnimation(.smooth(duration: 1.2)) { pulsing = nil }
        }
    }
}

/// 行标识 + 行内失败说明（红色 caption，3 秒后淡出）+ 搜索定位高亮。
private struct SettingsRowModifier: ViewModifier {
    let id: String
    let failureKey: String?

    @Environment(ConfigEditor.self) private var editor
    @Environment(SettingsFocus.self) private var focus
    @Environment(\.fluxAccent) private var accent

    func body(content: Content) -> some View {
        let failure = failureKey.flatMap { editor.failures[$0] }
        VStack(alignment: .leading, spacing: 6) {
            content
            if let failure {
                Label(failure, systemImage: FluxSymbol.failure)
                    .font(.caption)
                    .foregroundStyle(Color.fdStatusFailedText)
                    .transition(.opacity)
            }
        }
        .fluxAnimation(.smooth, value: failure)
        .id(id)
        .listRowBackground(focus.pulsing == id ? accent.color.opacity(0.18) : nil)
    }
}

extension View {
    /// 设置行：`id` 供搜索定位；`failureKey` 给出时在行下显示该键最近一次写入失败的原因。
    func settingsRow(_ id: String, failureKey: String? = nil) -> some View {
        modifier(SettingsRowModifier(id: id, failureKey: failureKey))
    }
}

/// 收起键盘（数字键盘无 Return 键：键盘工具条「完成」）。
@MainActor
func settingsDismissKeyboard() {
    UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
}

// MARK: - 页面骨架

/// 设置子页骨架：`Form` + 只读横幅 + 搜索定位 + 键盘「完成」 + 页脚 ☁︎ 图例。
struct SettingsPage<Content: View>: View {
    let title: String
    /// 主机类设置页：断线时顶部出现只读横幅（设备本地页不出现）。
    var showsReadOnlyBanner = false
    /// 页内有云同步行时在页脚显示「☁︎ 表示该项会在已登录的设备间同步」（03-settings §2.4）。
    var showsSyncLegend = false
    @ViewBuilder let content: Content

    @Environment(AppContainer.self) private var container
    @Environment(SettingsFocus.self) private var focus
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ScrollViewReader { proxy in
            Form {
                if showsReadOnlyBanner, container.store.state.isReadOnly {
                    Section {
                        Banner(text: L("localServiceDisconnected"), tone: .warning, systemImage: FluxSymbol.offline, slim: true)
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                    }
                }
                content
                if showsSyncLegend {
                    Section {} footer: { SettingsSyncLegend() }
                }
            }
            .readableContentWidth()
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .scrollDismissesKeyboard(.interactively)
            .toolbar {
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button(L("confirm")) { settingsDismissKeyboard() }
                }
            }
            .task(id: focus.revision) { await focus.reveal(using: proxy, reduceMotion: reduceMotion) }
        }
    }
}

// MARK: - ☁︎ 同步标记

/// ☁︎ 标记（标题后 12pt `cloud`，tertiary；VoiceOver 读「已同步」）。FluxCloud 是第三方服务，不得用受限的 `icloud*`。
struct SettingsSyncMark: View {
    var body: some View {
        Image(systemName: FluxSymbol.cloud)
            .font(.caption)
            .foregroundStyle(.tertiary)
            .accessibilityLabel(L("settingsSyncedA11y"))
    }
}

/// 页脚图例。
struct SettingsSyncLegend: View {
    var body: some View {
        Label {
            Text(L("settingsSyncLegend"))
        } icon: {
            Image(systemName: FluxSymbol.cloud).accessibilityHidden(true)
        }
        .font(.footnote)
        .foregroundStyle(.secondary)
        .labelStyle(.titleAndIcon)
    }
}

// MARK: - 行：文字块

/// 标题（+ ☁︎）+ 说明（≤2 行 `.footnote` `.secondary`）。
struct SettingsText: View {
    let title: String
    var detail: String?
    var synced = false

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                Text(title)
                if synced { SettingsSyncMark() }
            }
            if let detail, !detail.isEmpty {
                Text(detail)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// 文字块在左、控件在右（`LeadingTrailingRow`：常规字号恒左右排，说明文字换行；辅助功能字号上下排）。
struct SettingsTrailingLayout<Trailing: View>: View {
    let title: String
    var detail: String?
    var synced = false
    @ViewBuilder let trailing: Trailing

    var body: some View {
        LeadingTrailingRow {
            SettingsText(title: title, detail: detail, synced: synced)
        } trailing: {
            trailing
        }
    }
}

extension View {
    /// 行内数值输入框：右对齐等宽数字、与系统步进器同材质的填充色圆角底。
    /// `width` = 常规字号下的固定宽度（nil = 占满剩余宽度）；辅助功能字号下一律占满整行。
    func settingsValueField(width: CGFloat? = 88) -> some View {
        modifier(SettingsValueFieldStyle(width: width))
    }
}

private struct SettingsValueFieldStyle: ViewModifier {
    let width: CGFloat?
    @Environment(\.dynamicTypeSize) private var typeSize

    func body(content: Content) -> some View {
        let fixed = typeSize.isAccessibilitySize ? nil : width
        content
            .textFieldStyle(.plain)
            .multilineTextAlignment(.trailing)
            .monospacedDigit()
            .padding(.horizontal, 10)
            .frame(width: fixed)
            .frame(maxWidth: fixed == nil ? .infinity : nil, minHeight: 36)
            .background(.fill.tertiary, in: .rect(cornerRadius: 9, style: .continuous))
    }
}

// MARK: - 行：开关 / 选择 / 数字

/// `ToggleRow`：系统开关（统一系统默认绿，§1 foundations `ui.toggleRow`）；触感由系统开关自带。
struct ConfigToggleRow: View {
    let item: SettingsItem

    @Environment(ConfigEditor.self) private var editor

    init(item: SettingsItem) {
        self.item = item
    }

    init(row: SettingsDownloadRow) {
        item = row.item
    }

    var body: some View {
        let key = item.key
        Toggle(isOn: Binding(
            get: { editor.form.bool(key) },
            set: { editor.set(key, SettingsConfigForm.wire($0)) }
        )) {
            SettingsText(title: item.title, detail: item.detail, synced: item.isSynced)
        }
        .tint(Color.fdToggleOn)
        .settingsRow(item.id, failureKey: key)
    }
}

/// 选项（`ConfigPickerRow` / `ConfigSegmentedRow` 共用）。
nonisolated struct SettingsOption: Identifiable, Hashable {
    let id: String
    let label: String
}

/// `PickerRow`：`Picker(.menu)`，当前值右对齐；值不在选项里（如已删除的队列）时补一项原值。
struct ConfigPickerRow: View {
    typealias Option = SettingsOption

    let item: SettingsItem
    let options: [Option]
    let fallback: String

    @Environment(ConfigEditor.self) private var editor

    init(item: SettingsItem, options: [Option], fallback: String) {
        self.item = item
        self.options = options
        self.fallback = fallback
    }

    init(row: SettingsDownloadRow, options: [Option], fallback: String) {
        self.init(item: row.item, options: options, fallback: fallback)
    }

    var body: some View {
        let key = item.key
        let current = editor.form.value(key) ?? fallback
        let shown = options.contains { $0.id == current } ? options : options + [Option(id: current, label: current)]
        SettingsTrailingLayout(title: item.title, detail: item.detail, synced: item.isSynced) {
            Picker(item.title, selection: Binding(get: { current }, set: { editor.set(key, $0) })) {
                ForEach(shown) { Text($0.label).tag($0.id) }
            }
            .pickerStyle(.menu)
            .labelsHidden()
        }
        .settingsRow(item.id, failureKey: key)
    }
}

/// 选项 ≤4 且需要一眼对比时用分段控件（标题在上，分段在下）。
struct ConfigSegmentedRow: View {
    let item: SettingsItem
    let options: [SettingsOption]
    let fallback: String

    @Environment(ConfigEditor.self) private var editor

    var body: some View {
        let key = item.key
        let current = editor.form.value(key) ?? fallback
        VStack(alignment: .leading, spacing: 10) {
            SettingsText(title: item.title, detail: item.detail, synced: item.isSynced)
            Picker(item.title, selection: Binding(get: { current }, set: { editor.set(key, $0) })) {
                ForEach(options) { Text($0.label).tag($0.id) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
        }
        .settingsRow(item.id, failureKey: key)
    }
}

/// `NumberRow`：右侧数字框（失焦 / 完成时提交并钳位）+ 紧贴的 ± 步进。
/// 特殊值（0 = 自动、−1 = 无限…）显示为文字；越界输入钳位并提示「已调整为 n」+ 警告触感。
struct ConfigNumberRow: View {
    let item: SettingsItem
    let range: ClosedRange<Int>
    let fallback: Int
    var specials: [Int: String] = [:]
    var unitHint: String?
    var step = 1

    @Environment(ConfigEditor.self) private var editor
    @State private var draft = ""
    @State private var adjustedTo: Int?
    @State private var stepTick = 0
    @State private var warnTick = 0
    @FocusState private var focused: Bool

    init(
        item: SettingsItem, range: ClosedRange<Int>, fallback: Int,
        specials: [Int: String] = [:], unitHint: String? = nil, step: Int = 1
    ) {
        self.item = item
        self.range = range
        self.fallback = fallback
        self.specials = specials
        self.unitHint = unitHint
        self.step = step
    }

    init(
        row: SettingsDownloadRow, range: ClosedRange<Int>, fallback: Int,
        specials: [Int: String] = [:], unitHint: String? = nil
    ) {
        self.init(item: row.item, range: range, fallback: fallback, specials: specials, unitHint: unitHint)
    }

    private var value: Int {
        SettingsNumber.clamp(editor.form.int(item.key, default: fallback), to: range).value
    }

    private func display(_ v: Int) -> String { specials[v] ?? String(v) }

    var body: some View {
        let title = item.title
        VStack(alignment: .leading, spacing: 6) {
            SettingsTrailingLayout(title: title, detail: item.detail, synced: item.isSynced) {
                HStack(spacing: 8) {
                    TextField(title, text: $draft)
                        .keyboardType(range.lowerBound < 0 ? .numbersAndPunctuation : .numberPad)
                        .focused($focused)
                        .onSubmit(commit)
                        .settingsValueField()
                        .accessibilityLabel(title)
                        .accessibilityValue(display(value))
                    Stepper(title, value: Binding(get: { value }, set: { stepTo($0) }), in: range, step: step)
                        .labelsHidden()
                }
            }
            if let unitHint {
                Text(unitHint).font(.caption).foregroundStyle(.secondary)
            }
            if let adjustedTo {
                Text(L("mobileAdjustedTo", ["n": adjustedTo]))
                    .font(.caption)
                    .foregroundStyle(Color.fdStatusWarningText)
                    .transition(.opacity)
            }
        }
        .settingsRow(item.id, failureKey: item.key)
        .fluxAnimation(.smooth, value: adjustedTo)
        .sensoryFeedback(FluxHaptic.soft.sensoryFeedback, trigger: stepTick)
        .sensoryFeedback(FluxHaptic.warning.sensoryFeedback, trigger: warnTick)
        .onAppear { draft = display(value) }
        .onChange(of: value) { _, new in if !focused { draft = display(new) } }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        .onDisappear { commit() }
    }

    private func stepTo(_ next: Int) {
        stepTick += 1
        adjustedTo = nil
        editor.set(item.key, String(next))
        draft = display(next)
    }

    private func commit() {
        // 特殊值文字（「自动」）未被改动时保持原值。
        guard draft != display(value) else { return }
        guard let typed = SettingsNumber.parse(draft) else {
            draft = display(value)
            warnTick += 1
            return
        }
        let result = SettingsNumber.clamp(typed, to: range)
        draft = display(result.value)
        if result.adjusted {
            warnTick += 1
            adjustedTo = result.value
            Task {
                do {
                    try await Task.sleep(for: .seconds(3))
                } catch {
                    return
                }
                adjustedTo = nil
            }
        } else {
            adjustedTo = nil
        }
        if result.value != value { editor.set(item.key, String(result.value)) }
    }
}

/// 非负小数行（分享率等，步长 0.1，0 = 关闭）：数字框（小数键盘）+ 步进。
struct ConfigDecimalRow: View {
    let item: SettingsItem
    var fallback = 0.0
    var step = 0.1
    /// 0 的占位文字（如「关闭」）。
    var zeroText: String?

    @Environment(ConfigEditor.self) private var editor
    @State private var draft = ""
    @State private var stepTick = 0
    @State private var warnTick = 0
    @FocusState private var focused: Bool

    private var value: Double { max(0, editor.form.double(item.key, default: fallback)) }

    private func display(_ v: Double) -> String {
        if v == 0, let zeroText { return zeroText }
        var s = String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), v)
        while s.hasSuffix("0") { s.removeLast() }
        if s.hasSuffix(".") { s.removeLast() }
        return s.isEmpty ? "0" : s
    }

    var body: some View {
        let title = item.title
        SettingsTrailingLayout(title: title, detail: item.detail, synced: item.isSynced) {
            HStack(spacing: 8) {
                TextField(title, text: $draft)
                    .keyboardType(.decimalPad)
                    .focused($focused)
                    .settingsValueField()
                    .onChange(of: draft) { _, new in
                        let clean = SettingsRateLimit.sanitize(new)
                        if clean != new, new != zeroText { draft = clean }
                    }
                    .accessibilityLabel(title)
                    .accessibilityValue(display(value))
                Stepper(title, onIncrement: { nudge(up: true) }, onDecrement: { nudge(up: false) })
                    .labelsHidden()
            }
        }
        .settingsRow(item.id, failureKey: item.key)
        .sensoryFeedback(FluxHaptic.soft.sensoryFeedback, trigger: stepTick)
        .sensoryFeedback(FluxHaptic.warning.sensoryFeedback, trigger: warnTick)
        .onAppear { draft = display(value) }
        .onChange(of: value) { _, new in if !focused { draft = display(new) } }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        .onDisappear { commit() }
    }

    private func nudge(up: Bool) {
        let next = max(0, ((value + (up ? step : -step)) * 10).rounded() / 10)
        stepTick += 1
        focused = false
        editor.set(item.key, String(next))
        draft = display(next)
    }

    private func commit() {
        guard draft != display(value) else { return }
        let text = draft.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: ".")
        if text.isEmpty {
            if value != 0 { editor.set(item.key, "0") }
            draft = display(0)
            return
        }
        guard let typed = Double(text), typed.isFinite, typed >= 0 else {
            draft = display(value)
            warnTick += 1
            return
        }
        draft = display(typed)
        if typed != value { editor.set(item.key, String(typed)) }
    }
}

/// `TextRow`：失焦 / 完成键 / 离开页面时提交的单行文本（路径、UA 等不宜逐字写入主机）。
struct ConfigTextRow: View {
    let id: String
    let key: String
    let title: String
    var detail: String?
    var prompt: String?
    var keyboard: UIKeyboardType = .default
    /// 密码：`SecureField` + 显示 / 隐藏。
    var isSecure = false
    var monospaced = true
    /// 提交前校验；返回非 nil = 不写入并在行内显示该文案。
    var validate: (String) -> String? = { _ in nil }

    @Environment(ConfigEditor.self) private var editor
    @State private var draft = ""
    @State private var invalid: String?
    @State private var warnTick = 0
    @State private var revealed = false
    @FocusState private var focused: Bool

    private var current: String { editor.form.value(key) ?? "" }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SettingsText(title: title, detail: detail, synced: SettingsCatalog.isSynced(key))
            HStack(spacing: 8) {
                field
                if isSecure {
                    Button {
                        revealed.toggle()
                    } label: {
                        Image(systemName: revealed ? "eye.slash" : "eye")
                            .frame(minWidth: 44, minHeight: 44)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(L(revealed ? "mobileHidePassword" : "mobileShowPassword"))
                }
            }
            if let invalid {
                Label(invalid, systemImage: FluxSymbol.failure)
                    .font(.caption)
                    .foregroundStyle(Color.fdStatusFailedText)
            }
        }
        .settingsRow(id, failureKey: key)
        .fluxAnimation(.smooth, value: invalid)
        .sensoryFeedback(FluxHaptic.warning.sensoryFeedback, trigger: warnTick)
        .onAppear { draft = current }
        .onChange(of: current) { _, new in if !focused { draft = new } }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        .onDisappear { commit() }
    }

    @ViewBuilder
    private var field: some View {
        Group {
            if isSecure, !revealed {
                SecureField(title, text: $draft, prompt: prompt.map { Text($0) })
                    .textContentType(.password)
            } else {
                TextField(title, text: $draft, prompt: prompt.map { Text($0) })
                    .font(monospaced ? .fluxMono : .body)
            }
        }
        .keyboardType(keyboard)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .submitLabel(.done)
        .focused($focused)
        .onSubmit { commit() }
        .textFieldStyle(.roundedBorder)
        .frame(minHeight: 44)
    }

    private func commit() {
        let trimmed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != current else {
            invalid = nil
            return
        }
        if let message = validate(trimmed) {
            invalid = message
            warnTick += 1
            return
        }
        invalid = nil
        draft = trimmed
        editor.set(key, trimmed)
    }
}

/// `LinesRow`：多行文本（Tracker / 服务器 / 订阅地址，每行一条）。行内 `TextEditor`（等宽），标题下显示行数，
/// 失焦 / 离开页面时写回；提供「粘贴」与全屏编辑。`toEditor` / `fromEditor` 把存储形态与逐行编辑形态互转
/// （如 `ed2k_server_list` 的逗号分隔）。
struct ConfigLinesRow: View {
    let item: SettingsItem
    var placeholder: String
    var isEnabled = true
    var toEditor: (String) -> String = { $0 }
    var fromEditor: (String) -> String = { $0 }

    @Environment(ConfigEditor.self) private var editor
    @State private var draft = ""
    @State private var showFullScreen = false
    @FocusState private var focused: Bool

    private var stored: String { toEditor(editor.form.value(item.key) ?? "") }

    private var lineCount: Int {
        draft.split(whereSeparator: \.isNewline).filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }.count
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                SettingsText(title: item.title, detail: item.detail, synced: item.isSynced)
                Spacer(minLength: 8)
                Text(L("mobileLinesCount", ["n": lineCount]))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            TextEditor(text: $draft)
                .font(.fluxMono)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .scrollContentBackground(.hidden)
                .focused($focused)
                .frame(minHeight: 96, maxHeight: 220)
                .padding(8)
                .background(.fill.tertiary, in: .rect(cornerRadius: 12, style: .continuous))
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text(placeholder)
                            .font(.fluxMono)
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 16)
                            .allowsHitTesting(false)
                            .accessibilityHidden(true)
                    }
                }
                .accessibilityLabel(item.title)
            HStack(spacing: 16) {
                PasteButton(payloadType: String.self) { strings in
                    let pasted = strings.joined(separator: "\n")
                    guard !pasted.isEmpty else { return }
                    draft = draft.isEmpty || draft.hasSuffix("\n") ? draft + pasted : draft + "\n" + pasted
                    commit()
                }
                .labelStyle(.titleAndIcon)
                .buttonBorderShape(.capsule)
                .controlSize(.small)
                Button {
                    showFullScreen = true
                } label: {
                    Label(L("mobileEditFullScreen"), systemImage: "arrow.up.left.and.arrow.down.right")
                        .font(.footnote)
                }
                .buttonStyle(.borderless)
                Spacer(minLength: 0)
            }
        }
        .disabled(!isEnabled)
        .opacity(isEnabled ? 1 : 0.5)
        .settingsRow(item.id, failureKey: item.key)
        .onAppear { draft = stored }
        .onChange(of: stored) { _, new in if !focused, !showFullScreen { draft = new } }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        .onDisappear { commit() }
        .sheet(isPresented: $showFullScreen, onDismiss: commit) {
            NavigationStack {
                TextEditor(text: $draft)
                    .font(.fluxMono)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(.horizontal, 12)
                    .navigationTitle(item.title)
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button(L("confirm")) { showFullScreen = false }
                        }
                    }
            }
            .presentationDetents([.large])
        }
    }

    private func commit() {
        let lines = draft.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
        let cleaned = lines.filter { !$0.isEmpty }.joined(separator: "\n")
        let next = fromEditor(cleaned)
        guard next != (editor.form.value(item.key) ?? "") else { return }
        editor.set(item.key, next)
    }
}

// MARK: - 行：限速

/// `SettingsRateLimitRow`（§2.3）：数字框 + 单位菜单 + ± 步进 + 预设 chips；1024 进制两位小数，0 = 不限制。
/// 切换单位保留数字；所选单位只在本次会话记住；当前单位无法精确表示时自动回退到能精确表示的最大单位。
struct SettingsRateLimitRow: View {
    let item: SettingsItem

    @Environment(ConfigEditor.self) private var editor
    @State private var preferred: SettingsRateUnit = .mb
    @State private var draft = ""
    @State private var warnTick = 0
    @FocusState private var focused: Bool

    init(item: SettingsItem) {
        self.item = item
    }

    init(row: SettingsDownloadRow) {
        item = row.item
    }

    private var bytes: Int64 { max(0, editor.form.long(item.key, default: 0)) }
    private var unit: SettingsRateUnit { SettingsRateLimit.effectiveUnit(bytes: bytes, preferred: preferred) }

    var body: some View {
        let title = item.title
        let unlimited = L("mobileSpeedUnlimited")
        VStack(alignment: .leading, spacing: 10) {
            SettingsText(title: title, detail: item.detail, synced: item.isSynced)
            HStack(spacing: 8) {
                TextField(title, text: $draft, prompt: Text(unlimited))
                    .keyboardType(.decimalPad)
                    .focused($focused)
                    .settingsValueField(width: nil)
                    .onChange(of: draft) { _, new in
                        let clean = SettingsRateLimit.sanitize(new)
                        if clean != new { draft = clean }
                    }
                    .accessibilityLabel(title)
                    .accessibilityValue(bytes == 0 ? unlimited : "\(draft) \(unit.label)")
                Menu {
                    Picker(L("mobileSpeedUnit"), selection: Binding(get: { unit }, set: { changeUnit($0) })) {
                        ForEach(SettingsRateUnit.allCases) { Text($0.label).tag($0) }
                    }
                } label: {
                    HStack(spacing: 4) {
                        Text(unit.label).monospacedDigit()
                        Image(systemName: "chevron.up.chevron.down").font(.caption2)
                    }
                    .frame(minHeight: 44)
                    .contentShape(.rect)
                }
                .accessibilityLabel(L("mobileSpeedUnit"))
                .accessibilityValue(unit.label)
                Stepper(title, onIncrement: { nudge(up: true) }, onDecrement: { nudge(up: false) })
                    .labelsHidden()
                Menu {
                    ForEach(Array(SettingsRateLimit.presets.enumerated()), id: \.offset) { _, preset in
                        presetButton(preset.amount, preset.unit, unlimited: unlimited)
                    }
                } label: {
                    Image(systemName: "gauge.with.dots.needle.67percent")
                        .frame(minWidth: 44, minHeight: 44)
                        .contentShape(.rect)
                }
                .accessibilityLabel(L("mobileSpeedPresets"))
            }
        }
        .settingsRow(item.id, failureKey: item.key)
        .sensoryFeedback(FluxHaptic.warning.sensoryFeedback, trigger: warnTick)
        .onAppear { draft = SettingsRateLimit.text(bytes: bytes, unit: unit) }
        .onChange(of: bytes) { _, _ in if !focused { syncDraft() } }
        .onChange(of: unit) { _, _ in if !focused { syncDraft() } }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit(draft, unit) } }
        .onDisappear { commit(draft, unit) }
        .onSubmit { commit(draft, unit) }
    }

    @ViewBuilder
    private func presetButton(_ amount: Int, _ chipUnit: SettingsRateUnit, unlimited: String) -> some View {
        let value = SettingsRateLimit.bytes(amount: amount, unit: chipUnit)
        let label = amount == 0 ? unlimited : "\(amount) \(chipUnit.label)"
        let selected = bytes == value
        Button {
            if amount != 0 { preferred = chipUnit }
            focused = false
            if value != bytes { editor.set(item.key, String(value)) }
            draft = SettingsRateLimit.text(bytes: value, unit: amount == 0 ? unit : chipUnit)
        } label: {
            if selected {
                Label(label, systemImage: FluxSymbol.done)
            } else {
                Text(label)
            }
        }
    }

    private func syncDraft() {
        draft = SettingsRateLimit.text(bytes: bytes, unit: unit)
    }

    private func changeUnit(_ next: SettingsRateUnit) {
        preferred = next
        commit(draft, next) // 保留已输入的数字
    }

    private func nudge(up: Bool) {
        let next = SettingsRateLimit.stepped(bytes: bytes, unit: unit, up: up)
        focused = false
        if next != bytes { editor.set(item.key, String(next)) }
        draft = SettingsRateLimit.text(bytes: next, unit: unit)
    }

    private func commit(_ text: String, _ target: SettingsRateUnit) {
        guard let next = SettingsRateLimit.parse(text, unit: target) else {
            warnTick += 1
            syncDraft()
            return
        }
        if next != bytes { editor.set(item.key, String(next)) }
    }
}

// MARK: - 行：动作 / 状态

/// `ActionRow`：整行按钮；`isRunning` 时显示 `ProgressView` 与进行中文案并禁用（慢方法的加载态，不阻塞其它 UI）。
struct SettingsActionRow: View {
    let title: String
    var runningTitle: String?
    var systemImage: String?
    var role: ButtonRole?
    var isRunning = false
    let action: () -> Void

    var body: some View {
        Button(role: role, action: action) {
            HStack(spacing: 10) {
                if isRunning {
                    ProgressView()
                    Text(runningTitle ?? title)
                } else if let systemImage {
                    Label(title, systemImage: systemImage)
                } else {
                    Text(title)
                }
                Spacer(minLength: 0)
            }
            .frame(minHeight: 44, alignment: .leading)
            .contentShape(.rect)
        }
        .disabled(isRunning)
        .accessibilityAddTraits(isRunning ? .updatesFrequently : [])
    }
}

/// 结果 / 状态行：图标 + 文字（语气不只靠颜色），文字可长按选择复制。
struct SettingsStatusLine: View {
    nonisolated enum Tone { case success, warning, failure, neutral }

    let text: String
    var tone: Tone = .neutral
    var systemImage: String?

    var body: some View {
        Label {
            Text(text).textSelection(.enabled)
        } icon: {
            Image(systemName: systemImage ?? defaultSymbol).accessibilityHidden(true)
        }
        .font(.footnote)
        .foregroundStyle(color)
    }

    private var defaultSymbol: String {
        switch tone {
        case .success: "checkmark.circle.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .failure: "xmark.octagon.fill"
        case .neutral: "info.circle"
        }
    }

    private var color: Color {
        switch tone {
        case .success: Color.fdStatusSeedingText
        case .warning: Color.fdStatusWarningText
        case .failure: Color.fdStatusFailedText
        case .neutral: Color.secondary
        }
    }
}

// MARK: - 信息行

/// `SettingsInfoRow`：只读键值（版本、路径）。
struct SettingsInfoRow: View {
    let title: String
    let value: String

    var body: some View {
        LabeledContent(title) {
            Text(value).monospacedDigit().textSelection(.enabled)
        }
    }
}
