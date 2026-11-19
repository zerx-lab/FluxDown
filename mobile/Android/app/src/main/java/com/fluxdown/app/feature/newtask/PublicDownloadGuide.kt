package com.fluxdown.app.feature.newtask

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.fluxdown.app.R
import com.fluxdown.app.actions.LocalTaskActions
import com.fluxdown.app.data.DeviceSettings
import com.fluxdown.app.i18n.str
import com.fluxdown.app.shell.LocalAppContainer
import com.fluxdown.app.ui.defaultLocalSaveDir
import com.fluxdown.app.ui.publicDownloadSaveDir
import com.fluxdown.app.ui.rememberLocalDirectoryPicker
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.store.HostState
import com.fluxdown.fluxui.controls.ButtonVariant
import com.fluxdown.fluxui.controls.FluxButton
import com.fluxdown.fluxui.icons.FluxIcons
import com.fluxdown.fluxui.overlay.FluxBanner
import com.fluxdown.fluxui.overlay.FluxBannerKind
import com.fluxdown.fluxui.overlay.FluxToastKind
import com.fluxdown.fluxui.overlay.LocalFluxOverlays
import kotlinx.coroutines.launch

/** 仅针对仍使用应用专属默认目录的本机；与已有下载无关。 */
internal fun HostState.needsPublicDownloadGuide(host: HostRef, done: Boolean, appDir: String): Boolean =
    host is HostRef.Local && !isReadOnly && !done &&
        config["default_save_dir"]?.trim()?.trimEnd('/') == appDir.trim().trimEnd('/')

internal fun HostState.needsPublicDownloadGuide(context: Context, host: HostRef): Boolean =
    needsPublicDownloadGuide(host, DeviceSettings.of(context).downloadFolderGuideDone, defaultLocalSaveDir(context).path)

internal fun HostState.skipPublicDownloadGuide(host: HostRef, done: Boolean, appDir: String, finish: () -> Unit) {
    if (needsPublicDownloadGuide(host, done, appDir)) finish()
}

/** 选中可写目录后才修改默认值。跳过保留主机设置；权限拒绝或保存失败不结束引导。 */
@Composable
internal fun PublicDownloadGuide(
    enabled: Boolean,
    onSavingChange: (Boolean) -> Unit,
    onSaved: (String) -> Unit,
) {
    val context = LocalContext.current
    val container = LocalAppContainer.current
    val settings = remember(context) { DeviceSettings.of(context) }
    val publicDir = remember { publicDownloadSaveDir().path }
    val appDir = remember(context) { defaultLocalSaveDir(context).path }
    val overlays = LocalFluxOverlays.current
    val actions = LocalTaskActions.current
    val pickDirectory = rememberLocalDirectoryPicker { path ->
        val latest = container.store.state.value
        if (!enabled || !latest.needsPublicDownloadGuide(container.host.value, settings.downloadFolderGuideDone, appDir)) {
            return@rememberLocalDirectoryPicker
        }
        val session = container.session
        onSavingChange(true)
        container.appScope.launch {
            try {
                // 用检查过的 revision，不在冲突后重试覆盖用户另行设置的目录。
                session.patchConfig(latest.configRevision, mapOf("default_save_dir" to path))
                onSaved(path)
                settings.finishDownloadFolderGuide()
            } catch (e: HostException) {
                overlays.toast(actions.errorText(e), FluxToastKind.Error)
            } finally {
                onSavingChange(false)
            }
        }
    }
    FluxBanner(text = str(R.string.mobilePublicDownloadGuide), kind = FluxBannerKind.Info)
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        FluxButton(
            str(R.string.mobileUsePublicDownloadDir), { pickDirectory(publicDir) },
            enabled = enabled, icon = FluxIcons.FolderOpen,
        )
        FluxButton(
            str(R.string.mobileSkipPublicDownloadGuide), {
                if (enabled) {
                    container.store.state.value.skipPublicDownloadGuide(
                        container.host.value, settings.downloadFolderGuideDone, appDir, settings::finishDownloadFolderGuide,
                    )
                }
            },
            enabled = enabled, variant = ButtonVariant.Ghost,
        )
    }
}
