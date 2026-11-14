// BT 文件选择窗的输入规则（与 GPUI `pages/selection.rs` 对齐）。
// 保存目录：预填是任务当前目录；只有修剪后非空且不同于预填才下发，引擎据此改目录后重开。
// 空输入在预填非空时禁止确认（不能把已有目录清成空）。
// 名称：预填是任务当前名称；同样只在修剪后非空且改过时下发，清空 = 沿用种子内名称。

/** 修剪后非空且不同于预填时返回修剪值，否则 `undefined`。 */
function changedInput(prefill: string, input: string): string | undefined {
  const trimmed = input.trim()
  if (trimmed === '' || trimmed === prefill.trim()) return undefined
  return trimmed
}

/** 下发值；`undefined` = 沿用当前目录（协议缺省）。 */
export function btSelectionSaveDir(prefill: string, input: string): string | undefined {
  return changedInput(prefill, input)
}

/** 下发的落盘名；`undefined` = 种子内名称（协议缺省）。清洗由引擎按文件名规则做。 */
export function btSelectionName(prefill: string, input: string): string | undefined {
  return changedInput(prefill, input)
}

/** 没勾选文件，或预填非空却把输入清空时，不能确认。 */
export function btSelectionCanConfirm(prefill: string, input: string, selectedCount: number): boolean {
  if (selectedCount <= 0) return false
  return prefill.trim() === '' || input.trim() !== ''
}
