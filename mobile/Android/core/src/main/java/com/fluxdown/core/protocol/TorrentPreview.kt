package com.fluxdown.core.protocol

import com.fluxdown.core.model.BtFile

/** 镜像 `native/protocol/src/torrent_preview.rs`：只有 wire 类型，不含平台交互与 P2P 界面。 */
data class MagnetPreviewParams(val url: String) {
    fun toJson(): JsonValue = jsonObject("url" to url)
}

data class MagnetPreviewTicket(val previewId: String) {
    fun toJson(): JsonValue = jsonObject("previewId" to previewId)
    companion object {
        fun fromJson(value: JsonValue?): MagnetPreviewTicket? = value.strOrNull("previewId")?.let(::MagnetPreviewTicket)
    }
}

data class TorrentFilePreviewParams(val path: String) {
    fun toJson(): JsonValue = jsonObject("path" to path)
}

data class TorrentPreviewParams(val torrentBlobId: String) {
    fun toJson(): JsonValue = jsonObject("torrentBlobId" to torrentBlobId)
}

data class TorrentPreviewDto(val torrentBlobId: String, val name: String, val totalBytes: Long, val files: List<BtFile>) {
    companion object {
        fun fromJson(value: JsonValue?): TorrentPreviewDto? {
            val id = value.strOrNull("torrentBlobId") ?: return null
            return TorrentPreviewDto(id, value.str("name"), value.long("totalBytes"), value.list("files").map {
                BtFile(it.int("index"), it.str("path"), it.long("size"))
            })
        }
    }
}

data class TorrentConfirmParams(
    val torrentBlobId: String,
    val saveDir: String,
    val selectedFileIndices: List<Int>,
    val queueId: String = "",
    val startPaused: Boolean = false,
    val customName: String = "",
    val sourceUrl: String = "",
    /** 完整的 CreateTaskRequest 草稿；与上面的显式确认字段冲突时以显式字段为准。 */
    val options: JsonValue.Obj? = null,
) {
    fun toJson(): JsonValue = jsonObjectOmitNulls(
        "torrentBlobId" to torrentBlobId, "saveDir" to saveDir,
        "selectedFileIndices" to selectedFileIndices, "queueId" to queueId,
        "startPaused" to startPaused, "customName" to customName,
        "sourceUrl" to sourceUrl, "options" to options,
    )
}
