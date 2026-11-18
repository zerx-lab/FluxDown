// 剪贴板写入的统一出口。navigator.clipboard 仅在安全上下文（HTTPS / localhost）
// 暴露，NAS/Docker 面板经明文 HTTP + 局域网 IP 访问时整个对象为 undefined，直接
// 调用会抛 TypeError。此处优先走 Clipboard API，不可用或被拒时回退到隐藏
// textarea + execCommand('copy')（已废弃但各浏览器仍支持，且无安全上下文限制）。

function execCommandCopy(value: string): boolean {
  const textarea = document.createElement('textarea')
  textarea.value = value
  // 防滚动跳动/闪现：固定定位移出视口，只读避免移动端弹键盘
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.top = '-9999px'
  document.body.appendChild(textarea)
  textarea.select()
  let ok = false
  try {
    ok = document.execCommand('copy')
  } catch {
    ok = false
  }
  textarea.remove()
  return ok
}

/** 将文本写入剪贴板，返回 Clipboard API 或兼容写入是否成功。 */
export async function copyText(value: string): Promise<boolean> {
  if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(value)
      return true
    } catch {
      return execCommandCopy(value)
    }
  }
  return execCommandCopy(value)
}
