import { describe, expect, test } from 'bun:test'
import { interpolate, lookup, nativeName, resolveLocale, translate } from './catalog'

describe('i18n catalog', () => {
  test('locale 解析：精确、主语言、回退', () => {
    expect(resolveLocale('zh')).toBe('zh')
    expect(resolveLocale('zh_CN')).toBe('zh')
    expect(resolveLocale('zh_TW')).toBe('zh-tw')
    expect(resolveLocale('zh-Hant-TW')).toBe('zh-tw')
    expect(resolveLocale('zh-HK')).toBe('zh-tw')
    expect(resolveLocale('EN-us')).toBe('en')
    expect(resolveLocale('sv-SE')).toBe('en')
    expect(resolveLocale('pt-PT')).toBe('pt-br')
    expect(resolveLocale('in-ID')).toBe('id')
    expect(resolveLocale('ja-JP')).toBe('ja')
  })

  test('中文显式文字优先于地区，地区可带 Unicode 扩展', () => {
    for (const locale of [
      'zh_TW', 'zh-Hant', 'zh-Hant-TW', 'zh-Hant-CN', 'zh-HK', 'zh-MO',
      'zh-TW-u-nu-hanidec', 'zh-HK-u-nu-hanidec', ' ZH_hant_hk ',
    ]) {
      expect(resolveLocale(locale)).toBe('zh-tw')
    }
    for (const locale of ['zh', 'zh-CN', 'zh-SG', 'zh-Hans', 'zh-Hans-TW', 'zh-hantfoo']) {
      expect(resolveLocale(locale)).toBe('zh')
    }
  })

  test('占位插值：全量替换、未知占位保留、数字参数', () => {
    expect(interpolate('{n} of {n} / {m}', { n: 3 })).toBe('3 of 3 / {m}')
    expect(interpolate('Connected, latency {ms} ms', { ms: 12 })).toBe('Connected, latency 12 ms')
    expect(interpolate('no params')).toBe('no params')
  })

  test('缺键回退键名，中文缺失键回退英文', () => {
    expect(lookup('en', '__missing_key__')).toBe('__missing_key__')
    expect(translate('zh', 'proxyTestSuccess', { ms: 5 })).toBe(
      translate('zh', 'proxyTestSuccess').replaceAll('{ms}', '5'),
    )
    expect(nativeName('en')).toBe('English')
  })
})
