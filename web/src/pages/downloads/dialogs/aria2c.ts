// aria2c 命令行解析（#299）：粘贴 `aria2c ...` 命令文本即可建任务。
// 逐条对应 crates/downloads/src/model/aria2c.rs，两端规则必须保持同步。
//
// - 行首（第一个词，可带路径、忽略大小写）为 `aria2c` / `aria2c.exe` 才视为命令，其余行完全不受影响；
// - 按 shell 规则切词（单引号、双引号、反斜杠转义），遇到独立的 `&&` / `||` / `|` / `;` / `&` 结束；
// - 同一命令多个 URI 是同一文件的镜像，只取第一个；`-Z` / `--force-sequential` 时每个 URI 各成一条；
//   磁力 / ed2k / thunder 不是镜像，各成一条；
// - `-o` / `--out`（仅单条）→ 文件名，`--checksum`（仅单条、引擎支持的算法且哈希合法）→ 校验；
// - `--header` / `--referer` / `-U` / `--user-agent` 收集为 RequestHints，由表单回填高级字段（`Referer` 进「来源页」专门字段，不再作自定义请求头）；
// - `-i` / `--input-file` 读取本地文件，无法处理 → 该命令不产出条目。

import type { UrlEntry } from './model'

/** 命令里能映射到表单高级字段的请求信息。 */
export interface RequestHints {
  /** `Cookie` 头（多个 `Cookie` 头以 `; ` 连接）。 */
  cookie: string
  /** `-U` / `--user-agent` / `User-Agent` 头（后出现者生效）。 */
  userAgent: string
  /** `--referer` / `Referer` 头（后出现者生效），回填「来源页」框。 */
  referrer: string
  /** 其余请求头，按首次出现顺序，同名（忽略大小写）后者覆盖前者。 */
  headers: Array<[string, string]>
}

export function emptyHints(): RequestHints {
  return { cookie: '', userAgent: '', referrer: '', headers: [] }
}

export function hintsEqual(a: RequestHints, b: RequestHints): boolean {
  return (
    a.cookie === b.cookie &&
    a.userAgent === b.userAgent &&
    a.referrer === b.referrer &&
    a.headers.length === b.headers.length &&
    a.headers.every(([name, value], index) => name === b.headers[index][0] && value === b.headers[index][1])
  )
}

// Rust `char::is_whitespace`（Unicode White_Space），不同于 JS `\s`（后者含 U+FEFF、不含 U+0085）。
const WS = '\\t\\n\\v\\f\\r \\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000'
const WS_CHAR = new RegExp(`[${WS}]`)
const WS_START = new RegExp(`^[${WS}]`)
const WS_END = new RegExp(`[${WS}]+$`)
const WS_TRIM = new RegExp(`^[${WS}]+|[${WS}]+$`, 'g')

const isWhitespace = (c: string): boolean => WS_CHAR.test(c)
const trimEnd = (s: string): string => s.replace(WS_END, '')
/** Rust `str::trim`。 */
export const rustTrim = (s: string): string => s.replace(WS_TRIM, '')

const asciiLower = (s: string): string => s.replace(/[A-Z]/g, (c) => c.toLowerCase())

function setHeader(hints: RequestHints, name: string, value: string): void {
  const key = asciiLower(name)
  const existing = hints.headers.find(([n]) => asciiLower(n) === key)
  if (existing) existing[1] = value
  else hints.headers.push([name, value])
}

/** `--header "Name: value"`；`Cookie` / `User-Agent` 进专门字段，空名 / 空值忽略。 */
function addRawHeader(hints: RequestHints, raw: string): void {
  const colon = raw.indexOf(':')
  if (colon < 0) return
  const name = rustTrim(raw.slice(0, colon))
  const value = rustTrim(raw.slice(colon + 1))
  if (name === '' || value === '' || WS_CHAR.test(name)) return
  const lower = asciiLower(name)
  if (lower === 'cookie') {
    if (hints.cookie !== '') hints.cookie += '; '
    hints.cookie += value
  } else if (lower === 'user-agent') {
    hints.userAgent = value
  } else if (lower === 'referer') {
    hints.referrer = value
  } else {
    setHeader(hints, name, value)
  }
}

/** 一条 aria2c 命令的解析结果。 */
export interface Aria2cCommand {
  entries: UrlEntry[]
  hints: RequestHints
}

/**
 * 需要独立取值的长选项（`--name value`）；`--name=value` 写法不查此表。
 * 不在表内的长选项视为无值开关（aria2 的布尔选项只接受 `=` 写法）。
 */
