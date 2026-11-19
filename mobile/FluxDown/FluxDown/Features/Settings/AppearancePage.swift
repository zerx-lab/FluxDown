import FluxDomain
import FluxUI
import SwiftUI
import UIKit

/// S4 · 外观：预览卡、语言（系统每 App 语言）、明暗模式、主题色（预设 + 自定义取色器）。
/// 修改先写入 `AppearanceStore`（UserDefaults 兜底，断线也立即生效；根视图 `.tint` / `preferredColorScheme` 随之实时重绘），
/// 再经 `AppearanceSync` 写 `agent.preferences`（`appearance.theme_mode` / `color_scheme` / `custom_color`，云同步键）；
/// 主机值变化时反向应用到本机。
///
/// 省略的 PC 项：主题卡片 / 导入导出（GPUI token 主题，iOS 用系统视觉）、界面缩放与字体（由 Dynamic Type 取代）。
/// Android 的「跟随壁纸取色」「氛围光强度」在 iOS 没有对应平台能力 / 视觉元素，不提供。
struct AppearancePage: View {
    @Environment(AppContainer.self) private var container
    @Environment(AppearanceStore.self) private var appearance
    @Environment(\.openURL) private var openURL
    @Environment(\.colorScheme) private var colorScheme

    @State private var customDraft: Color = .accentColor
    @State private var customDirty = false
    @State private var customWrite: Task<Void, Never>?
    @State private var hexDraft = ""
    @State private var hexInvalid = false
    @FocusState private var hexFocused: Bool

