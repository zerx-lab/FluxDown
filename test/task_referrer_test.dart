// Repro for issue #111: DownloadTask must carry the referrer from TaskInfo
// and periodic TaskProgress updates must not clobber it.
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/download_task.dart';

void main() {
  TaskInfo makeInfo() => const TaskInfo(
    taskId: 't1',
    url: 'https://example.com/f.zip',
    fileName: 'f.zip',
    saveDir: '/tmp',
    status: 2,
    downloadedBytes: 10,
    totalBytes: 100,
    errorMessage: '',
    createdAt: '1700000000',
    proxyUrl: '',
    queueId: '',
    checksum: '',
    ignoreTlsErrors: false,
    fileMissing: false,
    completedAt: '',
    segments: 0,
    queueOrder: 0,
    referrer: 'https://example.com/page',
    groupId: '',
  );

  test('fromTaskInfo maps referrer', () {
    final task = DownloadTask.fromTaskInfo(makeInfo());
    expect(task.referrer, 'https://example.com/page');
  });

  test('applyProgress preserves referrer', () {
    final task = DownloadTask.fromTaskInfo(makeInfo());
    final updated = task.applyProgress(
      const TaskProgress(
        taskId: 't1',
        status: 1,
        downloadedBytes: 50,
        totalBytes: 100,
        speed: 1000,
        fileName: 'f.zip',
        saveDir: '/tmp',
        url: 'https://example.com/f.zip',
        errorMessage: '',
      ),
    );
    expect(updated.referrer, 'https://example.com/page');
  });

  test('copyWith preserves and overrides referrer', () {
    final task = DownloadTask.fromTaskInfo(makeInfo());
    expect(task.copyWith(status: TaskStatus.paused).referrer,
        'https://example.com/page');
  });

  test('fromTaskInfo maps checksum and proxyUrl', () {
    const info = TaskInfo(
      taskId: 't2',
      url: 'https://example.com/f.zip',
      fileName: 'f.zip',
      saveDir: '/tmp',
      status: 2,
      downloadedBytes: 10,
      totalBytes: 100,
      errorMessage: '',
      createdAt: '1700000000',
      proxyUrl: 'socks5://127.0.0.1:1080',
      queueId: '',
      checksum: 'sha256=deadbeef',
      ignoreTlsErrors: false,
      fileMissing: false,
      completedAt: '',
      segments: 0,
      queueOrder: 0,
      referrer: '',
      groupId: '',
    );
    final task = DownloadTask.fromTaskInfo(info);
    expect(task.checksum, 'sha256=deadbeef');
    expect(task.proxyUrl, 'socks5://127.0.0.1:1080');
  });

  test('fromTaskInfo defaults checksum/proxyUrl to empty when absent', () {
    final task = DownloadTask.fromTaskInfo(makeInfo());
    expect(task.checksum, '');
    expect(task.proxyUrl, '');
  });

  test('copyWith overrides checksum/proxyUrl independently', () {
    final task = DownloadTask.fromTaskInfo(makeInfo()).copyWith(
      checksum: 'md5=abc',
      proxyUrl: 'http://proxy:8080',
    );
    expect(task.checksum, 'md5=abc');
    expect(task.proxyUrl, 'http://proxy:8080');
    // 未再次指定时保留原值（不被其它字段的 copyWith 调用清空）。
    final unchanged = task.copyWith(status: TaskStatus.paused);
    expect(unchanged.checksum, 'md5=abc');
    expect(unchanged.proxyUrl, 'http://proxy:8080');
  });
}
