import FluxDomain
import SwiftUI
import UIKit

/// 设置带来的根级副作用（挂在根视图，与当前停留在哪个页面无关）：
/// - 外观：主机偏好 ↔ `AppearanceStore`（`AppearanceSync`）；
/// - 下载期间保持屏幕常亮（`download.keep_awake`，仅本机引擎且前台时）。
///
/// 用法：根视图 `.settingsEffects()`（与 `.backgroundTransfers()` 同级）。
@MainActor
final class SettingsEffects {
    static let shared = SettingsEffects()

    private weak var container: AppContainer?

    private init() {}

    /// 接入应用容器（幂等）。
    func attach(container: AppContainer) {
        guard self.container == nil else { return }
        self.container = container
        AppearanceSync.shared.attach(container: container)
    }

    /// 下载期间保持常亮：偏好开启、当前主机是本机（手机自己在下载）、前台、且确有活跃下载。
    /// `isIdleTimerDisabled` 只在前台有意义，进入后台即还原。
    static func keepAwakeWanted(
        preferences: AgentPreferencesDto, isLocalHost: Bool, hasActiveDownloads: Bool, isForeground: Bool
    ) -> Bool {
        preferences.bool("download.keep_awake", default: false) && isLocalHost && hasActiveDownloads && isForeground
    }
}

private struct SettingsEffectsModifier: ViewModifier {
    @Environment(AppContainer.self) private var container
    @Environment(\.scenePhase) private var scenePhase

    func body(content: Content) -> some View {
        let state = container.store.state
        let keepAwake = SettingsEffects.keepAwakeWanted(
            preferences: state.preferences,
            isLocalHost: container.isLocalHost,
            // 任务表分页载入中 `tasks` 是空占位，活跃数改取运行时统计。
            hasActiveDownloads: state.tasksLoading ? state.stats.activeTasks > 0 : state.tasks.contains { $0.status.isActive },
            isForeground: scenePhase == .active
        )
        content
            .task {
                SettingsEffects.shared.attach(container: container)
                AppearanceSync.shared.refresh()
            }
            .onChange(of: state.sections[HostSection.agentPreferences]) { AppearanceSync.shared.refresh() }
            .onChange(of: state.connection == .live) { AppearanceSync.shared.refresh() }
            .onChange(of: container.host.id) { AppearanceSync.shared.refresh() }
            .onChange(of: keepAwake, initial: true) { _, wanted in
                UIApplication.shared.isIdleTimerDisabled = wanted
            }
    }
}

extension View {
    /// 安装设置的根级副作用（外观同步 / 保持常亮）。
    func settingsEffects() -> some View {
        return modifier(SettingsEffectsModifier())
    }
}
