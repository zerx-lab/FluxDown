import { describe, expect, test } from 'bun:test'
import { btSelectionCanConfirm, btSelectionSaveDir } from './btSelection'

describe('btSelectionSaveDir', () => {
  test('未改、空白、与预填相同都不下发', () => {
    expect(btSelectionSaveDir('/data', '/data')).toBeUndefined()
    expect(btSelectionSaveDir('/data', '  /data  ')).toBeUndefined()
    expect(btSelectionSaveDir('/data', '   ')).toBeUndefined()
    expect(btSelectionSaveDir('', '  ')).toBeUndefined()
    expect(btSelectionSaveDir(' /data ', '/data')).toBeUndefined()
  })

  test('修剪后非空且不同于预填才下发', () => {
    expect(btSelectionSaveDir('/data', ' /new ')).toBe('/new')
    expect(btSelectionSaveDir('', '/new')).toBe('/new')
  })
})

describe('btSelectionCanConfirm', () => {
  test('预填非空且输入被清空时禁用；空预填允许空输入', () => {
    expect(btSelectionCanConfirm('/data', '  ', 1)).toBe(false)
    expect(btSelectionCanConfirm('', '', 1)).toBe(true)
    expect(btSelectionCanConfirm('/data', '/other', 1)).toBe(true)
  })

  test('没有勾选文件时禁用', () => {
    expect(btSelectionCanConfirm('/data', '/data', 0)).toBe(false)
  })
})
