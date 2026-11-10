import { describe, expect, test } from 'bun:test'
import { applyHints, emptyHints, emptyTracking, joinContinuations, parseCommand, planFieldFill } from './aria2c'
import type { FillTracking, HeaderRowLike } from './aria2c'
import { appendEntries, parseEntries, parseInput } from './model'

const urls = (command: string): string[] => {
  const parsed = parseCommand(command)
  if (!parsed) throw new Error(`not aria2c: ${command}`)
  return parsed.entries.map((e) => e.url)
}
const entry = (url: string) => ({ url, fileName: '', checksum: '' })
const cmd = (line: string) => {
  const parsed = parseCommand(line)
  if (!parsed) throw new Error(`not aria2c: ${line}`)
  return parsed
}

describe('aria2c 命令识别', () => {
  test('只识别 aria2c 命令行', () => {
    for (const line of [
      'aria2c https://a.example/x',
      'ARIA2C https://a.example/x',
      'aria2c.exe https://a.example/x',
      '/usr/local/bin/aria2c https://a.example/x',
      '"C:\\Program Files\\aria2\\aria2c.exe" https://a.example/x',
      'C:\\aria2\\aria2c.exe https://a.example/x',
    ]) {
      expect(urls(line)).toEqual(['https://a.example/x'])
    }
    for (const line of [
      'https://a.example/aria2c https://b.example/x',
      'ed2k://|file|aria2c|1|ff|/',
      'aria2c-wrapper https://a.example/x',
      '  aria2c https://a.example/x',
      'see aria2c https://a.example/x',
      'aria2',
    ]) {
      expect(parseCommand(line)).toBeNull()
    }
  })

  test('引号与转义遵循 shell 规则', () => {
    expect(urls(`aria2c 'https://a.example/x?a=1&b=2' "https://b.example/y z?q=\\"1\\"" https://c.example/a\\ b`)).toEqual(['https://a.example/x?a=1&b=2'])
    expect(urls(`aria2c -Z 'https://a.example/x?a=1&b=2' "https://b.example/y z" https://c.example/a\\ b "https://d.example/\\$x"`)).toEqual([
      'https://a.example/x?a=1&b=2',
      'https://b.example/y z',
      'https://c.example/a b',
      'https://d.example/$x',
    ])
    expect(cmd('aria2c -d "C:\\Downloads" -o "a b.zip" https://a.example/x').entries[0].fileName).toBe('a b.zip')
  })
})

describe('aria2c 选项', () => {
  test('--out / -o / --checksum', () => {
    const digest = 'd'.repeat(64)
    const command = cmd(`aria2c --out=a.zip --checksum=SHA-256=${digest.toUpperCase()} https://a.example/x`)
    expect(command.entries[0].fileName).toBe('a.zip')
    expect(command.entries[0].checksum).toBe(`sha-256=${digest}`)
    for (const line of [
      'aria2c --out b.zip https://a.example/x',
      'aria2c -o b.zip https://a.example/x',
      'aria2c -ob.zip https://a.example/x',
      'aria2c -cx16 -o b.zip https://a.example/x',
      'aria2c https://a.example/x -o b.zip',
    ]) {
      expect(cmd(line).entries[0].fileName).toBe('b.zip')
    }
    const sub = cmd('aria2c -o sub/dir/c.zip --checksum=adler32=abcd1234 https://a.example/x')
    expect(sub.entries[0].fileName).toBe('c.zip')
    expect(sub.entries[0].checksum).toBe('')
    expect(cmd('aria2c -o .. --checksum=md5=xyz https://a.example/x').entries[0]).toEqual(entry('https://a.example/x'))
  })

  test('多 URI 为镜像，-Z 时各成一条', () => {
    expect(urls('aria2c -o x.zip https://a.example/x https://b.example/x ftp://c.example/x')).toEqual(['https://a.example/x'])
    expect(cmd('aria2c -Z -o ignored.zip https://a.example/x https://b.example/y').entries).toEqual([entry('https://a.example/x'), entry('https://b.example/y')])
    for (const flag of ['--force-sequential', '--force-sequential=true', '-cZ']) {
      expect(urls(`aria2c ${flag} https://a.example/x https://b.example/y`)).toHaveLength(2)
    }
    expect(urls('aria2c --force-sequential=false https://a.example/x https://b.example/y')).toHaveLength(1)
    expect(urls("aria2c 'magnet:?xt=urn:btih:aa' https://a.example/x https://b.example/x 'magnet:?xt=urn:btih:bb'")).toEqual([
      'magnet:?xt=urn:btih:aa',
      'https://a.example/x',
      'magnet:?xt=urn:btih:bb',
    ])
  })

  test('带值选项的值不会误判为链接', () => {
    const command = cmd(
      'aria2c -d /tmp/dl -x 16 -s16 -k 1M --load-cookies cookies.txt --all-proxy http://proxy:8080 --referer https://ref.example/ --conf-path=/etc/a.conf --continue true https://a.example/x',
    )
    expect(command.entries).toEqual([entry('https://a.example/x')])
    expect(command.hints.referrer).toBe('https://ref.example/')
    expect(command.hints.headers).toEqual([])
    expect(urls('aria2c -- https://a.example/x')).toEqual(['https://a.example/x'])
    expect(urls('aria2c https://a.example/x && echo https://b.example/y')).toEqual(['https://a.example/x'])
    expect(urls('aria2c https://a.example/x?a=1&b=2')).toEqual(['https://a.example/x?a=1&b=2'])
  })

  test('头部信息收集到 hints', () => {
    const command = cmd(
      `aria2c --header "Cookie: a=1" --header='X-Token: t' --header "cookie: b=2" --header "Referer: https://r.example/" -U "Agent/1.0" --header "User-Agent: Agent/2.0" --header "Bad" --header "X-Token: t2" https://a.example/x`,
    )
    expect(command.hints).toEqual({
      cookie: 'a=1; b=2',
      userAgent: 'Agent/2.0',
      referrer: 'https://r.example/',
      headers: [['X-Token', 't2']],
    })
    expect(cmd('aria2c https://a.example/x').hints).toEqual(emptyHints())
  })

  test('-i 不产出条目', () => {
    for (const line of ['aria2c -i urls.txt', 'aria2c --input-file=urls.txt https://a.example/x', "aria2c -x16 -i list.txt --header 'Cookie: a=1'"]) {
      expect(cmd(line)).toEqual({ entries: [], hints: emptyHints() })
    }
  })
})

