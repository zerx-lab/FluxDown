/**
 * BT 代理提示（i18n 键；GPUI `sections/proxy.rs::bt_proxy_hint`）：未配置服务器优先，
 * 其次是 BT 不支持 / 无法转发 UDP 的类型；SOCKS5 无提示。
 */
export function btProxyHint(proxyType: string, host: string, port: string): string | null {
  if (host.trim() === '' || port.trim() === '') return 'btProxyNoServer'
  switch (proxyType) {
    case 'https':
      return 'btProxyUnsupportedType'
    case 'http':
    case 'socks4':
      return 'btProxyNoUdp'
    default:
      return null
  }
}
