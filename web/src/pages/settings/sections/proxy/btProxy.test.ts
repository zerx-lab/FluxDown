import { describe, expect, test } from 'bun:test'
import { btProxyHint } from './btProxy'

describe('btProxyHint（对齐 GPUI bt_proxy_hint）', () => {
  test('缺主机或端口优先', () => {
    expect(btProxyHint('socks5', '', '1080')).toBe('btProxyNoServer')
    expect(btProxyHint('https', 'proxy.lan', ' ')).toBe('btProxyNoServer')
  })
  test('HTTPS 不支持', () => {
    expect(btProxyHint('https', 'proxy.lan', '443')).toBe('btProxyUnsupportedType')
  })
  test('HTTP / SOCKS4 无法转发 UDP', () => {
    expect(btProxyHint('http', 'proxy.lan', '8080')).toBe('btProxyNoUdp')
    expect(btProxyHint('socks4', 'proxy.lan', '1080')).toBe('btProxyNoUdp')
  })
  test('SOCKS5 无提示', () => {
    expect(btProxyHint('socks5', 'proxy.lan', '1080')).toBeNull()
  })
})