describe('续行合并', () => {
  test('只在 aria2c 命令内部合并', () => {
    const text = 'aria2c -x16 \\\n  -o a.zip \\\r\n  https://a.example/x\nhttps://b.example/y\n  out=b.zip'
    expect(joinContinuations(text)).toBe('aria2c -x16   -o a.zip   https://a.example/x\nhttps://b.example/y\n  out=b.zip')
    expect(joinContinuations('aria2c ^\n  -o a.zip ^\n  https://a.example/x')).toBe('aria2c   -o a.zip   https://a.example/x')
    const literal = 'aria2c https://a.example/x\\\\\nhttps://b.example/y'
    expect(joinContinuations(literal)).toBe(literal)
    const plain = 'https://a.example/x\\\n  out=a.zip'
    expect(joinContinuations(plain)).toBe(plain)
  })
})

describe('parseEntries 集成', () => {
  test('命令与其他行混合，其他行不变', () => {
    const text =
      "https://plain.example/a.zip\n  out=plain.zip\naria2c -x16 \\\n  -o b.zip \\\n  'https://b.example/b?x=1&y=2'\n  checksum=md5=ff\nmagnet:?xt=urn:btih:xyz\naria2c -i list.txt\n  out=ignored\nhttps://tail.example/t"
    for (const loose of [false, true]) {
      expect(parseEntries(text, loose)).toEqual([
        { url: 'https://plain.example/a.zip', fileName: 'plain.zip', checksum: '' },
        { url: 'https://b.example/b?x=1&y=2', fileName: 'b.zip', checksum: 'md5=ff' },
        entry('magnet:?xt=urn:btih:xyz'),
        entry('https://tail.example/t'),
      ])
    }
    const seq = parseEntries('aria2c -Z https://a.example/1 https://a.example/2\n  out=two.zip', false)
    expect(seq).toHaveLength(2)
    expect(seq[0].fileName).toBe('')
    expect(seq[1].fileName).toBe('two.zip')
  })

  test('前导空白 / 零宽字符 / BOM 不吞链接，选项行语义不变', () => {
    const url = 'https://a.example/x.exe'
    for (const text of [` ${url}`, `\t${url}`, `\u200B${url}`, `\uFEFF${url}`, `  \u2060${url}\u200D`]) {
      expect(parseEntries(text, false)).toEqual([entry(url)])
    }
    expect(parseEntries('https://a.example/a\n  out=a.bin\n  https://a.example/b\n\tchecksum=md5=ff', false)).toEqual([
      { url: 'https://a.example/a', fileName: 'a.bin', checksum: '' },
      { url: 'https://a.example/b', fileName: '', checksum: 'md5=ff' },
    ])
    expect(parseEntries('  out=orphan.bin', false)).toEqual([])
  })

  test('hints 取首条产出条目的命令', () => {
    const parsed = parseInput("aria2c -i x.txt -U ignored\naria2c -U first https://a.example/1\naria2c -U second https://a.example/2", false)
    expect(parsed.entries.map((e) => e.url)).toEqual(['https://a.example/1', 'https://a.example/2'])
    expect(parsed.hints.userAgent).toBe('first')
    expect(parseInput('https://a.example/1', false).hints).toEqual(emptyHints())
  })
})