const LONG_VALUE_OPTIONS: ReadonlySet<string> = new Set([
  'all-proxy',
  'all-proxy-passwd',
  'all-proxy-user',
  'async-dns-server',
  'auto-save-interval',
  'bt-exclude-tracker',
  'bt-external-ip',
  'bt-max-open-files',
  'bt-max-peers',
  'bt-min-crypto-level',
  'bt-prioritize-piece',
  'bt-request-peer-speed-limit',
  'bt-stop-timeout',
  'bt-tracker',
  'bt-tracker-connect-timeout',
  'bt-tracker-interval',
  'bt-tracker-timeout',
  'ca-certificate',
  'certificate',
  'checksum',
  'conf-path',
  'connect-timeout',
  'console-log-level',
  'dht-file-path',
  'dht-file-path6',
  'dht-listen-addr6',
  'dht-listen-port',
  'dht-message-timeout',
  'dir',
  'disk-cache',
  'download-result',
  'dscp',
  'event-poll',
  'file-allocation',
  'follow-metalink',
  'follow-torrent',
  'ftp-passwd',
  'ftp-proxy',
  'ftp-proxy-passwd',
  'ftp-proxy-user',
  'ftp-type',
  'ftp-user',
  'gid',
  'header',
  'http-passwd',
  'http-proxy',
  'http-proxy-passwd',
  'http-proxy-user',
  'http-user',
  'https-proxy',
  'https-proxy-passwd',
  'https-proxy-user',
  'index-out',
  'input-file',
  'interface',
  'listen-port',
  'load-cookies',
  'log',
  'log-level',
  'lowest-speed-limit',
  'max-concurrent-downloads',
  'max-connection-per-server',
  'max-download-limit',
  'max-download-result',
  'max-file-not-found',
  'max-mmap-limit',
  'max-overall-download-limit',
  'max-overall-upload-limit',
  'max-resume-failure-tries',
  'max-tries',
  'max-upload-limit',
  'metalink-base-uri',
  'metalink-file',
  'metalink-language',
  'metalink-location',
  'metalink-os',
  'metalink-preferred-protocol',
  'metalink-version',
  'min-split-size',
  'min-tls-version',
  'multiple-interface',
  'netrc-path',
  'no-file-allocation-limit',
  'no-proxy',
  'on-bt-download-complete',
  'on-download-complete',
  'on-download-error',
  'on-download-pause',
  'on-download-start',
  'on-download-stop',
  'out',
  'peer-agent',
  'peer-id-prefix',
  'piece-length',
  'private-key',
  'proxy-method',
  'referer',
  'retry-wait',
  'rlimit-nofile',
  'rpc-certificate',
  'rpc-listen-port',
  'rpc-max-request-size',
  'rpc-passwd',
  'rpc-private-key',
  'rpc-secret',
  'rpc-user',
  'save-cookies',
  'save-session',
  'save-session-interval',
  'seed-ratio',
  'seed-time',
  'select-file',
  'server-stat-if',
  'server-stat-of',
  'server-stat-timeout',
  'socket-recv-buffer-size',
  'split',
  'stop',
  'stop-with-process',
  'stream-piece-selector',
  'summary-interval',
  'timeout',
  'torrent-file',
  'uri-selector',
  'user-agent',
])

/** 带值短选项（`-x16` / `-x 16`）：`-d -i -j -k -l -m -M -o -O -s -t -T -u -U -x`。 */
const SHORT_VALUE_OPTIONS = 'dijklmMoOstTuUx'

const LINK_SCHEMES = ['https://', 'http://', 'ftp://', 'thunder://', 'magnet:?', 'ed2k://']
const MIRROR_SCHEMES = ['http://', 'https://', 'ftp://']

const startsWithScheme = (text: string, schemes: readonly string[]): boolean => {
  const head = asciiLower(text.slice(0, 10))
  return schemes.some((scheme) => head.startsWith(scheme))
}

/** 是否以可建任务的链接开头（忽略大小写）。 */
export const startsWithLink = (text: string): boolean => startsWithScheme(text, LINK_SCHEMES)

/** http(s) / ftp 才可能是同一文件的镜像；磁力 / ed2k / thunder 自成一个下载。 */
export const isMirrorable = (uri: string): boolean => startsWithScheme(uri, MIRROR_SCHEMES)

/**
 * 命令行的参数部分（去掉命令名之后）；该行不是 aria2c 命令时为 null。
 * 命令名必须位于行首，可带路径 / 引号；含 `://` 的链接行不会被误判。
 */
