import 'dart:io';
import 'dart:math';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';

/// 任务状态 — 与 Rust 端状态码对应
/// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
/// resuming 为纯 Dart 端状态，点击继续后立即切换，Rust 返回 status=1 后自动过渡到 downloading
enum TaskStatus {
  pending,
  downloading,
  paused,
  completed,
  error,
  resuming,
  preparing,
}

/// 文件类型分类 — 由扩展名推断
enum FileCategory {
  all,
  video,
  audio,
  document,
  image,
  archive,
  other;

  String get label {
    final s = currentS;
    return switch (this) {
      FileCategory.all => s.categoryAll,
      FileCategory.video => s.categoryVideo,
      FileCategory.audio => s.categoryAudio,
      FileCategory.document => s.categoryDocument,
      FileCategory.image => s.categoryImage,
      FileCategory.archive => s.categoryArchive,
      FileCategory.other => s.categoryOther,
    };
  }

  static const _videoExts = {
    'mp4',
    'mkv',
    'avi',
    'mov',
    'wmv',
    'flv',
    'webm',
    'ts',
    'm4v',
    'rmvb',
    'rm',
    '3gp',
    'vob',
    'mpg',
    'mpeg',
  };
  static const _audioExts = {
    'mp3',
    'flac',
    'wav',
    'aac',
    'ogg',
    'wma',
    'm4a',
    'opus',
    'ape',
    'aiff',
  };
  static const _docExts = {
    'pdf',
    'doc',
    'docx',
    'xls',
    'xlsx',
    'ppt',
    'pptx',
    'txt',
    'csv',
    'rtf',
    'epub',
    'mobi',
    'md',
    'odt',
    'ods',
    'odp',
  };
  static const _imageExts = {
    'jpg',
    'jpeg',
    'png',
    'gif',
    'bmp',
    'webp',
    'svg',
    'ico',
    'tiff',
    'tif',
    'psd',
    'raw',
    'heic',
    'avif',
  };
  static const _archiveExts = {
    'zip',
    'rar',
    '7z',
    'tar',
    'gz',
    'bz2',
    'xz',
    'zst',
    'iso',
    'dmg',
    'cab',
    'lz',
    'lzma',
  };

  /// 根据文件扩展名推断分类
  static FileCategory fromExtension(String ext) {
    final e = ext.toLowerCase();
    if (_videoExts.contains(e)) return FileCategory.video;
    if (_audioExts.contains(e)) return FileCategory.audio;
    if (_docExts.contains(e)) return FileCategory.document;
    if (_imageExts.contains(e)) return FileCategory.image;
    if (_archiveExts.contains(e)) return FileCategory.archive;
    return FileCategory.other;
  }
}

TaskStatus taskStatusFromInt(int value) {
  return switch (value) {
    0 => TaskStatus.pending,
    1 => TaskStatus.downloading,
    2 => TaskStatus.paused,
    3 => TaskStatus.completed,
    4 => TaskStatus.error,
    5 => TaskStatus.preparing,
    _ => TaskStatus.error,
  };
}

/// Per-segment progress data for IDM-style visualization
class SegmentData {
  final int index;
  final int startByte;
  final int endByte;
  final int downloadedBytes;

  const SegmentData({
    required this.index,
    required this.startByte,
    required this.endByte,
    required this.downloadedBytes,
  });

  /// Segment size in bytes
  int get size => endByte - startByte + 1;

  /// Progress [0.0, 1.0]
  double get progress =>
      size > 0 ? (downloadedBytes / size).clamp(0.0, 1.0) : 0;
}

/// Record of a dynamic segment split event from the coordinator.
/// Used to trigger split animations in the detail panel.
class SplitEventData {
  final int parentIndex;
  final int parentNewEnd;
  final int childIndex;
  final int childStart;
  final int childEnd;
  final bool isProactive;
  final int totalSegments;

  const SplitEventData({
    required this.parentIndex,
    required this.parentNewEnd,
    required this.childIndex,
    required this.childStart,
    required this.childEnd,
    required this.isProactive,
    required this.totalSegments,
  });
}

class DownloadTask {
  final String id;
  final String url;
  final String fileName;
  final String saveDir;
  final TaskStatus status;
  final int downloadedBytes;
  final int totalBytes;
  final int speed; // bytes per second
  final String errorMessage;
  final bool isSelected;
  final DateTime createdAt;

  /// Per-segment progress data (null if no segment info received yet)
  final List<SegmentData>? segments;

  /// Recent split events (for animation). Kept for a short window then cleared.
  final List<SplitEventData> recentSplits;

