// 剪贴板写入的统一出口。navigator.clipboard 仅在安全上下文（HTTPS / localhost）
// 暴露，NAS/Docker 面板经明文 HTTP + 局域网 IP 访问时整个对象为 undefined，直接
// 调用会抛 TypeError。无 API 时同步走 textarea + execCommand('copy')；
// API 拒绝后仍尝试兼容写入，但浏览器可能已不允许（返回 false）。

function execCommandCopy(value: string): boolean {
  try {
    const textarea = document.createElement('textarea')
    textarea.value = value
    textarea.setAttribute('readonly', '')
    textarea.style.position = 'fixed'
    textarea.style.top = '-9999px'
    try {
      document.body.appendChild(textarea)
      textarea.select()
      return document.execCommand('copy')
    } finally {
      textarea.remove()
    }
  } catch {
    return false
  }
}

/** 将文本写入剪贴板，返回 Clipboard API 或兼容写入是否成功。 */
export async function copyText(value: string): Promise<boolean> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value)
      return true
    }
  } catch {
    // Rejection or an unavailable API: attempt the legacy path.
  }
  return execCommandCopy(value)
}