export function commandArgs(rawLine: string): string | null {
  // 命令名必含 aria2c（忽略大小写）：廉价预检，避免对大批量普通行做完整扫描。
  if (!/aria2c/i.test(rawLine)) return null
  const line = trimEnd(rawLine)
  if (line === '') return null
  const first = String.fromCodePoint(line.codePointAt(0) as number)
  if (isWhitespace(first)) return null
  let head: string
  let rest: string
  if (first === '"' || first === "'") {
    const body = line.slice(1)
    const end = body.indexOf(first)
    if (end < 0) return null
    head = body.slice(0, end)
    rest = body.slice(end + 1)
  } else {
    let end = line.length
    for (let i = 0; i < line.length; i += 1) {
      if (WS_START.test(line[i])) {
        end = i
        break
      }
    }
    head = line.slice(0, end)
    rest = line.slice(end)
  }
  if (rest !== '' && !WS_START.test(rest)) return null
  if (head.includes('://')) return null
  const name = head.split(/[/\\]/).pop() as string
  const lower = asciiLower(name)
  return lower === 'aria2c' || lower === 'aria2c.exe' ? rest : null
}

/** 行末的续行符（奇数个 `\` 或 Windows `^`）：返回去掉续行符后的内容。 */
function stripContinuation(line: string): string | null {
  const marker = line.slice(-1)
  if (marker !== '\\' && marker !== '^') return null
  let run = 0
  for (let i = line.length - 1; i >= 0 && line[i] === marker; i -= 1) run += 1
  return run % 2 === 1 ? line.slice(0, -1) : null
}

/**
 * 合并 aria2c 命令的续行（`\`+换行、Windows `^`+换行），只作用于 aria2c 命令内部；
 * 其余行逐字保留。必须先于按行解析进行。
 */
export function joinContinuations(text: string): string {
  const lines = text.split('\n')
  const needsJoin = lines.some((line) => commandArgs(line) !== null && stripContinuation(trimEnd(line)) !== null)
  if (!needsJoin) return text
  const out: string[] = []
  let index = 0
  while (index < lines.length) {
    const line = lines[index]
    index += 1
    if (commandArgs(line) === null) {
      out.push(line)
      continue
    }
    let piece = line
    let merged = ''
    for (;;) {
      const trimmed = trimEnd(piece)
      const stripped = stripContinuation(trimmed)
      if (stripped === null) {
        merged += trimmed
        break
      }
      merged += stripped
      if (index >= lines.length) break
      piece = lines[index]
      index += 1
    }
    out.push(merged)
  }
  return out.join('\n')
}

/** 切词结果；`literal` = 含引号 / 转义（此时 `&&` 等不是 shell 运算符）。 */
interface Token {
  text: string
  literal: boolean
}

/**
 * 按 POSIX shell 规则切词：单引号内逐字，双引号内只转义 `"` `\` `$` `` ` ``，
 * 引号外 `\` 转义下一个字符；引号未闭合时取到末尾。
 */
function tokenize(input: string): Token[] {
  const tokens: Token[] = []
  const chars = Array.from(input)
  let text = ''
  let inToken = false
  let literal = false
  let i = 0
  while (i < chars.length) {
    const c = chars[i]
    i += 1
    if (isWhitespace(c)) {
      if (inToken) {
        tokens.push({ text, literal })
        text = ''
        inToken = false
        literal = false
      }
    } else if (c === "'") {
      inToken = true
      literal = true
      while (i < chars.length) {
        const q = chars[i]
        i += 1
        if (q === "'") break
        text += q
      }
    } else if (c === '"') {
      inToken = true
      literal = true
      while (i < chars.length) {
        const q = chars[i]
        i += 1
        if (q === '"') break
        if (q === '\\') {
          const next = chars[i]
          if (next === '"' || next === '\\' || next === '$' || next === '`') {
            text += next
            i += 1
          } else {
            text += '\\'
          }
        } else {
          text += q
        }
      }
    } else if (c === '\\') {
      inToken = true
      literal = true
      if (i < chars.length) {
        text += chars[i]
        i += 1
      }
    } else {
      inToken = true
      text += c
    }
  }
  if (inToken) tokens.push({ text, literal })
  return tokens
}

/** `-o` 值 → 文件名：只取最后一段；`.` / `..` 无效。 */
export function outFileName(raw: string): string {
  const name = rustTrim(raw.split(/[/\\]/).pop() as string)
  return name === '' || name === '.' || name === '..' ? '' : name
}

