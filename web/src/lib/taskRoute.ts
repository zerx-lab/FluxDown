// 任务链路标签 → 文案（Auto 路由、站点规则、全局 / 任务级代理与 BT 会话出口）；未知值原样返回。

import type { TFunction } from '../i18n'

const EXACT_KEYS: Readonly<Record<string, string>> = {
  'rule:direct': 'taskRouteRuleDirect',
  'rule:system': 'taskRouteRuleSystem',
  'rule:manual': 'taskRouteRuleManual',
  'rule:custom': 'taskRouteRuleCustom',
  'global:direct': 'taskRouteGlobalDirect',
  'global:bypass': 'taskRouteGlobalBypass',
  'global:manual': 'taskRouteGlobalManual',
  'global:system': 'taskRouteGlobalSystem',
  'global:pac': 'taskRouteGlobalPac',
  'task:proxy': 'taskRouteTaskProxy',
  'task:direct': 'taskRouteTaskDirect',
  'bt:direct': 'taskRouteBtDirect',
  'bt:trackers:manual': 'taskRouteBtTrackersManual',
  'bt:trackers:system': 'taskRouteBtTrackersSystem',
  'bt:all:manual': 'taskRouteBtAllManual',
  'bt:all:system': 'taskRouteBtAllSystem',
}

const BASE_KEYS: Readonly<Record<string, string>> = {
  direct: 'taskRouteDirect',
  'direct:sampled': 'taskRouteDirectSampled',
  'direct:pinned': 'taskRouteDirectPinned',
  'direct:failover': 'taskRouteDirectFailover',
  'proxy:cached': 'taskRouteProxyCached',
  'proxy:sampled': 'taskRouteProxySampled',
  'proxy:failover': 'taskRouteProxyFailover',
}

export function routeLabel(t: TFunction, route: string): string {
  // 整串精确匹配必须先于后缀拆分，否则 `rule:system`、`bt:all:manual` 会被误拆。
  const exactKey = EXACT_KEYS[route]
  if (exactKey) return t(exactKey)
  let base = route
  let via: string | null = null
  if (route.endsWith(':system')) {
    base = route.slice(0, -':system'.length)
    via = t('taskRouteViaSystem')
  } else if (route.endsWith(':manual')) {
    base = route.slice(0, -':manual'.length)
    via = t('taskRouteViaManual')
  }
  const key = BASE_KEYS[base]
  const label = key ? t(key) : route
  return via === null ? label : `${label} · ${via}`
}
