// 不建任务的种子 / 磁力预览（torrent_preview.rs）。文件下标一律沿用种子内的原始顺序。
import type { BtFileDto } from './selection';
import type { CreateTaskRequest } from './task';

/** `daemon.torrent.prepareMagnet` 参数。 */
export interface MagnetPreviewParams {
  url: string;
}

/** 一次性的预览预约，须在两分钟内解析或取消。 */
export interface MagnetPreviewTicket {
  previewId: string;
}

/** `agent.capture.previewTorrentFile` 参数：由 agent 读取的本机文件，不建任务。 */
export interface TorrentFilePreviewParams {
  path: string;
}

/** 已上传的种子，保留到确认、显式丢弃或过期为止。 */
export interface TorrentPreviewParams {
  torrentBlobId: string;
}

export interface TorrentPreviewDto {
  torrentBlobId: string;
  name: string;
  totalBytes: number;
  files: BtFileDto[];
}

/** `daemon.torrent.confirm` 参数：显式确认，至少选中一个（种子内原始下标的）文件。 */
export interface TorrentConfirmParams {
  torrentBlobId: string;
  saveDir: string;
  selectedFileIndices: number[];
  queueId?: string;
  startPaused?: boolean;
  /**
   * 落盘名（空 / 缺省 = 用种子内的原始名称）。必须是单个路径组件；
   * 多文件种子改的是外层容器目录名。
   */
  customName?: string;
  sourceUrl?: string;
  options?: CreateTaskRequest;
}