const CHECKSUM_LEN: Readonly<Record<string, number>> = {
  md5: 32,
  'sha-1': 40,
  sha1: 40,
  'sha-256': 64,
  sha256: 64,
  'sha-512': 128,
  sha512: 128,
}

/** `--checksum=TYPE=DIGEST` → `algo=hex`（小写）；只接受引擎支持的算法且哈希长度合法。 */
export function normalizeChecksum(raw: string): string | null {
  const eq = raw.indexOf('=')
  if (eq < 0) return null
  const algo = asciiLower(rustTrim(raw.slice(0, eq)))
  const digest = asciiLower(rustTrim(raw.slice(eq + 1)))
  const len = Object.hasOwn(CHECKSUM_LEN, algo) ? CHECKSUM_LEN[algo] : undefined
  if (len === undefined) return null
  return digest.length === len && /^[0-9a-f]*$/.test(digest) ? `${algo}=${digest}` : null
}

/** 扫描过程中收集的命令状态。 */
class Scan {
  uris: string[] = []
  out: string | null = null
  checksum: string | null = null
  forceSequential = false
  inputFile = false
  hints: RequestHints = emptyHints()

  positional(text: string): void {
    if (startsWithLink(text)) this.uris.push(text)
  }

  longOption(name: string, value: string | null): void {
    switch (name) {
      case 'out':
        this.out = value === null ? null : outFileName(value)
        break
      case 'checksum':
        this.checksum = value === null ? null : normalizeChecksum(value)
        break
      case 'header':
        if (value !== null) addRawHeader(this.hints, value)
        break
      case 'referer':
        this.setReferer(value)
        break
      case 'user-agent':
        this.setUserAgent(value)
        break
      case 'force-sequential':
        this.forceSequential = value === null || asciiLower(value) !== 'false'
        break
      case 'input-file':
        this.inputFile = true
        break
      default:
    }
  }

  shortOption(flag: string, value: string | null): void {
    if (flag === 'o') this.out = value === null ? null : outFileName(value)
    else if (flag === 'U') this.setUserAgent(value)
    else if (flag === 'i') this.inputFile = true
  }

  private setReferer(value: string | null): void {
    const v = value === null ? '' : rustTrim(value)
    if (v !== '') this.hints.referrer = v
  }

  private setUserAgent(value: string | null): void {
    const v = value === null ? '' : rustTrim(value)
    if (v !== '') this.hints.userAgent = v
  }

  finish(): Aria2cCommand {
    if (this.inputFile) return { entries: [], hints: emptyHints() }
    const entries: UrlEntry[] = []
    let mirrorTaken = false
    for (const uri of this.uris) {
      if (!this.forceSequential && isMirrorable(uri)) {
        if (mirrorTaken) continue
        mirrorTaken = true
      }
      entries.push({ url: uri, fileName: '', checksum: '' })
    }
    if (entries.length === 1) {
      if (this.out !== null && this.out !== '') entries[0].fileName = this.out
      if (this.checksum !== null) entries[0].checksum = this.checksum
    }
    return { entries, hints: this.hints }
  }
}

/** 解析一行 aria2c 命令（调用方已合并续行）；不是 aria2c 命令时返回 null。 */
export function parseCommand(line: string): Aria2cCommand | null {
  const args = commandArgs(line)
  if (args === null) return null
  const tokens = tokenize(args)
  const scan = new Scan()
  let optionsDone = false
  let index = 0
  while (index < tokens.length) {
    const token = tokens[index]
    index += 1
    const text = token.text
    if (!token.literal && (text === '&&' || text === '||' || text === '|' || text === ';' || text === '&')) break
    if (optionsDone || !text.startsWith('-') || text === '-') {
      scan.positional(text)
    } else if (text === '--') {
      optionsDone = true
    } else if (text.startsWith('--')) {
      const long = text.slice(2)
      const eq = long.indexOf('=')
      const name = eq < 0 ? long : long.slice(0, eq)
      let value: string | null
      if (eq >= 0) {
        value = long.slice(eq + 1)
      } else if (LONG_VALUE_OPTIONS.has(name)) {
        value = index < tokens.length ? tokens[index].text : null
        if (value !== null) index += 1
      } else {
        value = null
      }
      scan.longOption(name, value)
    } else {
      const cluster = text.slice(1)
      let offset = 0
      for (const flag of cluster) {
        offset += flag.length
        if (SHORT_VALUE_OPTIONS.includes(flag)) {
          const attached = cluster.slice(offset)
          let value: string | null
          if (attached === '') {
            value = index < tokens.length ? tokens[index].text : null
            if (value !== null) index += 1
          } else {
            value = attached
          }
          scan.shortOption(flag, value)
          break
        }
        if (flag === 'Z') scan.forceSequential = true
      }
    }
  }
  return scan.finish()
}

