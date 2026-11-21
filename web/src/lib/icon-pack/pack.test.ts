import { describe, expect, test } from 'bun:test'
import { defaultIcon, fileKindOf, matchedIcon, parseIconPack, svgIsSafe } from './pack'
import type { FileKind } from './pack'
import { iconPackState, resolveFileIcon } from './registry'

interface Case {
  name: string
  text?: string
  pack?: unknown
  error?: string
  diagnostics?: { path: string; kind: string }[]
  icons?: Record<string, string>
  resolve?: { file: string; kind: FileKind; icon: string | null }[]
}

const byPath = (left: [string, string], right: [string, string]) => (left[0] < right[0] ? -1 : left[0] > right[0] ? 1 : left[1] < right[1] ? -1 : 1)

// 用例与 crates/icon_pack/tests/fixtures.rs 共用同一份 cases.json（运行期读取：Docker 构建上下文不含 crates/）。
// tsconfig 未引入 Bun 类型，仅声明测试用到的最小接口。
declare const Bun: { file(path: URL): { json(): Promise<unknown> } }
const cases = (await Bun.file(new URL('../../../../crates/icon_pack/tests/fixtures/cases.json', import.meta.url)).json()) as Case[]

describe('icon pack parse (shared fixtures)', () => {
  for (const item of cases) {
    test(item.name, () => {
      const parsed = parseIconPack(item.text ?? JSON.stringify(item.pack))
      if (item.error) {
        expect(parsed).toEqual({ ok: false, error: item.error as never })
        return
      }
      if (!parsed.ok) throw new Error(parsed.error)
      const diagnostics = parsed.diagnostics.map((entry): [string, string] => [entry.path, entry.kind]).sort(byPath)
      expect(diagnostics).toEqual((item.diagnostics ?? []).map((entry): [string, string] => [entry.path, entry.kind]).sort(byPath))
      const icons = [...parsed.pack.icons].map(([name, icon]): [string, string] => [name, icon.mode]).sort(byPath)
      expect(icons).toEqual(Object.entries(item.icons ?? {}).sort(byPath))
      for (const check of item.resolve ?? []) {
        const found = matchedIcon(parsed.pack, check.file.toLowerCase(), check.kind) ?? defaultIcon(parsed.pack)
        expect(found?.[0] ?? null).toBe(check.icon)
      }
    })
  }
})

describe('svgIsSafe', () => {
  // 与 pack.rs `svg_safety_rejects_active_content_and_external_references` 同组。
  test('accepts fragment references and rejects active or external content', () => {
    expect(svgIsSafe('<svg viewBox="0 0 16 16"><use href="#a"/><path fill="url(#g)"/></svg>')).toBe(true)
    expect(svgIsSafe('<?xml version="1.0"?><svg><path d="M0 0"/></svg>')).toBe(true)
    expect(svgIsSafe('<svg viewBox="0 0 8 8"><image width="8" height="8" href="data:image/png;base64,iVBORw0KGgo="/></svg>')).toBe(true)
    for (const svg of [
      '<svg><script>alert(1)</script></svg>',
      '<svg><image href="file:///etc/passwd"/></svg>',
      '<svg><image href="data:image/svg+xml;base64,PHN2Zz4="/></svg>',
      '<svg><image href="https://x/a.png"/></svg>',
      '<svg><use xlink:href="other.svg#a"/></svg>',
      "<svg><use href = 'https://x/a.svg#a'/></svg>",
      '<svg><path fill="url(https://x/p)"/></svg>',
      '<svg onload="x()"></svg>',
      '<svg><a\nonclick ="x()"/></svg>',
      '<!DOCTYPE svg [<!ENTITY a "b">]><svg/>',
      '<html><svg/></html>',
      "<svg><style>@import 'x.css';</style></svg>",
    ]) {
      expect(svgIsSafe(svg)).toBe(false)
    }
  })
})

describe('builtin packs', () => {
  test('material tells documents apart and system falls back on the web', () => {
    const material = iconPackState('builtin:material')
    const pdf = resolveFileIcon(material, 'report.pdf', fileKindOf('report.pdf'))
    const docx = resolveFileIcon(material, 'notes.docx', fileKindOf('notes.docx'))
    expect(pdf).not.toBeUndefined()
    expect(pdf?.svg.text).not.toBe(docx?.svg.text)
    const system = iconPackState('builtin:system')
    expect(resolveFileIcon(system, 'a.mp4', 'video')?.mode).toBe('mask')
    expect(iconPackState('custom:missing').chain).toEqual(system.chain)
  })

  test('file kinds come from the shared table', () => {
    expect(fileKindOf('Movie.MKV')).toBe('video')
    expect(fileKindOf('backup.tar.gz')).toBe('archive')
    expect(fileKindOf('README')).toBe('other')
    expect(fileKindOf('dir.d/file')).toBe('other')
  })
})
