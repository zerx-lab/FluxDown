import FluxDomain
import FluxUI
import SwiftUI

@main
struct FluxDownApp: App {
    @State private var container: AppContainer

    init() {
        let container = AppContainer()
        Format.setFileSizeUnits(container.appearance.fileSizeUnits)
        _container = State(initialValue: container)
        NotificationService.shared.attach(container: container)
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                // 挂在外观 / 强调色环境之内（外层 `.environment` / `.preferredColorScheme` 才对它生效）。
                // 112 = `Assets.xcassets/LaunchMark` 的点尺寸（logo 400 单位画框，`scripts/gen_icons.ts` 生成）。
                .launchReveal(markSide: 112)
                .backgroundTransfers()
                .settingsEffects()
                .environment(container)
                .environment(container.store)
                .environment(container.router)
                .environment(container.appearance)
                .environment(container.viewPrefs)
                .environment(container.toasts)
                .tint(container.appearance.accent.color)
                .environment(\.fluxAccent, container.appearance.accent)
                .preferredColorScheme(container.appearance.mode.colorScheme)
        }
    }
}
