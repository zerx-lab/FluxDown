import { CircleAlert, CircleCheck } from 'lucide-react'
import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { useT } from '../../../i18n'
import { fileHashDisplay, fileHashMatchesChecksum, hashAlgorithmLabel } from '../../../lib/fileHash'
import { HASH_ALGORITHMS } from '../../../lib/rpc'
import { Button, Icon, Select, Tooltip } from '../../../ui'
import { computeTaskFileHash } from '../model/actions'
import type { DownloadTaskView } from '../model/task'
import { CopyButton } from './CopyButton'
import { DetailRow } from './DetailRow'

type HashAlgorithm = (typeof HASH_ALGORITHMS)[number]

const HASH_OPTIONS = HASH_ALGORITHMS.map((algo) => ({ value: algo, label: hashAlgorithmLabel(algo) }))

// 在途任务集合放在模块级：关闭详情 / 切换任务再回来，按钮仍反映该任务的请求是否在途。
let hashing: ReadonlySet<string> = new Set()
const listeners = new Set<() => void>()

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

function setHashing(taskId: string, active: boolean): void {
  if (hashing.has(taskId) === active) return
  const next = new Set(hashing)
  if (active) next.add(taskId)
  else next.delete(taskId)
  hashing = next
  for (const listener of listeners) listener()
}

/** 已记入在途集合的任务发起计算，结束后撤掉标记。 */
async function finishHash(view: DownloadTaskView, algorithm: HashAlgorithm, notify: boolean): Promise<void> {
  try {
    await computeTaskFileHash(view, algorithm, notify)
  } finally {
    setHashing(view.taskId, false)
  }
}

/** 摘要中间省略时固定保留的尾部字符数（头部按可用宽度截断）。 */
const DIGEST_TAIL = 12

/** 打开详情后停留多久才自动计算（与 GPUI `AUTO_FILE_HASH_DELAY` 一致）：方向键浏览不逐行读盘。 */
const AUTO_HASH_DELAY_MS = 600

/**
 * 常规页「文件哈希」行，与 GPUI 同一布局：标签位是算法下拉（即摘要的标签），值位是
 * `摘要（中间省略，悬浮看全文）[✓/✕] [复制]`。尚无任何记录的任务打开后短暂停留即自动计算
 * （失败不提示，回落为「计算」按钮）；切到尚无记录的算法同样即时计算。
 * 调用方只对已完成的本地任务渲染；哈希只来自任务行（daemon 写回后先发布 TaskChanged 再回执）。
 */
export function FileHashRow({ view }: { view: DownloadTaskView }) {
  const t = useT()
  const inflight = useSyncExternalStore(subscribe, () => hashing, () => hashing)
  const recorded = view.fileHash || ''
  const computing = inflight.has(view.taskId)
  const [picked, setPicked] = useState<{ taskId: string; forHash: string; algo: HashAlgorithm } | null>(null)
  const parsed = recorded === '' ? null : fileHashDisplay(recorded)
  const recordedAlgo = parsed?.algo ?? null
  const algorithm = picked && picked.taskId === view.taskId && picked.forHash === recorded ? picked.algo : (recordedAlgo ?? 'sha256')
  const digest = parsed && recordedAlgo === algorithm ? parsed.hex : null
  const match = digest === null ? null : fileHashMatchesChecksum(recorded, view.dto?.checksum ?? '')

  async function compute(algo: HashAlgorithm, notify = true) {
    if (hashing.has(view.taskId)) return
    setHashing(view.taskId, true)
    await finishHash(view, algo, notify)
  }

  // 打开即计算：每个任务只发起一次（切到别的任务再回来重新计时），失败后不反复重试。
  // 停留期就把任务记入在途集合，行内直接显示「计算中」，不先闪一下「计算」按钮。
  const autoTried = useRef<string | null>(null)
  const autoEligible = recorded === '' && !view.fileMissing
  useEffect(() => {
    const taskId = view.taskId
    if (!autoEligible || autoTried.current === taskId || hashing.has(taskId)) return
    autoTried.current = taskId
    setHashing(taskId, true)
    let fired = false
    const timer = setTimeout(() => {
      fired = true
      void finishHash(view, algorithm, false)
    }, AUTO_HASH_DELAY_MS)
    return () => {
      if (fired) return
      // 尚未发起就被撤销（离开 / 关闭 / StrictMode 重挂）：撤掉在途标记，下次重新计时。
      clearTimeout(timer)
      setHashing(taskId, false)
      if (autoTried.current === taskId) autoTried.current = null
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 只按任务与资格重排；算法变化走 pick 即时计算
  }, [view.taskId, autoEligible])

  function pick(next: HashAlgorithm) {
    if (next === algorithm) return
    setPicked({ taskId: view.taskId, forHash: recorded, algo: next })
    if (next !== recordedAlgo && !view.fileMissing) void compute(next)
  }

  let value
  if (computing) {
    value = <span className="text-muted-foreground">{t('fileHashComputing')}</span>
  } else if (digest !== null) {
    const head = digest.length > DIGEST_TAIL ? digest.slice(0, -DIGEST_TAIL) : digest
    const tail = digest.length > DIGEST_TAIL ? digest.slice(-DIGEST_TAIL) : ''
    value = (
      <>
        <span className="flex min-w-0 flex-1 font-mono tabular-nums" title={digest}>
          <span className="min-w-0 truncate">{head}</span>
          <span className="shrink-0 whitespace-nowrap">{tail}</span>
        </span>
        {match !== null ? (
          <Tooltip content={t(match ? 'fileHashMatch' : 'fileHashMismatch')}>
            <span className="inline-flex shrink-0" aria-label={t(match ? 'fileHashMatch' : 'fileHashMismatch')}>
              <Icon icon={match ? CircleCheck : CircleAlert} className={match ? 'text-success' : 'text-destructive'} />
            </span>
          </Tooltip>
        ) : null}
        <CopyButton text={digest} label={t('fileHashCopy')} copiedLabel={t('fileHashCopied')} />
      </>
    )
  } else if (view.fileMissing) {
    value = <span className="text-text-tertiary">{t('fileHashNotComputed')}</span>
  } else {
    value = (
      <Button variant="outline" onClick={() => void compute(algorithm)}>
        {t('fileHashCompute')}
      </Button>
    )
  }

  // 标签位的下拉：去掉边框与背景、沿用标签的 xs 三级文字，负外边距抵消内边距使文字与其它行标签左缘对齐。
  const label = (
    <Select
      className="-ml-2 w-auto border-transparent bg-transparent px-2 text-xs text-text-tertiary"
      aria-label={t('hashAlgorithm')}
      value={algorithm}
      disabled={computing || view.fileMissing}
      options={HASH_OPTIONS}
      onValueChange={pick}
    />
  )

  return (
    <DetailRow label={label}>
      <div className="flex min-w-0 items-center gap-2">{value}</div>
    </DetailRow>
  )
}
