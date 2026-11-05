// 交互选择（HLS 画质 / BT 文件 / 插件变体）。
// 流程：`daemon.selection.subscribe` → 收到 `selectionPending` 事件 → `daemon.selection.resolve`；
// 超过 `deadlineUnixMs` 服务端按 `defaultChoice` 自动决定。

export interface HlsQualityOptionDto {
  /** 用于 `SelectionOutcome.hls.index`。 */
  index: number;
  bandwidth: number;
  width: number;
  height: number;
}

export interface BtFileDto {
  index: number;
  path: string;
  size: number;
}

export interface ResolveVariantOptionDto {
  index: number;
  label: string;
  container: string;
  bandwidth: number;
  width: number;
  height: number;
  totalBytes: number;
}

/** 多插件择一的候选。 */
export interface PluginCandidateDto {
  identity: string;
  name: string;
  version: string;
  description: string;
}

/** 「文件已存在」可选动作；`cancelled` 结果 = 取消下载（任务暂停）。 */
export type FileExistsAction = 'rename' | 'overwrite' | 'skip';

/** 选择种类，`type` 内部标记。 */
export type SelectionKind =
  | { type: 'hls'; options: HlsQualityOptionDto[] }
  | { type: 'bt'; files: BtFileDto[]; /** 任务当前保存目录（主机路径，供预填；旧 daemon 缺省）。 */ saveDir?: string }
  | { type: 'variant'; options: ResolveVariantOptionDto[] }
  | {
      type: 'fileExists';
      fileName: string;
      saveDir: string;
      existingSize?: number;
      /** 已有文件修改时间（Unix 毫秒）。 */
      existingModifiedUnixMs?: number;
      /** 新下载的总大小；未知时缺省。 */
      incomingSize?: number;
      /** 选「重命名」时将落盘的文件名。 */
      renamePreview: string;
      actions: FileExistsAction[];
    }
  | {
      type: 'plugin';
      /** 命中多个插件的站点（如 `bilibili.com`）。 */
      site: string;
      /** 任务原始 URL。 */
      url: string;
      /** 候选插件，按 identity 升序。 */
      candidates: PluginCandidateDto[];
    };

/**
 * 选择结果，`kind` 内部标记。`bt.indices` 空数组 = 全部文件；
 * `cancelled` 对 HLS 会被拒绝（invalidArgument）；对 `fileExists` = 取消下载。
 */
export type SelectionOutcome =
  | { kind: 'hls'; index: number }
  | { kind: 'bt'; indices: number[]; /** 非空且不同于预填目录时改目录后重开；缺省 = 沿用。 */ saveDir?: string }
  | { kind: 'variant'; index: number }
  | { kind: 'fileExists'; action: FileExistsAction }
  | { kind: 'plugin'; identity: string; remember: boolean }
  | { kind: 'cancelled' };

/** 待选择请求（快照 `pendingSelections` / `selectionPending` 事件）。 */
export interface SelectionRequestDto {
  requestId: string;
  taskId: string;
  kind: SelectionKind;
  /** 超时自动采用的默认选择。 */
  defaultChoice: SelectionOutcome;
  deadlineUnixMs: number;
}

/** `daemon.selection.resolve` 参数。 */
export interface SelectionResolutionDto {
  requestId: string;
  outcome: SelectionOutcome;
}
