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
    DateTime? createdAt,
  }) : createdAt = createdAt ?? DateTime.now();

  /// 从 AllTasks 信号中的 TaskInfo 构建
  factory DownloadTask.fromTaskInfo(TaskInfo info) {
    final seconds = int.tryParse(info.createdAt) ?? 0;
    return DownloadTask(
      id: info.taskId,
      url: info.url,
      fileName: info.fileName.isEmpty ? currentS.unknownFile : info.fileName,
      saveDir: info.saveDir,
      status: taskStatusFromInt(info.status),
      downloadedBytes: info.downloadedBytes,
      totalBytes: info.totalBytes,
      errorMessage: info.errorMessage,
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
      createdAt: createdAt ?? this.createdAt,
    );
  }

  /// 根据 TaskProgress 信号增量更新
  DownloadTask applyProgress(TaskProgress p) {
    // Dart-side EMA smoothing for speed display (α = 0.3).
    // Rust already sends EMA-smoothed speed; this second pass further damps
    // any residual jitter from multi-segment reporting.
    final newStatus = taskStatusFromInt(p.status);
    final int smoothedSpeed;
    if (newStatus == TaskStatus.downloading && p.speed > 0) {
      if (speed > 0) {
        smoothedSpeed = (0.3 * p.speed + 0.7 * speed).round();
      } else {
        smoothedSpeed = p.speed; // first update — use raw value
      }
    } else {
      smoothedSpeed = p.speed;
    }

    return copyWith(
      status: newStatus,
      downloadedBytes: p.downloadedBytes,
      totalBytes: p.totalBytes > 0 ? p.totalBytes : null,
      speed: smoothedSpeed,
      fileName: p.fileName.isNotEmpty ? p.fileName : null,
      saveDir: p.saveDir.isNotEmpty ? p.saveDir : null,
      errorMessage: p.errorMessage,
    );
  }

  // ---------------------------------------------------------------------------
  // Computed properties
  // ---------------------------------------------------------------------------

  /// 下载进度 [0.0, 1.0]
  double get progress {
    if (totalBytes <= 0) return 0;
    return (downloadedBytes / totalBytes).clamp(0.0, 1.0);
  }

  /// 文件扩展名（用于图标显示）
  String get fileExtension {
    final dot = fileName.lastIndexOf('.');
    if (dot < 0 || dot == fileName.length - 1) return '?';
    return fileName.substring(dot + 1).toLowerCase();
  }

  /// 文件类型分类
  FileCategory get fileCategory => FileCategory.fromExtension(fileExtension);

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

  /// 副标题信息
  String get subtitle {
    final s = currentS;
    switch (status) {
      case TaskStatus.downloading:
        return 'HTTP · $sizeText · $speedText';
      case TaskStatus.paused:
        return 'HTTP · $sizeText · ${s.subtitlePaused}';
      case TaskStatus.completed:
        return 'HTTP · $sizeText';
      case TaskStatus.error:
        return 'HTTP · $sizeText · ${errorMessage.isEmpty ? s.subtitleError : errorMessage}';
      case TaskStatus.pending:
        return 'HTTP · ${s.subtitlePending}';
      case TaskStatus.preparing:
        return 'HTTP · ${s.subtitlePreparing}';
      case TaskStatus.resuming:
        return 'HTTP · $sizeText · ${s.subtitleResuming}';
    }
  }

  /// 状态文本
  String get statusText {
    final s = currentS;
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
    final seconds = remaining / speed;
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
    final monthAgo = DateTime(now.year, now.month - 1, now.day);

    if (createdAt.isAfter(today)) return TimeGroup.today;
    if (createdAt.isAfter(yesterday)) return TimeGroup.yesterday;
    if (createdAt.isAfter(weekAgo)) return TimeGroup.thisWeek;
    if (createdAt.isAfter(monthAgo)) return TimeGroup.thisMonth;
    return TimeGroup.older;
  }
}

/// 任务分组数据
class TaskGroup {
  final TimeGroup group;
  final List<DownloadTask> tasks;

  const TaskGroup({required this.group, required this.tasks});
}
