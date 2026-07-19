// 站点分桶键/展示 label 提取 —— 移植自 lib/src/models/download_task.dart
// extractSiteKey/extractSiteLabel（注册域聚合，磁力/BT 归一为 `bt`）。
// 供视图系统「按站点分组」维度使用。

/** 非详尽 Public Suffix List——零外联纪律下的内置精简表。 */
const TWO_LEVEL_PUBLIC_SUFFIXES = new Set([
  'com.cn', 'net.cn', 'org.cn', 'gov.cn', 'edu.cn',
  'co.uk', 'org.uk', 'ac.uk', 'gov.uk',
  'com.au', 'net.au', 'org.au',
  'co.jp', 'ne.jp', 'or.jp',
  'co.kr', 'ne.kr',
  'com.hk', 'com.tw', 'com.sg', 'com.br',
])

/** 从 URL 识别的粗粒度协议 token（对齐桌面 protocolLabel 分类）。 */
function protocolToken(url: string): 'BT' | 'FTP' | 'ED2K' | 'HTTP' {
  const lower = url.toLowerCase()
  if (lower.startsWith('magnet:') || lower.startsWith('torrent-file://')) return 'BT'
  if (lower.startsWith('ftp://')) return 'FTP'
  if (lower.startsWith('ed2k://')) return 'ED2K'
  return 'HTTP'
}

/** 解析并规范化 host：小写化 + 去除 `www.` 前缀；解析失败/无 host 返回空串。 */
function normalizedHost(url: string): string {
  try {
    let host = new URL(url).hostname.toLowerCase()
    if (host.startsWith('www.')) host = host.slice(4)
    return host
  } catch {
    return ''
  }
}

/** 由规范化 host 推导注册域（去子域聚合，如 `pan.baidu.com`→`baidu.com`；`foo.bar.com.cn`→`bar.com.cn`）。 */
function registrableDomain(host: string): string {
  const labels = host.split('.')
  if (labels.length <= 2) return host
  const lastTwo = `${labels[labels.length - 2]}.${labels[labels.length - 1]}`
  if (TWO_LEVEL_PUBLIC_SUFFIXES.has(lastTwo) && labels.length >= 3) {
    return labels.slice(labels.length - 3).join('.')
  }
  return lastTwo
}

/** 从 URL 提取站点分桶键（注册域聚合；磁力/BT 协议归一为固定 `bt`；host 解析失败的其它
 *  协议回退为协议 token 小写形式，保证分桶键永不为空）。 */
export function extractSiteKey(url: string): string {
  if (protocolToken(url) === 'BT') return 'bt'
  const host = normalizedHost(url)
  if (!host) return protocolToken(url).toLowerCase()
  return registrableDomain(host)
}

/** 从 URL 提取站点展示 label：保留离用户最近的一级子域（如 `pan.baidu.com`），
 *  更深的子域链收敛掉；磁力/BT 显示为 `btLabel` 参数（唯一特例）。 */
export function extractSiteLabel(url: string, btLabel: string): string {
  if (protocolToken(url) === 'BT') return btLabel
  const host = normalizedHost(url)
  if (!host) return protocolToken(url)
  const registrable = registrableDomain(host)
  const hostLabels = host.split('.')
  const registrableLabels = registrable.split('.')
  if (hostLabels.length <= registrableLabels.length) return registrable
  const keepFrom = hostLabels.length - registrableLabels.length - 1
  return hostLabels.slice(keepFrom).join('.')
}