  /// 在 pending_queue 中的排队位置（1-based）。-1 = 不在队列中。
  final int queuePosition;

  /// 所属命名队列 ID（空字符串 = 默认队列）。
  final String queueId;

  /// 文件名是否已由 Rust 引擎或 DB 确认（非占位符）。
  ///
  /// 设为 true 的时机：
  ///   - [fromTaskInfo]：DB 中有非空文件名
  ///   - [applyProgress]：收到 Rust 下载引擎发来的非空 file_name
  ///
  /// 用途：阻止后台 meta_prober 的 [TaskMetaProbed] 信号覆盖用户已设置的
  /// 自定义文件名。只要此字段为 true，probe 结果中的文件名将被忽略。
  final bool fileNameConfirmed;

  /// 文件跟踪：completed 任务的目标文件在磁盘上是否已丢失（被删除/移动）。
  /// 由引擎扫描后经 FileMissingChanged / AllTasks 下发，仅对 completed 有意义。
  final bool fileMissing;

  DownloadTask({
    required this.id,
    required this.url,
    required this.fileName,
    required this.saveDir,
    required this.status,
    required this.downloadedBytes,
    required this.totalBytes,
    this.speed = 0,
    this.errorMessage = '',
    this.isSelected = false,
    this.segments,
    this.recentSplits = const [],
    this.queuePosition = -1,
    this.queueId = '',
    this.fileNameConfirmed = false,
    this.fileMissing = false,
    DateTime? createdAt,
  }) : createdAt = createdAt ?? DateTime.now();

  /// 从 AllTasks 信号中的 TaskInfo 构建
  factory DownloadTask.fromTaskInfo(TaskInfo info) {
    final seconds = int.tryParse(info.createdAt) ?? 0;
    // DB 中有非空文件名，说明 Rust 已确认过（create_task 写入的用户名或
    // 下载引擎 update_task_file_info 写入的实际名），标记为已确认。
    final hasName = info.fileName.isNotEmpty;
    return DownloadTask(
      id: info.taskId,
      url: info.url,
      fileName: hasName ? info.fileName : currentS.unknownFile,
      saveDir: info.saveDir,
      status: taskStatusFromInt(info.status),
      downloadedBytes: info.downloadedBytes,
      totalBytes: info.totalBytes,
      errorMessage: info.errorMessage,
      queueId: info.queueId,
      fileNameConfirmed: hasName,
      fileMissing: info.fileMissing,
      createdAt: seconds > 0
          ? DateTime.fromMillisecondsSinceEpoch(seconds * 1000)
          : DateTime.now(),
    );
  }

  DownloadTask copyWith({
    String? id,
    String? url,
    String? fileName,
    String? saveDir,
    TaskStatus? status,
    int? downloadedBytes,
    int? totalBytes,
    int? speed,
    String? errorMessage,
    bool? isSelected,
    List<SegmentData>? segments,
    List<SplitEventData>? recentSplits,
    int? queuePosition,
    String? queueId,
    bool? fileNameConfirmed,
    bool? fileMissing,
    DateTime? createdAt,
  }) {
    return DownloadTask(
      id: id ?? this.id,
      url: url ?? this.url,
      fileName: fileName ?? this.fileName,
      saveDir: saveDir ?? this.saveDir,
      status: status ?? this.status,
      downloadedBytes: downloadedBytes ?? this.downloadedBytes,
      totalBytes: totalBytes ?? this.totalBytes,
      speed: speed ?? this.speed,
      errorMessage: errorMessage ?? this.errorMessage,
      isSelected: isSelected ?? this.isSelected,
      segments: segments ?? this.segments,
      recentSplits: recentSplits ?? this.recentSplits,
      queuePosition: queuePosition ?? this.queuePosition,
      queueId: queueId ?? this.queueId,
      fileNameConfirmed: fileNameConfirmed ?? this.fileNameConfirmed,
      fileMissing: fileMissing ?? this.fileMissing,
      createdAt: createdAt ?? this.createdAt,
    );
  }

