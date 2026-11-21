/** 镜像 `native/engine/src/downloader.rs` 的 `POST_ONLY_ENDPOINT_MESSAGE`；改动需两边同步。 */
export const POST_ONLY_ENDPOINT_MESSAGE = 'This link only accepts POST requests (HTTP 405); start the download from the browser'

/** 镜像 `native/engine/src/downloader.rs` 的 `POST_ONLY_RESUME_MESSAGE`；改动需两边同步。 */
export const POST_ONLY_RESUME_MESSAGE = 'This task was started by a browser POST request and cannot be resumed; start it again from the browser'

/** 镜像 `native/engine/src/downloader.rs` 的 `UNEXPECTED_HTML_PREFIX`（前缀匹配，后接 Content-Type 等动态内容）。 */
export const UNEXPECTED_HTML_PREFIX = 'server returned HTML page (Content-Type: '

/** 镜像 `native/engine/src/downloader.rs` 的 `HTML_RESPONSE_CHANGED_MESSAGE`。 */
export const HTML_RESPONSE_CHANGED_MESSAGE = 'the server did not return a complete HTML page; nothing was saved'

// Mirrors the terminal error persisted by bt_downloader.rs.
const MAGNET_METADATA_TIMEOUT_MESSAGE = 'magnet metadata resolution took too long (300s) — no peers/DHT response; check trackers or network'

/** 引擎稳定错误消息 → i18n key：意外 HTML 按前缀匹配，其余按 trim 后的完整消息匹配。 */
export function taskErrorKey(message: string): string | null {
  const text = message.trim()
  if (text.startsWith(UNEXPECTED_HTML_PREFIX)) return 'taskErrorUnexpectedHtml'
  switch (text) {
    case POST_ONLY_ENDPOINT_MESSAGE:
      return 'taskErrorPostOnlyEndpoint'
    case POST_ONLY_RESUME_MESSAGE:
      return 'taskErrorPostOnlyResume'
    case HTML_RESPONSE_CHANGED_MESSAGE:
      return 'taskErrorHtmlChanged'
    case MAGNET_METADATA_TIMEOUT_MESSAGE:
      return 'taskErrorMagnetMetadataTimeout'
    default:
      return null
  }
}

/** 展示用：已知引擎消息本地化，其余原样返回。 */
export function localizeTaskError(t: (key: string) => string, message: string): string {
  const key = taskErrorKey(message)
  return key === null ? message : t(key)
}
