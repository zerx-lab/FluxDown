// 导出文件的浏览器侧保存：云端生成内容（Markdown / JSON），这里只负责 Blob 下载。
// 文件名来自云端（已去掉路径分隔符等不安全字符），仍再兜一层：只保留最后一段、去掉控制字符。

import type { AssistantExportResult } from '../../lib/rpc'

/** `a[download]` 用的安全文件名：取最后一段路径、去控制字符与首尾空白，空则回退。 */
export function safeFileName(fileName: string, fallback: string): string {
  const last = fileName.split(/[\\/]/).pop() ?? ''
  // eslint-disable-next-line no-control-regex
  const cleaned = last.replace(/[\u0000-\u001f\u007f]/g, '').trim()
  return cleaned === '' || cleaned === '.' || cleaned === '..' ? fallback : cleaned
}

/** 把导出结果存成本地文件（触发浏览器下载）。 */
export function saveExport(result: AssistantExportResult): void {
  const url = URL.createObjectURL(new Blob([result.content], { type: `${result.mime};charset=utf-8` }))
  let anchor: HTMLAnchorElement | undefined
  try {
    anchor = document.createElement('a')
    anchor.href = url
    anchor.download = safeFileName(result.fileName, 'assistant-chat.txt')
    anchor.setAttribute('data-fluxdown-skip', '1')
    document.body.append(anchor)
    anchor.click()
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 10_000)
    anchor?.remove()
  }
}