  /// 根据 TaskProgress 信号增量更新
  DownloadTask applyProgress(TaskProgress p) {
    final newStatus = taskStatusFromInt(p.status);
    // Rust 端已通过固定窗口采样 + 单层 EMA 充分平滑，Dart 直接使用。
    // 非下载状态强制归零，防止残留值。
    final int displaySpeed = newStatus == TaskStatus.downloading ? p.speed : 0;

    // 收到 Rust 下载引擎发来的非空文件名，视为已确认（用户输入或引擎解析）。
    // 一旦确认，后续 TaskMetaProbed 不再覆盖此名字。
    final nameFromProgress = p.fileName.isNotEmpty ? p.fileName : null;
    final nowConfirmed = fileNameConfirmed || nameFromProgress != null;

    return copyWith(
      status: newStatus,
      downloadedBytes: p.downloadedBytes,
      totalBytes: p.totalBytes > 0 ? p.totalBytes : null,
      speed: displaySpeed,
      fileName: nameFromProgress,
      saveDir: p.saveDir.isNotEmpty ? p.saveDir : null,
      errorMessage: p.errorMessage,
      fileNameConfirmed: nowConfirmed,
    );
  }

  // ---------------------------------------------------------------------------
  // Computed properties
  // ---------------------------------------------------------------------------

  /// 下载进度 [0.0, 1.0]
  double get progress {
    // 已完成的任务强制返回 100%，避免未知大小文件完成后仍显示 0%
    if (status == TaskStatus.completed) return 1.0;
    if (totalBytes <= 0) return 0;
    // 上限 0.999 而非 1.0：Rust 层 BT 下载在 finished=false 时已将 downloaded_bytes
    // 限制为 total_bytes-1，但 (total_bytes-1)/total_bytes 经浮点运算后对大文件
    // 仍会被 toStringAsFixed(1) 四舍五入为 "100.0%"，造成进度已到 100% 但状态仍
    // 显示"下载中"的视觉误导。限制为 0.999 确保未完成任务最多显示 "99.9%"。
    return (downloadedBytes / totalBytes).clamp(0.0, 0.999);
  }

  /// 是否为不确定进度（文件大小未知且处于活跃下载阶段）
  bool get isIndeterminate =>
      totalBytes <= 0 &&
      (status == TaskStatus.downloading ||
          status == TaskStatus.preparing ||
          status == TaskStatus.resuming);

  /// 文件扩展名（用于图标显示）
  String get fileExtension {
    final dot = fileName.lastIndexOf('.');
    if (dot < 0 || dot == fileName.length - 1) return '?';
    return fileName.substring(dot + 1).toLowerCase();
  }

  /// 文件类型分类
  FileCategory get fileCategory => FileCategory.fromExtension(fileExtension);

  /// 任务目标文件的完整路径（`saveDir` + 分隔符 + `fileName`）。
  ///
  /// 拼接时去重 `saveDir` 末尾可能存在的路径分隔符，避免产生重复分隔符；
  /// `saveDir` 为空时退回裸文件名。作为文件路径拼接的单一事实来源，替代散落
  /// 各处的手写 `'${saveDir}${sep}${fileName}'`。
  String get filePath {
    if (saveDir.isEmpty) return fileName;
    final separator = Platform.pathSeparator;
    final dir = saveDir.endsWith(separator)
        ? saveDir.substring(0, saveDir.length - separator.length)
        : saveDir;
    return '$dir$separator$fileName';
  }

  /// 「打开所在文件夹」应传给原生层的路径。
  ///
  /// 已完成且文件存在时返回完整文件路径，便于文件管理器定位并选中文件；下载中、
  /// 暂停、失败、排队、准备中、文件丢失等状态下最终文件可能尚未落盘，改为返回
  /// 保存目录 [saveDir]，避免原生层将不存在的文件路径误判后打不开任何位置。
  /// [saveDir] 为空时退回文件路径。
  String get revealFolderPath {
    if (status == TaskStatus.completed && !fileMissing) return filePath;
    if (saveDir.isNotEmpty) return saveDir;
    return filePath;
  }

  /// 格式化文件大小
  String get sizeText {
    if (totalBytes <= 0) return currentS.unknownSize;
    return formatBytes(totalBytes);
  }

  /// 格式化已下载
  String get downloadedText => formatBytes(downloadedBytes);

  /// 格式化速度
  String get speedText {
    if (speed <= 0) return '—';
    return '${formatBytes(speed)}/s';
  }

  /// 协议类型标识
  String get protocolLabel {
    final lower = url.toLowerCase();
    if (lower.startsWith('magnet:')) return 'BT';
    if (lower.startsWith('torrent-file://')) return 'BT';
    if (lower.startsWith('ftp://')) return 'FTP';
    if (lower.startsWith('ed2k://')) return 'ED2K';
    return 'HTTP';
  }

