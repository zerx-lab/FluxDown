import SwiftUI
#if FULL_FLAVOR

/// 设置页页签选择（BT：常规 · Tracker · 做种 · 防吸血；eD2K：常规 · 服务器）。
/// 常规字号下是分段控件（列表首行，无行底）；辅助功能字号下换成菜单，避免标题被截断。
struct BtSettingsTabPicker<Tab: Hashable & Identifiable & CaseIterable>: View where Tab.AllCases: RandomAccessCollection {
    let title: String
    @Binding var selection: Tab
    let label: (Tab) -> String

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        Section {
            if typeSize.isAccessibilitySize {
                Picker(title, selection: $selection) { options }
                    .pickerStyle(.menu)
            } else {
                Picker(title, selection: $selection) { options }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .controlSize(.large)
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
            }
        }
    }

    private var options: some View {
        ForEach(Tab.allCases) { tab in
            Text(label(tab)).tag(tab)
        }
    }
}
#endif
