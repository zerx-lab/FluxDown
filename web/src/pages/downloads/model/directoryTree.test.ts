import { describe, expect, test } from 'bun:test'
import { flattenDirectories } from './directoryTree'
import type { DirectoryItem, DirectoryRow } from './directoryTree'

interface ExpectedRow {
  kind: 'header' | 'item'
  directoryKind?: string
  key?: string
  label?: string
  path?: string
  depth: number
  count?: number
  index?: number
}
interface Fixture {
  cases: { name: string; items: DirectoryItem[]; collapsed: string[]; expect: ExpectedRow[] }[]
}

declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const fixture = (await Bun.file(new URL('../../../../../crates/downloads/tests/fixtures/directory_tree_cases.json', import.meta.url)).json()) as Fixture

const comparable = (row: DirectoryRow): ExpectedRow => row.kind === 'header'
  ? { kind: row.kind, directoryKind: row.directoryKind, key: row.key, label: row.label, path: row.path, depth: row.depth, count: row.count }
  : { kind: row.kind, index: row.index, depth: row.depth }

describe('directory tree (shared fixtures)', () => {
  for (const item of fixture.cases) {
    test(item.name, () => {
      const rows = flattenDirectories(item.items, (key) => item.collapsed.includes(key))
      expect(rows.map(comparable)).toEqual(item.expect)
    })
  }
})