  /// 副标题信息
  String get subtitle {
    final s = currentS;
    final proto = protocolLabel;
    switch (status) {
      case TaskStatus.downloading:
        return '$proto · $sizeText · $speedText';
      case TaskStatus.paused:
        return '$proto · $sizeText · ${s.subtitlePaused}';
      case TaskStatus.completed:
        return '$proto · $sizeText';
      case TaskStatus.error:
        return '$proto · $sizeText · ${errorMessage.isEmpty ? s.subtitleError : errorMessage}';
      case TaskStatus.pending:
        final queueStr = queuePosition > 0
            ? ' · ${s.subtitleQueued(queuePosition)}'
            : '';
        if (totalBytes > 0) return '$proto · $sizeText$queueStr';
        return '$proto · ${s.subtitlePending}$queueStr';
      case TaskStatus.preparing:
        return '$proto · ${s.subtitlePreparing}';
      case TaskStatus.resuming:
        return '$proto · $sizeText · ${s.subtitleResuming}';
    }
  }

  /// 状态文本
  String get statusText {
    final s = currentS;
    if (status == TaskStatus.completed && fileMissing) {
      return s.statusFileMissing;
    }
    return switch (status) {
      TaskStatus.pending => s.statusPending,
      TaskStatus.downloading => s.statusDownloading,
      TaskStatus.paused => s.statusPaused,
      TaskStatus.completed => s.statusCompleted,
      TaskStatus.error => s.statusError,
      TaskStatus.preparing => s.statusPreparing,
      TaskStatus.resuming => s.statusResuming,
    };
  }

  /// 剩余时间估算
  String get etaText {
    if (status != TaskStatus.downloading || speed <= 0 || totalBytes <= 0) {
      return '—';
    }
    final remaining = totalBytes - downloadedBytes;
    // 已下载超过或等于总大小，即将完成（等待写盘/校验）
    if (remaining <= 0) return '—';
    final seconds = remaining / speed;
    // ETA 超过 24 小时视为不可靠，不显示
    if (seconds > 86400) return '—';
    final s = currentS;
    if (seconds < 60) return s.etaSeconds(seconds.toInt());
    if (seconds < 3600) return s.etaMinutes((seconds / 60).toInt());
    return s.etaHours((seconds / 3600).toStringAsFixed(1));
  }

  // ---------------------------------------------------------------------------
  // Utility
  // ---------------------------------------------------------------------------

  static String formatBytes(int bytes) {
    if (bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    final i = (log(bytes) / log(1024)).floor().clamp(0, units.length - 1);
    final value = bytes / pow(1024, i);
    return '${value.toStringAsFixed(value >= 100 ? 0 : 1)} ${units[i]}';
  }
}

// =============================================================================
// 时间分组
// =============================================================================

/// 时间分组类型 — 按创建时间将任务分入不同组
enum TimeGroup {
  today,
  yesterday,
  thisWeek,
  thisMonth,
  older;

  String get label {
    final s = currentS;
    return switch (this) {
      TimeGroup.today => s.today,
      TimeGroup.yesterday => s.yesterday,
      TimeGroup.thisWeek => s.thisWeek,
      TimeGroup.thisMonth => s.thisMonth,
      TimeGroup.older => s.older,
    };
  }

  /// 根据创建时间判断属于哪个分组
  static TimeGroup fromDateTime(DateTime createdAt) {
    final now = DateTime.now();
    final today = DateTime(now.year, now.month, now.day);
    final yesterday = today.subtract(const Duration(days: 1));
    final weekAgo = today.subtract(const Duration(days: 7));
    final monthAgo = today.subtract(const Duration(days: 30));

    if (!createdAt.isBefore(today)) return TimeGroup.today;
    if (!createdAt.isBefore(yesterday)) return TimeGroup.yesterday;
    if (!createdAt.isBefore(weekAgo)) return TimeGroup.thisWeek;
    if (!createdAt.isBefore(monthAgo)) return TimeGroup.thisMonth;
    return TimeGroup.older;
  }
}

/// 任务分组数据
class TaskGroup {
  /// null 表示「活跃任务组」（正在下载 + 排队），不可折叠
  final TimeGroup? group;
  final List<DownloadTask> tasks;

  const TaskGroup({this.group, required this.tasks});

  /// 是否为活跃任务组（不按时间分组，不可折叠）
  bool get isActiveGroup => group == null;
}

// =============================================================================
// TaskStatus 扩展
// =============================================================================

extension TaskStatusExt on TaskStatus {
  /// 是否为"活跃"状态（正在下载 / 准备 / 恢复中）
  bool get isActive =>
      this == TaskStatus.downloading ||
      this == TaskStatus.preparing ||
      this == TaskStatus.resuming;

  /// 是否为"活跃或排队"状态（置顶显示）
  bool get isActiveOrQueued => isActive || this == TaskStatus.pending;
}
