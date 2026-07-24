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
    uploadedBytes: 0,
    uploadedAtCompletion: 0,
    seedingStatus: 0,
    seedingMessage: '',
    referrer: 'https://example.com/page',
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
        uploadSpeedBps: 0,
        uploadedBytes: 0,
        seedingStatus: 0,
        seedingMessage: '',
      ),
    );
    expect(updated.referrer, 'https://example.com/page');
  });

  test('copyWith preserves and overrides referrer', () {
    final task = DownloadTask.fromTaskInfo(makeInfo());
    expect(task.copyWith(status: TaskStatus.paused).referrer,
        'https://example.com/page');
  });
}
