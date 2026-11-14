package com.fluxdown.core.protocol

/*
 * eD2K 本机网络状态（`daemon.ed2k.idCheck` → `Ed2kNetworkDto`，镜像 `native/protocol/src/daemon.rs`）。
 * 移动端只在设置页「ID 检测」行消费；可缺省字段取默认值时不上线，反序列化宽松。
 */

enum class Ed2kIdKind(val wire: String) {
    None("none"), Low("low"), High("high");

    companion object {
        fun fromWire(v: String): Ed2kIdKind = entries.firstOrNull { it.wire == v } ?: None
    }
}

enum class Ed2kUpnpState(val wire: String) {
    Disabled("disabled"), Pending("pending"), Mapped("mapped"), Failed("failed");

    companion object {
        fun fromWire(v: String): Ed2kUpnpState = entries.firstOrNull { it.wire == v } ?: Disabled
    }
}

/** `Ed2kNetworkDto` 中 ID 检测用到的字段。 */
data class Ed2kNetworkDto(
    val id: Ed2kIdKind = Ed2kIdKind.None,
    /** `host:port`；空 = 未登录。 */
    val server: String = "",
    /** 0 = 监听器未启动。 */
    val listenPort: Long = 0,
    val upnp: Ed2kUpnpState = Ed2kUpnpState.Disabled,
    /** 仅 `upnp == failed` 时非空：映射器给出的原因（英文原文）。 */
    val upnpError: String = "",
    /** 仅 idCheck 未能登录任何服务器时非空。 */
    val error: String = "",
) {
    companion object {
        fun fromJson(v: JsonValue?): Ed2kNetworkDto = Ed2kNetworkDto(
            id = Ed2kIdKind.fromWire(v.str("id")),
            server = v.str("server"),
            listenPort = v.long("listenPort"),
            upnp = Ed2kUpnpState.fromWire(v.str("upnp")),
            upnpError = v.str("upnpError"),
            error = v.str("error"),
        )
    }
}

/** ID 检测结果文案所需的已本地化片段（app 层按语言解析后传入）。 */
class Ed2kIdCheckTexts(
    val idLabel: (Ed2kIdKind) -> String,
    val upnpLabel: (Ed2kUpnpState) -> String,
    /** `ed2kIdCheckResult{id, server, port, upnp}`。 */
    val result: (id: String, server: String, port: String, upnp: String) -> String,
    /** `ed2kIdLowHint`。 */
    val lowHint: String,
    /** `ed2kIdCheckFailed`。 */
    val failed: String,
)

object Ed2kIdCheckFormat {
    private const val EMPTY = "—"

    /**
     * 设置页 ID 检测行的结果文本（契约 §4）：`id != none` → 结果行（`low` 时再跟一行 LowID 提示）；
     * `id == none` → 失败文案，有 `error` 时附上。空服务器 / 端口 0 显示 `—`。
     */
    fun text(net: Ed2kNetworkDto, t: Ed2kIdCheckTexts): String {
        if (net.id == Ed2kIdKind.None) return failed(net.error, t)
        val line = t.result(
            t.idLabel(net.id),
            net.server.ifBlank { EMPTY },
            if (net.listenPort > 0) net.listenPort.toString() else EMPTY,
            upnpText(net, t),
        )
        return if (net.id == Ed2kIdKind.Low) "$line\n${t.lowHint}" else line
    }

    /** UPnP 状态文本：失败且有原因 → `失败: 原因`（与 BT 端口映射「状态: 原因」一致），否则仅状态。 */
    fun upnpText(net: Ed2kNetworkDto, t: Ed2kIdCheckTexts): String {
        val label = t.upnpLabel(net.upnp)
        val reason = net.upnpError.trim()
        return if (net.upnp == Ed2kUpnpState.Failed && reason.isNotEmpty()) "$label: $reason" else label
    }

    /** 调用失败或未登录：失败文案 + 可选原因。 */
    fun failed(detail: String, t: Ed2kIdCheckTexts): String =
        if (detail.isBlank()) t.failed else "${t.failed}: $detail"
}
