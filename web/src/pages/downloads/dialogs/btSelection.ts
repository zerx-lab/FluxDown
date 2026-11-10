// BT 文件选择窗的保存目录规则：预填是任务当前目录；只有修剪后非空且不同于预填才下发，
// 引擎据此改目录后重开。空输入在预填非空时禁止确认（不能把已有目录清成空）。

/** 下发值；`undefined` = 沿用当前目录（协议缺省）。 */
export function btSelectionSaveDir(prefill: string, input: string): string | undefined {
  const trimmed = input.trim()
  if (trimmed === '' || trimmed === prefill.trim()) return undefined
  return trimmed
}

/** 没勾选文件，或预填非空却把输入清空时，不能确认。 */
export function btSelectionCanConfirm(prefill: string, input: string, selectedCount: number): boolean {
  if (selectedCount <= 0) return false
  return prefill.trim() === '' || input.trim() !== ''
}
