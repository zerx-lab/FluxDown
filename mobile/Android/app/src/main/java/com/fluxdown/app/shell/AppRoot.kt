package com.fluxdown.app.shell

import android.view.ViewTreeObserver
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.ui.platform.LocalView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.fluxdown.app.AppContainer
import com.fluxdown.app.data.AppearanceState
import com.fluxdown.app.data.ThemeMode
import com.fluxdown.app.nav.AppNavigator
import com.fluxdown.app.nav.LocalNavigator
import com.fluxdown.fluxui.theme.FluxTheme

internal val LocalAppearance = compositionLocalOf<AppearanceState> { error("AppearanceState missing") }

/** 各 Activity 的组合根：外观偏好 → [FluxTheme]，并注入进程级容器与该窗口自己的导航状态。 */
@Composable
fun FluxAppRoot(container: AppContainer, navigator: AppNavigator, content: @Composable () -> Unit) {
    val appearance = container.appearanceState.collectAsStateWithLifecycle().value
    if (appearance == null) {
        // Keep the starting window until Compose applies the first frame with loaded preferences.
        val view = LocalView.current
        DisposableEffect(view) {
            val listener = ViewTreeObserver.OnPreDrawListener { false }
            val observer = view.viewTreeObserver
            observer.addOnPreDrawListener(listener)
            onDispose {
                val current = if (observer.isAlive) observer else view.viewTreeObserver
                current.removeOnPreDrawListener(listener)
            }
        }
        return
    }
    val dark = when (appearance.mode) {
        ThemeMode.System -> isSystemInDarkTheme()
        ThemeMode.Dark -> true
        ThemeMode.Light -> false
    }
    FluxTheme(dark = dark, accent = appearance.accent) {
        CompositionLocalProvider(
            LocalAppContainer provides container,
            LocalNavigator provides navigator,
            LocalAppearance provides appearance,
            content = content,
        )
    }
}