    var body: some View {
        SettingsPage(title: L("settingsCatAppearance"), showsSyncLegend: true) {
            Section {
                ThemePreviewCard()
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }

            Section {
                Button(action: openLanguageSettings) {
                    LabeledContent {
                        HStack(spacing: 6) {
                            Text(L("languageNativeName"))
                            Image(systemName: FluxSymbol.openFile).font(.footnote)
                        }
                    } label: {
                        SettingsTileLabel(title: L("language"), symbol: "globe", color: .blue)
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityHint(L("mobileLanguageSystemHint"))
                .settingsRow("appearance.language")
            } footer: {
                Text(L("mobileLanguageSystemHint"))
            }

            Section {
                Picker(L("themeMode"), selection: modeBinding) {
                    Text(L("themeModeSystem")).tag(ThemeMode.system)
                    Text(L("themeModeLight")).tag(ThemeMode.light)
                    Text(L("themeModeDark")).tag(ThemeMode.dark)
                }
                .pickerStyle(.segmented)
                .settingsRow("appearance.mode")
            } header: {
                HStack(spacing: 5) {
                    Text(L("themeMode"))
                    SettingsSyncMark()
                }
            }

            Section {
                AccentSwatches(
                    scheme: appearance.scheme,
                    customColor: appearance.customColor,
                    onPreset: selectPreset,
                    onCustom: selectCustom
                )
                .settingsRow("appearance.color")

                if appearance.scheme == "custom" {
                    ColorPicker(L("colorCustom"), selection: $customDraft, supportsOpacity: false)
                        .onChange(of: customDraft) { _, new in customChanged(new) }

                    LabeledContent(L("mobileColorHex")) {
                        TextField(L("mobileColorHex"), text: $hexDraft, prompt: Text(verbatim: "RRGGBB"))
                            .font(.fluxMono)
                            .multilineTextAlignment(.trailing)
                            .textInputAutocapitalization(.characters)
                            .autocorrectionDisabled()
                            .submitLabel(.done)
                            .focused($hexFocused)
                            .onSubmit(commitHex)
                            .frame(minHeight: 44)
                    }
                    if hexInvalid {
                        Label(L("mobileHexInvalid"), systemImage: FluxSymbol.failure)
                            .font(.caption)
                            .foregroundStyle(Color.fdStatusFailedText)
                    }
                    if lowContrast {
                        Label(L("mobileColorLowContrast"), systemImage: FluxSymbol.warning)
                            .font(.footnote)
                            .foregroundStyle(Color.fdStatusWarningText)
                    }
                }
            } header: {
                HStack(spacing: 5) {
                    Text(L("themeColor"))
                    SettingsSyncMark()
                }
            }

            Section(L("settingsGroupInterface")) {
                // iOS 没有公开的「显示与亮度」深链；文字大小由系统（动态字体）接管，这里只做说明，不放无效按钮。
                SettingsTileLabel(
                    title: L("mobileSystemTextSize"),
                    subtitle: L("mobileSystemTextSizeDesc"),
                    symbol: "textformat.size",
                    color: .gray
                )
                .settingsRow("appearance.textSize")

                Picker(L("fileSizeUnits"), selection: fileSizeUnitsBinding) {
                    Text(L("fileSizeUnitsLegacy")).tag(FileSizeUnits.legacy)
                    Text(L("fileSizeUnitsIec")).tag(FileSizeUnits.iec)
                }
                .settingsRow("appearance.fileSizeUnits")
            } footer: {
                Text(L("fileSizeUnitsDesc"))
            }
        }
        .onAppear { syncCustomFromStore() }
        .onChange(of: appearance.customColor) { _, _ in if !customDirty { syncCustomFromStore() } }
        .onChange(of: appearance.scheme) { _, _ in syncCustomFromStore() }
        .onChange(of: hexFocused) { _, focused in if !focused { commitHex() } }
        .onDisappear { flushCustom() }
        .sensoryFeedback(FluxHaptic.selection.sensoryFeedback, trigger: appearance.mode)
        .sensoryFeedback(FluxHaptic.selection.sensoryFeedback, trigger: appearance.scheme)
    }

    // MARK: 绑定与动作

    private var modeBinding: Binding<ThemeMode> {
        Binding(get: { appearance.mode }, set: { newMode in
            appearance.mode = newMode
            AppearanceSync.shared.modeChanged(newMode)
        })
    }

    private var fileSizeUnitsBinding: Binding<FileSizeUnits> {
        Binding(get: { appearance.fileSizeUnits }, set: { container.setFileSizeUnits($0) })
    }

    private func selectPreset(_ id: String) {
        guard id != appearance.scheme else { return }
        flushCustom()
        appearance.scheme = id
        AppearanceSync.shared.accentChanged()
    }

    private func selectCustom() {
        guard appearance.scheme != "custom" else { return }
        // 取色器以持久化的自定义色初始化
        appearance.scheme = "custom"
        AppearanceSync.shared.accentChanged()
    }

    private func openLanguageSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        openURL(url) { accepted in
            if !accepted {
                container.toasts.show(text: L("mobileNoSettingsApp"), tone: .error)
            }
        }
    }

    // MARK: 自定义色

    private func syncCustomFromStore() {
        customDraft = Color(rgb: appearance.customColor)
        hexDraft = Self.hex(appearance.customColor)
        hexInvalid = false
    }

    /// 取色器每次变化 → 250ms 防抖写入；仍处于「自定义」时才写，避免覆盖随后点选的预设。
    private func customChanged(_ color: Color) {
        guard let rgb = color.rgb, rgb != appearance.customColor else { return }
        customDirty = true
        hexDraft = Self.hex(rgb)
        hexInvalid = false
        customWrite?.cancel()
        customWrite = Task {
            do {
                try await Task.sleep(for: ConfigEditor.debounce)
            } catch {
                return
            }
            flushCustom()
        }
    }

    private func flushCustom() {
        customWrite?.cancel()
        customWrite = nil
        guard customDirty else { return }
        customDirty = false
        if appearance.scheme == "custom", let rgb = customDraft.rgb, rgb != appearance.customColor {
            appearance.setCustom(rgb)
            AppearanceSync.shared.accentChanged()
        }
    }

    private func commitHex() {
        let cleaned = hexDraft.trimmingCharacters(in: CharacterSet(charactersIn: "# ").union(.whitespacesAndNewlines))
        guard cleaned.count == 6, let value = UInt32(cleaned, radix: 16) else {
            hexInvalid = !hexDraft.isEmpty && Self.hex(appearance.customColor) != hexDraft
            if hexInvalid { return }
            hexDraft = Self.hex(appearance.customColor)
            return
        }
        hexInvalid = false
        customWrite?.cancel()
        customDirty = false
        appearance.setCustom(value)
        AppearanceSync.shared.accentChanged()
        customDraft = Color(rgb: value)
        hexDraft = Self.hex(value)
    }

    /// 自定义色相对当前背景的对比度 < 3:1 → 提示（Android 同阈值）。
    private var lowContrast: Bool {
        let canvas = UIColor.systemGroupedBackground
            .resolvedColor(with: UITraitCollection(userInterfaceStyle: colorScheme == .dark ? .dark : .light))
        guard let background = canvas.rgb else { return false }
        return Contrast.ratio(appearance.customColor, background) < 3
    }

    static func hex(_ rgb: UInt32) -> String {
        String(format: "#%06X", rgb & 0xFFFFFF)
    }
}

extension AppearancePage {
    static func searchEntries(_ ctx: SettingsSearchContext) -> [SettingsEntry] {
        let name = L("settingsCatAppearance")
        return [
            SettingsEntry(
                id: "appearance.language", route: .appearance, title: L("language"), detail: L("languageDesc"),
                breadcrumb: name, symbol: "globe"
            ),
            SettingsEntry(
                id: "appearance.mode", route: .appearance, title: L("themeMode"), detail: "",
                breadcrumb: name, symbol: "paintpalette.fill"
            ),
            SettingsEntry(
                id: "appearance.color", route: .appearance, title: L("themeColor"), detail: "",
                breadcrumb: name, symbol: "paintpalette.fill"
            ),
            SettingsEntry(
                id: "appearance.textSize", route: .appearance, title: L("mobileSystemTextSize"),
                detail: L("mobileSystemTextSizeDesc"), breadcrumb: "\(name) › \(L("settingsGroupInterface"))",
                symbol: "textformat.size"
            ),
            SettingsEntry(
                id: "appearance.fileSizeUnits", route: .appearance, title: L("fileSizeUnits"),
                detail: L("fileSizeUnitsDesc"), breadcrumb: "\(name) › \(L("settingsGroupInterface"))",
                symbol: "internaldrive"
            ),
        ]
    }
}

// MARK: - 色点

/// 主题色点（§5.4）：预设 + 自定义；选中态外圈环 + 对勾，颜色以外还有对勾与 VoiceOver 色名。
private struct AccentSwatches: View {
    let scheme: String
    let customColor: UInt32
    let onPreset: (String) -> Void
    let onCustom: () -> Void

