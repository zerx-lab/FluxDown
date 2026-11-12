import { describe, expect, test } from 'bun:test'
import { btProxyHint } from './btProxy'

describe('btProxyHint（对齐 GPUI bt_proxy_hint）', () => {
  const manual = (type: string, host: string, port: string) => btProxyHint('manual', type, host, port)
  test('缺主机或端口优先', () => {
    expect(manual('socks5', '', '1080')).toBe('btProxyNoServer')
    expect(manual('https', 'proxy.lan', ' ')).toBe('btProxyNoServer')
  })
  test('HTTPS 不支持', () => {
    expect(manual('https', 'proxy.lan', '443')).toBe('btProxyUnsupportedType')
  })
  test('HTTP / SOCKS4 无法转发 UDP', () => {
    expect(manual('http', 'proxy.lan', '8080')).toBe('btProxyNoUdp')
    expect(manual('socks4', 'proxy.lan', '1080')).toBe('btProxyNoUdp')
  })
  test('SOCKS5 无提示', () => {
    expect(manual('socks5', 'proxy.lan', '1080')).toBeNull()
  })
  test('系统代理来源与手动服务器无关', () => {
    expect(btProxyHint('system', 'https', '', '')).toBe('btProxySystemNote')
    expect(btProxyHint('system', 'socks5', 'proxy.lan', '1080')).toBe('btProxySystemNote')
  })
})
