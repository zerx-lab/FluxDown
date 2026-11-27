// 订阅刷新结果的展示判定（GPUI `subscription::RefreshStatus::from_response` 同语义）。

export interface RefreshResponse {
  success: boolean
  error: string
}

/** `failed`：全部源失败；`partial`：成功但有源失败（`detail` = daemon 的失败源摘要）；`ok`：全部成功。 */
export type RefreshStatus = { kind: 'ok' } | { kind: 'failed' } | { kind: 'partial'; detail: string }

export function classifyRefresh(result: RefreshResponse): RefreshStatus {
  if (!result.success) return { kind: 'failed' }
  const detail = result.error.trim()
  return detail === '' ? { kind: 'ok' } : { kind: 'partial', detail }
}