describe('planFieldFill', () => {
  test('只动空字段或自己回填的字段', () => {
    expect(planFieldFill('', null, 'a=1')).toEqual({ kind: 'set', value: 'a=1' })
    expect(planFieldFill('  ', null, '')).toEqual({ kind: 'keep' })
    expect(planFieldFill('mine', null, 'a=1')).toEqual({ kind: 'keep' })
    expect(planFieldFill('mine', null, '')).toEqual({ kind: 'keep' })
    expect(planFieldFill('a=1', null, 'a=1')).toEqual({ kind: 'keep' })
    expect(planFieldFill('a', 'a', 'ab')).toEqual({ kind: 'set', value: 'ab' })
    expect(planFieldFill('ab', 'ab', 'ab')).toEqual({ kind: 'keep' })
    expect(planFieldFill('ab', 'ab', '')).toEqual({ kind: 'clear' })
    expect(planFieldFill('edited', 'ab', 'abc')).toEqual({ kind: 'forget' })
    expect(planFieldFill('', 'ab', 'abc')).toEqual({ kind: 'set', value: 'abc' })
  })
})

describe('applyHints', () => {
  const hints = (headers: Array<[string, string]>, cookie = '', userAgent = '') => ({ cookie, userAgent, referrer: '', headers })
  let seq = 0
  const nextId = () => (seq += 1)

  test('新增 / 更新 / 删除自己回填的请求头，不碰用户行', () => {
    let tracking: FillTracking = emptyTracking()
    let rows: HeaderRowLike[] = [{ id: 100, key: 'x-mine', value: 'u' }]
    let r = applyHints({ cookie: '', userAgent: '', rows }, tracking, hints([['X-Ref', 'https://r/'], ['X-Mine', 'cmd']], 'a=1', 'UA/1'), nextId)
    expect(r.filled).toBe(true)
    expect(r.cookie).toBe('a=1')
    expect(r.userAgent).toBe('UA/1')
    expect(r.rows).toHaveLength(2)
    expect(r.rows[0]).toEqual({ id: 100, key: 'x-mine', value: 'u' })
    expect(r.rows[1].key).toBe('X-Ref')

    tracking = r.tracking
    rows = r.rows
    r = applyHints({ cookie: r.cookie, userAgent: r.userAgent, rows }, tracking, hints([['x-ref', 'https://r2/']], 'a=2', ''), nextId)
    expect(r.rows.find((row) => row.key === 'X-Ref')?.value).toBe('https://r2/')
    expect(r.cookie).toBe('a=2')
    expect(r.userAgent).toBe('')
    expect(r.rows.find((row) => row.id === 100)?.value).toBe('u')

    r = applyHints({ cookie: r.cookie, userAgent: r.userAgent, rows: r.rows }, r.tracking, hints([]), nextId)
    expect(r.rows).toEqual([{ id: 100, key: 'x-mine', value: 'u' }])
    expect(r.cookie).toBe('')
  })

  test('用户改过 / 删除的行不再覆盖或重建', () => {
    const first = applyHints({ cookie: '', userAgent: '', rows: [] }, emptyTracking(), hints([['X-A', '1']]), nextId)
    const id = first.rows[0].id
    const edited = applyHints({ cookie: '', userAgent: '', rows: [{ id, key: 'X-A', value: 'mine' }] }, first.tracking, hints([['X-A', '2']]), nextId)
    expect(edited.rows[0].value).toBe('mine')
    expect(edited.tracking.headers).toHaveLength(0)
    const deleted = applyHints({ cookie: '', userAgent: '', rows: [] }, first.tracking, hints([['X-A', '3']]), nextId)
    expect(deleted.rows).toHaveLength(0)
    expect(deleted.filled).toBe(false)
    const gone = applyHints({ cookie: '', userAgent: '', rows: [{ id, key: 'X-A', value: 'mine' }] }, first.tracking, hints([]), nextId)
    expect(gone.rows).toHaveLength(1)
  })
})

describe('TXT 导入追加', () => {
  test('原样保留已有 aria2c 命令及其头部信息，按 URL 去重追加', () => {
    const existing = "aria2c -x16 \\\n  --header 'Cookie: sid=abc' \\\n  -o a.zip 'https://a.example/a?t=1&u=2'"
    const text = appendEntries(existing, [entry('https://b.example/b.zip'), entry('https://a.example/a?t=1&u=2')])
    expect(text).toBe(`${existing}\nhttps://b.example/b.zip`)
    const parsed = parseInput(text, false)
    expect(parsed.hints.cookie).toBe('sid=abc')
    expect(parsed.entries.map((e) => e.url)).toEqual(['https://a.example/a?t=1&u=2', 'https://b.example/b.zip'])
    expect(parsed.entries[0].fileName).toBe('a.zip')
  })
})
