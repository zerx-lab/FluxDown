import { beforeEach, describe, expect, test } from 'bun:test'
import {
  clampPanelWidth,
  closeAssistantPanel,
  getAssistantPanel,
  isAssistantShortcut,
  openAssistantPanel,
  resetAssistantPanel,
  setAssistantPanelWidth,
  showAssistantChat,
  showAssistantHistory,
  toggleAssistantPanel,
} from './assistantPanelStore'

const key = (init: Partial<Parameters<typeof isAssistantShortcut>[0]>) => ({ key: 'k', metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...init })

describe('assistantPanelStore', () => {
  beforeEach(resetAssistantPanel)

  test('默认关闭、宽 400、对话视图', () => {
    expect(getAssistantPanel()).toEqual({ open: false, width: 400, view: 'chat' })
  })

  test('开关；重新打开保留上次视图与宽度', () => {
    openAssistantPanel()
    showAssistantHistory()
    setAssistantPanelWidth(500)
    expect(getAssistantPanel()).toEqual({ open: true, width: 500, view: 'history' })
    toggleAssistantPanel()
    expect(getAssistantPanel().open).toBe(false)
    toggleAssistantPanel()
    expect(getAssistantPanel()).toEqual({ open: true, width: 500, view: 'history' })
    showAssistantChat()
    toggleAssistantPanel()
    toggleAssistantPanel()
    expect(getAssistantPanel().view).toBe('chat')
    showAssistantChat()
    closeAssistantPanel()
    expect(getAssistantPanel().open).toBe(false)
  })

  test('无变化不换快照引用', () => {
    const before = getAssistantPanel()
    closeAssistantPanel()
    showAssistantChat()
    setAssistantPanelWidth(400)
    expect(getAssistantPanel()).toBe(before)
  })

  test('宽度夹到 320–720，窗口过窄时保证主区 ≥ 360', () => {
    expect(clampPanelWidth(100)).toBe(320)
    expect(clampPanelWidth(9999)).toBe(720)
    expect(clampPanelWidth(500.4)).toBe(500)
    expect(clampPanelWidth(720, 1000)).toBe(640)
    expect(clampPanelWidth(720, 1600)).toBe(720)
    expect(clampPanelWidth(500, 600)).toBe(320)
    setAssistantPanelWidth(700, 900)
    expect(getAssistantPanel().width).toBe(540)
  })

  test('⌘K / Ctrl+K 按平台区分，修饰键组合不算', () => {
    expect(isAssistantShortcut(key({ metaKey: true }), true)).toBe(true)
    expect(isAssistantShortcut(key({ key: 'K', metaKey: true }), true)).toBe(true)
    expect(isAssistantShortcut(key({ ctrlKey: true }), true)).toBe(false)
    expect(isAssistantShortcut(key({ ctrlKey: true }), false)).toBe(true)
    expect(isAssistantShortcut(key({ metaKey: true }), false)).toBe(false)
    expect(isAssistantShortcut(key({ ctrlKey: true, shiftKey: true }), false)).toBe(false)
    expect(isAssistantShortcut(key({ ctrlKey: true, altKey: true }), false)).toBe(false)
    expect(isAssistantShortcut(key({ ctrlKey: true, key: 'j' }), false)).toBe(false)
    expect(isAssistantShortcut(key({}), false)).toBe(false)
  })
})
