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
    groupId: '',
    rssSourceId: '',
    originUrl: '',
    autoRoute: '',
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
      rssSourceId: '',
      originUrl: '',
      autoRoute: '',
      uploadedBytes: 0,
      uploadedAtCompletion: 0,
      seedingStatus: 0,
      seedingMessage: '',
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

  // RSS 自动建的 BT 任务 url 是 `torrent-file://local` 哨兵，右键「复制下载
  // 链接」必须给出真实来源，否则用户复制到的是一段无法交给任何工具的噪音。
  test('shareUrl prefers originUrl over the torrent-file sentinel', () {
    final sentinel = DownloadTask.fromTaskInfo(makeInfo()).copyWith(
      url: 'torrent-file://local',
      originUrl: 'https://mikanani.me/Download/ep01.torrent',
    );
    expect(sentinel.shareUrl, 'https://mikanani.me/Download/ep01.torrent');

    // 没有原始来源（手动拖入的本地 .torrent）时回退 url，不能返回空串。
    final plain = DownloadTask.fromTaskInfo(makeInfo());
    expect(plain.originUrl, '');
    expect(plain.shareUrl, 'https://example.com/f.zip');

    // copyWith 不得在改别的字段时把它抹掉。
    expect(sentinel.copyWith(status: TaskStatus.paused).shareUrl,
        'https://mikanani.me/Download/ep01.torrent');
  });
}