/** 回填单个表单字段的决策（Cookie / UA / 请求头值）。 */
export type FieldFill = { kind: 'keep' } | { kind: 'forget' } | { kind: 'set'; value: string } | { kind: 'clear' }

const KEEP: FieldFill = { kind: 'keep' }

/**
 * 命令头部信息 → 字段的回填规则：只在字段为空、或仍是上次自动回填的值时才写；
 * 用户填过的内容绝不覆盖。`filled` = 上次回填的值（无 = null）。
 */
export function planFieldFill(current: string, filled: string | null, hint: string): FieldFill {
  const value = rustTrim(current)
  if (value !== '' && filled !== value) return filled !== null ? { kind: 'forget' } : KEEP
  if (hint === '') return filled !== null ? { kind: 'clear' } : KEEP
  return value === hint ? KEEP : { kind: 'set', value: hint }
}

/** `parseEntries` 的完整结果：条目 + 首条产出了条目的 aria2c 命令带出的请求信息。 */
export interface ParsedInput {
  entries: UrlEntry[]
  hints: RequestHints
}

/** 自定义请求头行（与表单状态同形）。 */
export interface HeaderRowLike {
  id: number
  key: string
  value: string
}

/** 回填的跟踪状态：上次回填的值；自定义头按名称（小写）记录行 id 与回填值。 */
export interface FillTracking {
  cookie: string | null
  userAgent: string | null
  headers: Array<{ name: string; id: number; value: string }>
}

export const emptyTracking = (): FillTracking => ({ cookie: null, userAgent: null, headers: [] })

export interface FillResult {
  cookie: string
  userAgent: string
  rows: HeaderRowLike[]
  tracking: FillTracking
  /** 是否写入 / 新增了内容（调用方据此展开「高级」区）。 */
  filled: boolean
}

/**
 * 把命令头部信息回填到表单（规则与桌面端一致）：Cookie / UA / 请求头值都只在为空或仍是
 * 上次回填值时更新；用户编辑过即不再认领；用户已有的同名请求头不覆盖；
 * 命令里消失的、值仍是回填值的行被删除。
 */
export function applyHints(
  form: { cookie: string; userAgent: string; rows: readonly HeaderRowLike[] },
  tracking: FillTracking,
  hints: RequestHints,
  nextId: () => number,
): FillResult {
  let filled = false
  const next: FillTracking = { cookie: tracking.cookie, userAgent: tracking.userAgent, headers: [] }

  const fillField = (current: string, last: string | null, hint: string): [string, string | null] => {
    const plan = planFieldFill(current, last, hint)
    switch (plan.kind) {
      case 'set':
        filled = true
        return [plan.value, plan.value]
      case 'clear':
        return ['', null]
      case 'forget':
        return [current, null]
      default:
        return [current, last]
    }
  }

  const [cookie, cookieFilled] = fillField(form.cookie, tracking.cookie, hints.cookie)
  const [userAgent, userAgentFilled] = fillField(form.userAgent, tracking.userAgent, hints.userAgent)
  next.cookie = cookieFilled
  next.userAgent = userAgentFilled

  let rows = form.rows.slice()
  const rowName = (row: HeaderRowLike) => asciiLower(rustTrim(row.key))
  const hinted = new Set<string>()
  for (const [name, value] of hints.headers) {
    const lower = asciiLower(name)
    hinted.add(lower)
    const tracked = tracking.headers.find((item) => item.name === lower)
    if (tracked) {
      const at = rows.findIndex((row) => row.id === tracked.id)
      if (at < 0) {
        next.headers.push(tracked)
        continue
      }
      const plan = planFieldFill(rows[at].value, tracked.value, value)
      if (plan.kind === 'forget') continue
      if (plan.kind === 'set') {
        filled = true
        rows[at] = { ...rows[at], value: plan.value }
        next.headers.push({ name: lower, id: tracked.id, value: plan.value })
      } else {
        next.headers.push(tracked)
      }
      continue
    }
    if (rows.some((row) => rowName(row) === lower)) continue
    const id = nextId()
    rows.push({ id, key: name, value })
    next.headers.push({ name: lower, id, value })
    filled = true
  }
  for (const tracked of tracking.headers) {
    if (hinted.has(tracked.name)) continue
    rows = rows.filter((row) => !(row.id === tracked.id && rustTrim(row.value) === tracked.value))
  }

  return { cookie, userAgent, rows, tracking: next, filled }
}
