// i18n 提供者：语言来源优先级 = agent 偏好 `general.locale`（`system`/缺省 → 继续往下）
// → `/ping` 的 `language`（FLUXDOWN_LANG）→ 浏览器语言。首屏/登录页用本地缓存 + /ping。

import { useEffect, useLayoutEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { fetchPing } from '../lib/access/setup'
import { useAgentSnapshot, usePref } from '../lib/rpc/hooks'
import { FALLBACK_LOCALE, resolveLocale, translate } from './catalog'
import { I18nContext, LANGUAGES, LOCALE_PREF_KEY, setCurrentLocale } from './runtime'
import type { I18nContextValue } from './runtime'

const CACHE_KEY = 'fluxdown.web.locale'

/** 浏览器语言列表里第一个有译文的语言；英文排在前面就是英文，都不支持回退英文。 */
function browserLocale(): string {
  const tags = navigator.languages?.length ? navigator.languages : [navigator.language ?? 'en']
  for (const tag of tags) {
    const resolved = resolveLocale(tag)
    if (resolved !== FALLBACK_LOCALE || tag.toLowerCase().startsWith('en')) return resolved
  }
  return FALLBACK_LOCALE
}

function readCache(): string {
  try {
    return localStorage.getItem(CACHE_KEY) ?? ''
  } catch {
    return ''
  }
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const ready = useAgentSnapshot() !== null
  const pref = usePref<unknown>(LOCALE_PREF_KEY)
  const [cached] = useState(readCache)
  const [pingLanguage, setPingLanguage] = useState('')

  useEffect(() => {
    const controller = new AbortController()
    void fetchPing(controller.signal).then((info) => {
      if (info?.language) setPingLanguage(info.language)
    })
    return () => controller.abort()
  }, [])

  const explicit = typeof pref === 'string' && pref !== '' && pref !== 'system' ? pref : ''
  // 已连接：以偏好为准；未连接：用上次缓存。
  const source = ready ? explicit : cached
  const locale = resolveLocale(source || pingLanguage || browserLocale())
  useLayoutEffect(() => {
    setCurrentLocale(locale)
  }, [locale])

  useEffect(() => {
    document.documentElement.lang = locale
    if (!ready) return
    try {
      localStorage.setItem(CACHE_KEY, explicit)
    } catch {
      // 忽略存储不可用。
    }
  }, [locale, ready, explicit])

  const value = useMemo<I18nContextValue>(
    () => ({ locale, t: (key, params) => translate(locale, key, params), languages: LANGUAGES }),
    [locale],
  )
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>
}
