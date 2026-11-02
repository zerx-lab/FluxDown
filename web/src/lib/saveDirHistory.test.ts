import { describe, expect, test } from 'bun:test'
import { SAVE_DIR_HISTORY_LIMIT, readCategorySaveDirHistory, readSaveDirHistory, recordCategorySaveDir, recordSaveDir, saveDirChoices } from './saveDirHistory'

interface Fixture {
  limit: number
  read: { name: string; value: unknown; expected: string[] }[]
  record: { name: string; history: string[]; dir: string; expected: string[] | null }[]
  choices: { name: string; category: string[]; history: string[]; current: string; expected: string[] }[]
  categoryRead: { name: string; value: unknown; expected: Record<string, string[]> }[]
  categoryRecord: { name: string; history: Record<string, string[]>; category: string; dir: string; expected: Record<string, string[]> | null }[]
}

// 与 crates/downloads/tests/fixtures 共用同一份用例（运行期读取：Docker 构建上下文不含 crates/）。
// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const fixture = (await Bun.file(new URL('../../../crates/downloads/tests/fixtures/save_dir_history_cases.json', import.meta.url)).json()) as Fixture

describe('save dir history (shared fixtures)', () => {
  test('limit', () => {
    expect(SAVE_DIR_HISTORY_LIMIT).toBe(fixture.limit)
  })
  for (const item of fixture.read) {
    test(`read: ${item.name}`, () => {
      expect(readSaveDirHistory(item.value)).toEqual(item.expected)
    })
  }
  for (const item of fixture.record) {
    test(`record: ${item.name}`, () => {
      expect(recordSaveDir(item.history, item.dir)).toEqual(item.expected)
    })
  }
  for (const item of fixture.choices) {
    test(`choices: ${item.name}`, () => {
      expect(saveDirChoices(item.category, item.history, item.current)).toEqual(item.expected)
    })
  }
  for (const item of fixture.categoryRead) {
    test(`categoryRead: ${item.name}`, () => {
      expect(readCategorySaveDirHistory(item.value)).toEqual(item.expected)
    })
  }
  for (const item of fixture.categoryRecord) {
    test(`categoryRecord: ${item.name}`, () => {
      expect(recordCategorySaveDir(item.history, item.category, item.dir)).toEqual(item.expected)
    })
  }
})
