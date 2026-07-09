// 批量管理条：全选 / 已选计数 / 批量暂停恢复删除。仅在 manageMode 时渲染内容
// （所有 hooks 必须先于该判断无条件调用，满足 Rules of Hooks）。

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../../lib/api'
import { confirmDialog } from '../../lib/confirm'
import { filterTasks } from './filters'
import { useTasksUi } from './context'
import { useViewTasks } from './useViewTasks'

export function ManageBar() {
  const { manageMode, setManageMode, selected, setSelected, statusTab, typeFilter, queueFilter, search } = useTasksUi()
  const tasks = useViewTasks()
  const qc = useQueryClient()
  const invalidate = () => qc.invalidateQueries({ queryKey: ['tasks'] })

  const batchPause = useMutation({
    mutationFn: (ids: string[]) => Promise.all(ids.map((id) => api.pauseTask(id))),
    onSuccess: invalidate,
  })
  const batchContinue = useMutation({
    mutationFn: (ids: string[]) => Promise.all(ids.map((id) => api.continueTask(id))),
    onSuccess: invalidate,
  })
  const batchDelete = useMutation({
    mutationFn: (ids: string[]) => Promise.all(ids.map((id) => api.deleteTask(id, false))),
    onSuccess: () => {
      invalidate()
      setSelected(new Set())
    },
  })

  if (!manageMode) return null

  const visible = filterTasks(tasks, { statusTab, typeFilter, queueFilter, search })
  const allSelected = visible.length > 0 && visible.every((t) => selected.has(t.taskId))

  function toggleAll(checked: boolean) {
    setSelected(checked ? new Set(visible.map((t) => t.taskId)) : new Set())
  }

  return (
    <div className="manage-bar on">
      <label className="mcheck">
        <input type="checkbox" checked={allSelected} onChange={(e) => toggleAll(e.target.checked)} />
        <i />
        全选
      </label>
      <span>已选 {selected.size} 项</span>
      <span className="flex1" />
      <button
        type="button"
        className="btn ghost sm"
        disabled={selected.size === 0}
        onClick={() => batchPause.mutate(Array.from(selected))}
      >
        暂停
      </button>
      <button
        type="button"
        className="btn ghost sm"
        disabled={selected.size === 0}
        onClick={() => batchContinue.mutate(Array.from(selected))}
      >
        恢复
      </button>
      <button
        type="button"
        className="btn danger sm"
        disabled={selected.size === 0}
        onClick={async () => {
          if (selected.size > 0 && (await confirmDialog({ title: '删除任务', message: `删除选中的 ${selected.size} 个任务？`, danger: true }))) batchDelete.mutate(Array.from(selected))
        }}
      >
        删除
      </button>
      <button type="button" className="btn ghost sm" onClick={() => setManageMode(false)}>
        完成
      </button>
    </div>
  )
}