    @ScaledMetric(relativeTo: .body) private var side: CGFloat = 36

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 14) {
                ForEach(FluxAccent.presets) { accent in
                    swatch(
                        fill: accent.color,
                        selected: scheme == accent.id,
                        name: Self.name(accent.id),
                        content: nil
                    ) { onPreset(accent.id) }
                }
                swatch(
                    fill: Color(rgb: customColor),
                    selected: scheme == "custom",
                    name: L("colorCustom"),
                    content: scheme == "custom" ? nil : "paintpalette.fill"
                ) { onCustom() }
            }
            .padding(.vertical, 6)
            .padding(.horizontal, 4)
        }
        .scrollClipDisabled()
    }

    private func swatch(fill: Color, selected: Bool, name: String, content symbol: String?, action: @escaping () -> Void) -> some View {
        let dot = min(side, 52)
        return Button(action: action) {
            ZStack {
                Circle().fill(fill).frame(width: dot, height: dot)
                if selected {
                    Image(systemName: FluxSymbol.done)
                        .font(.system(size: dot * 0.42, weight: .bold))
                        .foregroundStyle(Color.fdOnColor(fill))
                        .transition(.scale.combined(with: .opacity))
                } else if let symbol {
                    Image(systemName: symbol)
                        .font(.system(size: dot * 0.42, weight: .medium))
                        .foregroundStyle(Color.fdOnColor(fill))
                }
            }
            .padding(4)
            .overlay {
                if selected {
                    Circle().stroke(fill, lineWidth: 3)
                }
            }
            .frame(minWidth: 44, minHeight: 44)
            .contentShape(.circle)
        }
        .buttonStyle(.fluxPress(scale: 0.9))
        .fluxAnimation(.bouncy, value: selected)
        .accessibilityLabel(name)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    static func name(_ id: String) -> String {
        switch id {
        case "blue": L("colorBlue")
        case "green": L("colorGreen")
        case "violet": L("colorViolet")
        case "rose": L("colorRose")
        case "orange": L("colorOrange")
        case "indigo": L("colorIndigo")
        default: L("colorCustom")
        }
    }
}

// MARK: - 预览卡

/// 实时预览卡（§5.1）：用中性控件展示当前强调色，随明暗 / 强调色实时重绘。内容层，不是玻璃；不含示意任务数据，
/// 整卡对 VoiceOver 只读出「预览」。
private struct ThemePreviewCard: View {
    @Environment(\.fluxAccent) private var accent

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "arrow.down.circle.fill")
                    .font(.largeTitle)
                    .foregroundStyle(accent.color)
                Text(L("mobilePreview")).font(.headline)
                Spacer(minLength: 8)
                Toggle(isOn: .constant(true)) { EmptyView() }
                    .labelsHidden()
                    .tint(accent.color)
                    .allowsHitTesting(false)
            }
            Capsule()
                .fill(accent.color)
                .frame(height: 6)
        }
        .padding(16)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: .rect(cornerRadius: 22, style: .continuous))
        .fluxAnimation(.smooth, value: accent)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(L("mobilePreview"))
    }
}
