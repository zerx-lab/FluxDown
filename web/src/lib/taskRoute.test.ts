import { describe, expect, test } from 'bun:test'
import { routeLabel } from './taskRoute'

const t = (key: string) => `<${key}>`

describe('routeLabel', () => {
  test('rule labels match exactly before suffix split', () => {
    expect(routeLabel(t, 'rule:direct')).toBe('<taskRouteRuleDirect>')
    expect(routeLabel(t, 'rule:system')).toBe('<taskRouteRuleSystem>')
    expect(routeLabel(t, 'rule:manual')).toBe('<taskRouteRuleManual>')
    expect(routeLabel(t, 'rule:custom')).toBe('<taskRouteRuleCustom>')
  })
  test('global, task and BT labels match exactly before suffix split', () => {
    expect(routeLabel(t, 'global:manual')).toBe('<taskRouteGlobalManual>')
    expect(routeLabel(t, 'global:system')).toBe('<taskRouteGlobalSystem>')
    expect(routeLabel(t, 'global:bypass')).toBe('<taskRouteGlobalBypass>')
    expect(routeLabel(t, 'task:proxy')).toBe('<taskRouteTaskProxy>')
    expect(routeLabel(t, 'bt:trackers:manual')).toBe('<taskRouteBtTrackersManual>')
    expect(routeLabel(t, 'bt:all:system')).toBe('<taskRouteBtAllSystem>')
  })
  test('auto labels with via suffix', () => {
    expect(routeLabel(t, 'proxy:cached:system')).toBe('<taskRouteProxyCached> · <taskRouteViaSystem>')
    expect(routeLabel(t, 'direct:manual')).toBe('<taskRouteDirect> · <taskRouteViaManual>')
    expect(routeLabel(t, 'direct:sampled')).toBe('<taskRouteDirectSampled>')
  })
  test('unknown returns as is', () => {
    expect(routeLabel(t, 'weird')).toBe('weird')
    expect(routeLabel(t, 'weird:system')).toBe('weird:system · <taskRouteViaSystem>')
  })
})
