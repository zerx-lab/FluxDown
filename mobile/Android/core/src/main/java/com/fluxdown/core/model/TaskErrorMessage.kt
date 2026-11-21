package com.fluxdown.core.model

/**
 * 引擎产生的稳定任务错误文案；`:app` 将 [i18nKey] 映射到 `R.string.<i18nKey>` 本地化展示。
 *
 * 字面量镜像引擎的持久化任务错误；改动引擎文案须同步此处。
 */
enum class TaskErrorMessage(val raw: String, val i18nKey: String) {
    PostOnlyEndpoint(
        "This link only accepts POST requests (HTTP 405); start the download from the browser",
        "taskErrorPostOnlyEndpoint",
    ),
    PostOnlyResume(
        "This task was started by a browser POST request and cannot be resumed; start it again from the browser",
        "taskErrorPostOnlyResume",
    ),
    UnexpectedHtml(
        "server returned HTML page (Content-Type: ",
        "taskErrorUnexpectedHtml",
    ),
    HtmlChanged(
        "the server did not return a complete HTML page; nothing was saved",
        "taskErrorHtmlChanged",
    ),
    MagnetMetadataTimeout(
        "magnet metadata resolution took too long (300s) — no peers/DHT response; check trackers or network",
        "taskErrorMagnetMetadataTimeout",
    ),
    ;

    companion object {
        /** HTML 诊断带动态详情，按前缀匹配；其余文案整串匹配。 */
        fun match(raw: String): TaskErrorMessage? {
            val s = raw.trim()
            if (s.startsWith(UnexpectedHtml.raw)) return UnexpectedHtml
            return entries.firstOrNull { it.raw == s }
        }
    }
}
