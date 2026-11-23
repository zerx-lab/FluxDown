// 下载页命令的分派规划（纯函数）：按来源拆分本地 / 远程任务；本地任务 ≥2 个时合并为一次批量 RPC，
// 避免逐任务并发请求撑满 agent 的 daemon 通道并造成 N 次快照重发。
// 远程任务：宿主带 `agent.remoteBatch` 时按 `REMOTE_COMMAND_MANY_MAX` 分块走 `agent.remote.commandMany`，
// 旧宿主逐条 `agent.remote.command`（并发上限 `REMOTE_FALLBACK_CONCURRENCY`）；失败聚合成一条 toast。

import { APPLICATION_ERROR_CODE, METHOD, REMOTE_COMMAND_MANY_MAX, RpcError } from '../../../lib/rpc'
import type { RemoteCommandManyParams, RemoteCommandManyResult, RemoteCommandParams } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import type { TFunction } from '../../../i18n'
import { canResumeSeeding } from './task'
import type { DownloadTaskView } from './task'

export type LocalTaskAction = 'pause' | 'resume' | 'delete'

/**
 * 远程任务的云端命令是否适用（镜像 crates/downloads/src/model/dispatch.rs
 * `remote_action_applies`）：状态未知一律不控制；Pause 只对进行中，Resume 只对已暂停；
 * Delete 对任何已知状态成立。本地任务恒为 true。
 */
export function remoteCan(view: DownloadTaskView, action: RemoteCommandParams['action']): boolean {
  if (view.source === 'local') return true
  switch (view.remoteStatus) {
    case null:
    case 'unknown':
      return false
    case 'pending':
    case 'accepted':
    case 'downloading':
      return action === 'pause' || action === 'cancel' || action === 'delete'
    case 'paused':
      return action === 'resume' || action === 'cancel' || action === 'delete'
    default:
      return action === 'delete'
  }
}

export interface TaskCommandPlan {
  /** 本地任务 id（去重、保持选择顺序）；单个走单任务方法，多个走批量方法。 */
  localIds: string[]
  batch: boolean
  /** 适用的远程任务，经 `agent.remote.commandMany`（宿主带 `agent.remoteBatch`）或逐条 `agent.remote.command` 发送。 */
  remote: DownloadTaskView[]
}

/**
 * 已完成的本地任务只有处于可恢复做种态（`canResumeSeeding`）才参与「继续」：引擎对其余完成态的
 * resume 会从头重新下载（#935，镜像 crates/downloads `task_command`）。
 */
export function planTaskCommand(views: readonly DownloadTaskView[], action: LocalTaskAction): TaskCommandPlan {
  const localIds: string[] = []
  const seen = new Set<string>()
  const remote: DownloadTaskView[] = []
  for (const view of views) {
    if (!remoteCan(view, action)) continue
    if (view.source === 'local') {
      if (action === 'resume' && view.state === 'completed' && !canResumeSeeding(view)) continue
      if (!seen.has(view.taskId)) {
        seen.add(view.taskId)
        localIds.push(view.taskId)
      }
    } else {
      remote.push(view)
    }
  }
  return { localIds, batch: localIds.length > 1, remote }
}

/** 旧宿主（无 `agent.remoteBatch`）逐条回退 `agent.remote.command` 时的并发上限。 */
export const REMOTE_FALLBACK_CONCURRENCY = 4

/** 远程任务 id：去重、保持选择顺序。 */
export function remoteTaskIds(views: readonly DownloadTaskView[]): string[] {
  return [...new Set(views.map((view) => view.taskId))]
}

/** 按 `agent.remote.commandMany` 的单次上限分块。 */
export function chunkIds(ids: readonly string[], size = REMOTE_COMMAND_MANY_MAX): string[][] {
  const chunks: string[][] = []
  for (let index = 0; index < ids.length; index += size) chunks.push(ids.slice(index, index + size))
  return chunks
}

export interface RemoteFailure {
  taskId: string
  /** `RpcError`（批量失败项由 `RpcErrorData` 还原），或逐条路径上抛出的任意错误。 */
  error: unknown
}

export interface RemoteOutcome {
  done: number
  failed: RemoteFailure[]
}

export interface RemoteCommandDeps {
  /** 宿主是否声明 `agent.remoteBatch`。 */
  batch: boolean
  command: (params: RemoteCommandParams) => Promise<unknown>
  commandMany: (params: RemoteCommandManyParams) => Promise<RemoteCommandManyResult>
}

/**
 * 向远程任务下发同一动作；永不抛出，成功数与失败项汇总在返回值里。
 * 批量：每 `REMOTE_COMMAND_MANY_MAX` 个 id 一次 `commandMany`（整次调用失败则该块全部记为失败）；
 * 逐条：至多 `REMOTE_FALLBACK_CONCURRENCY` 个并发 `command`。
 */
export async function runRemoteCommands(
  ids: readonly string[],
  action: RemoteCommandParams['action'],
  deleteFiles: boolean,
  deps: RemoteCommandDeps,
): Promise<RemoteOutcome> {
  const outcome: RemoteOutcome = { done: 0, failed: [] }
  if (deps.batch) {
    for (const taskIds of chunkIds(ids)) {
      try {
        const result = await deps.commandMany(
          action === 'delete' ? { taskIds, action, deleteFiles } : { taskIds, action },
        )
        outcome.done += result.succeeded.length
        for (const failure of result.failed) {
          outcome.failed.push({
            taskId: failure.taskId,
            error: new RpcError({
              code: APPLICATION_ERROR_CODE,
              message: '',
              data: failure.error,
              method: METHOD.AGENT_REMOTE_COMMAND_MANY,
            }),
          })
        }
      } catch (error) {
        for (const taskId of taskIds) outcome.failed.push({ taskId, error })
      }
    }
    return outcome
  }
  let next = 0
  const worker = async (): Promise<void> => {
    while (next < ids.length) {
      const taskId = ids[next++]
      if (taskId === undefined) return
      try {
        await deps.command(action === 'delete' ? { taskId, action, deleteFiles } : { taskId, action })
        outcome.done += 1
      } catch (error) {
        outcome.failed.push({ taskId, error })
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(REMOTE_FALLBACK_CONCURRENCY, ids.length) }, worker))
  return outcome
}

export interface RemoteSummary {
  level: 'success' | 'error'
  text: string
}

/**
 * 远程批量结果 → 一条 toast：单条成功不提示（行随投影更新）；单条失败直接显示原因；
 * 多条全部成功 `remoteBatchDone`；多条有失败 `remoteBatchPartial`（原因取第一条失败）。
 */
export function summarizeRemote(outcome: RemoteOutcome, t: TFunction): RemoteSummary | null {
  const [first] = outcome.failed
  const total = outcome.done + outcome.failed.length
  if (!first) return total > 1 ? { level: 'success', text: t('remoteBatchDone', { n: outcome.done }) } : null
  const reason = rpcErrorText(first.error, t)
  if (total === 1) return { level: 'error', text: reason }
  return {
    level: 'error',
    text: t('remoteBatchPartial', { done: outcome.done, failed: outcome.failed.length, reason }),
  }
}
