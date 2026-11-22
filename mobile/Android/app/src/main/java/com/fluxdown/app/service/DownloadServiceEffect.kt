package com.fluxdown.app.service

import android.Manifest
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.fluxdown.app.shell.LocalAppContainer
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/**
 * 观察「本机引擎有活跃 / 排队 / 待重试任务」（与当前选中的主机无关）并在应用前台时拉起 [DownloadService]；
 * 停止由服务自己决定（本机空闲）。
 *
 * - 只在 STARTED 期间收集：Android 12+ 不允许后台启动前台服务；回到前台会重新评估（也覆盖 Android 15
 *   dataSync 6 小时超时后的重新拉起）。
 * - 首次出现下载活动时（Android 13+，且从未请求过）请求 POST_NOTIFICATIONS；拒绝不影响服务运行。
 */
@Composable
fun DownloadServiceEffect() {
    val container = LocalAppContainer.current
    val context = LocalContext.current.applicationContext
    val lifecycleOwner = LocalLifecycleOwner.current
    val requestNotifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        DownloadServiceController.onNotificationPermissionResult(context)
    }

    LaunchedEffect(container, lifecycleOwner) {
        lifecycleOwner.repeatOnLifecycle(Lifecycle.State.STARTED) {
            container.localActivity
                .map { it.busy }
                .distinctUntilChanged()
                .collect { busy ->
                    if (!busy) return@collect
                    DownloadServiceController.start(context, "foreground")
                    if (DownloadServiceController.shouldRequestNotificationPermission(context)) {
                        DownloadServiceController.markNotificationPermissionAsked(context)
                        requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                    }
                }
        }
    }
}
