// 「复制诊断信息」的纯文本（格式与 GPUI `crates/settings/src/version_button.rs::diagnostic_text` 一致），可直接贴进问题反馈。
// 只含版本、渠道、协议、主机安装形态、客户端与界面语言——绝不含路径、账户、设备名或下载地址。
// Web 拿不到主机的操作系统 / 架构（握手与快照都不带），以安装形态（docker / synology / serverBinary…）代替。

import type { UpdateStatusDto } from './rpc'

export interface DiagnosticInput {
  /** 握手 `serviceVersion`（Web SPA 内嵌在 agent 里，二者同版本）。 */
  serviceVersion: string
  protocolVersion: number | null
  status: UpdateStatusDto | null
  locale: string
  userAgent: string
}

export function diagnosticText({ serviceVersion, protocolVersion, status, locale, userAgent }: DiagnosticInput): string {
  const version = serviceVersion || status?.currentVersion || '?'
  const channel = status?.channel ? ` (${status.channel})` : ''
  const lines = [`FluxDown v${version}${channel}`]
  if (protocolVersion != null) lines.push(`Protocol: ${protocolVersion}`)
  lines.push(`Host: ${status?.installKind ?? 'unknown'}`)
  lines.push(`Client: web · ${userAgent || 'unknown'}`)
  lines.push(`Locale: ${locale}`)
  if (status?.hasUpdate && status.latestVersion) lines.push(`Latest: v${status.latestVersion}`)
  return lines.join('\n')
}
