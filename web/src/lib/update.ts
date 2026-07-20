// 版本更新检测：对比服务器版本（/api/v1/info）与 GitHub 最新 server-v* release。
// 直查 GitHub API（带 CORS 通配头）；每会话查一次，失败静默（视为无更新）。
// 渠道：服务器 config 键 web_update_channel（stable 默认 / frontier 放行预发布）。

import { useQuery } from '@tanstack/react-query'
import { api } from './api'

const RELEASES_URL = 'https://api.github.com/repos/zerx-lab/FluxDown/releases?per_page=30'
// 稳定版仅认严格三段式；预览版额外放行 -rc.N 预发布后缀。
const STABLE_TAG_RE = /^server-v(\d+\.\d+\.\d+)$/
const FRONTIER_TAG_RE = /^server-v(\d+\.\d+\.\d+(?:-[\w.]+)?)$/

interface GitHubRelease {
  tag_name: string
  html_url: string
  draft: boolean
  prerelease: boolean
}

interface LatestServerRelease {
  version: string
  url: string
}

export interface UpdateState {
  /** 当前服务器版本（info 未加载时为 null）。 */
  current: string | null
  /** 最新 server release 版本（检测失败/未完成时为 null）。 */
  latest: string | null
  /** release 页面地址（手动升级入口）。 */
  releaseUrl: string | null
  /** 有可用新版本。 */
  hasUpdate: boolean
}

/** SemVer 2.0 精度比较：a > b 返回正数。处理 frontier 的 `-rc.N` 预发布后缀。 */
function cmpVersion(a: string, b: string): number {
  const [ca, pa = ''] = a.split('-', 2)
  const [cb, pb = ''] = b.split('-', 2)
  const na = ca.split('.').map((n) => Number.parseInt(n, 10) || 0)
  const nb = cb.split('.').map((n) => Number.parseInt(n, 10) || 0)
  for (let i = 0; i < Math.max(na.length, nb.length); i++) {
    const d = (na[i] ?? 0) - (nb[i] ?? 0)
    if (d !== 0) return d
  }
  // core 相等：无预发布 > 有预发布（SemVer 2.0 §11.3）
  if (!pa && !pb) return 0
  if (!pa) return 1
  if (!pb) return -1
  const ida = pa.split('.')
  const idb = pb.split('.')
  for (let i = 0; i < Math.max(ida.length, idb.length); i++) {
    const x = ida[i]
    const y = idb[i]
    if (x === undefined) return -1
    if (y === undefined) return 1
    const xn = /^\d+$/.test(x)
    const yn = /^\d+$/.test(y)
    if (xn && yn) {
      const d = Number.parseInt(x, 10) - Number.parseInt(y, 10)
      if (d !== 0) return d
    } else if (xn !== yn) {
      return xn ? -1 : 1 // 数字标识符 < 字母数字标识符
    } else if (x !== y) {
      return x < y ? -1 : 1
    }
  }
  return 0
}

async function fetchLatestServerRelease(frontier: boolean): Promise<LatestServerRelease | null> {
  const res = await fetch(RELEASES_URL, {
    headers: { Accept: 'application/vnd.github+json' },
  })
  if (!res.ok) throw new Error(`github releases: ${res.status}`)
  const releases = (await res.json()) as GitHubRelease[]
  const re = frontier ? FRONTIER_TAG_RE : STABLE_TAG_RE
  // stable 排除 prerelease；frontier 放行。
  const matches = releases.filter(
    (r) => !r.draft && (frontier || !r.prerelease) && re.test(r.tag_name),
  )
  if (matches.length === 0) return null
  // stable 取 GitHub created_at 倒序首个；frontier 取 SemVer 最大。
  let best = matches[0]
  if (frontier) {
    for (const r of matches) {
      const bm = re.exec(best.tag_name)
      const rm = re.exec(r.tag_name)
      if (bm && rm && cmpVersion(rm[1], bm[1]) > 0) best = r
    }
  }
  const m = re.exec(best.tag_name)
  return m ? { version: m[1], url: best.html_url } : null
}

/** 启动后自动检测新版本；结果全会话缓存，失败静默。dev 构建（本地 `cargo run`，
 * 未经发布流水线注入版本号）跳过检测——`dev` 不是可比较的版本号，且本地开发
 * 无需被打扰更新提示。 */
export function useUpdateCheck(): UpdateState {
  const { data: info } = useQuery({ queryKey: ['info'], queryFn: api.info })
  const current = info?.version ?? null
  const isDev = current === 'dev'
  // 复用设置页的 config 查询键，切换渠道（web_update_channel）后自动重取。
  const { data: config } = useQuery({
    queryKey: ['config'],
    queryFn: api.getConfig,
    staleTime: Number.POSITIVE_INFINITY,
  })
  const frontier = config?.web_update_channel === 'frontier'
  const { data: latest } = useQuery({
    queryKey: ['latest-server-release', frontier],
    queryFn: () => fetchLatestServerRelease(frontier),
    staleTime: Number.POSITIVE_INFINITY,
    gcTime: Number.POSITIVE_INFINITY,
    retry: 1,
    enabled: !isDev,
  })

  return {
    current,
    latest: isDev ? null : (latest?.version ?? null),
    releaseUrl: isDev ? null : (latest?.url ?? null),
    hasUpdate: !isDev && current != null && latest != null && cmpVersion(latest.version, current) > 0,
  }
}
