// 跨设备任务协同数据模型单测 —— 防守进度回流的核心不变式：
//   1. RemoteTaskStatus 线格映射与往返、活跃/终态分类；
//   2. RemoteTask.fromJson 全字段解析 + 缺省容错；
//   3. RemoteTask.copyWith 增量更新绝不丢失标识/URL/目录/归属（SSE 高频进度路径）；
//   4. CloudDevice 在线态/当前设备标志解析（服务端下发，缺省 false）；
//   5. ProgressReport 批量上报载荷序列化。
// 纯数据逻辑，不依赖 rinf FFI，可直接实例化断言（对齐 device_identity_test 策略）。
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/services/cloud/cloud_models.dart';

void main() {
  group('RemoteTaskStatus.fromWire', () {
    test('maps every wire value', () {
      expect(RemoteTaskStatus.fromWire('pending'), RemoteTaskStatus.pending);
      expect(RemoteTaskStatus.fromWire('accepted'), RemoteTaskStatus.accepted);
      expect(
        RemoteTaskStatus.fromWire('downloading'),
        RemoteTaskStatus.downloading,
      );
      expect(RemoteTaskStatus.fromWire('paused'), RemoteTaskStatus.paused);
      expect(RemoteTaskStatus.fromWire('completed'), RemoteTaskStatus.completed);
      expect(RemoteTaskStatus.fromWire('failed'), RemoteTaskStatus.failed);
      expect(RemoteTaskStatus.fromWire('canceled'), RemoteTaskStatus.canceled);
    });

    test('unknown / empty falls back to pending (fail-safe)', () {
      expect(RemoteTaskStatus.fromWire('bogus'), RemoteTaskStatus.pending);
      expect(RemoteTaskStatus.fromWire(''), RemoteTaskStatus.pending);
    });

    test('wire roundtrips for every value', () {
      for (final s in RemoteTaskStatus.values) {
        expect(RemoteTaskStatus.fromWire(s.wire), s);
      }
    });

    test('isActive / isTerminal classification', () {
      expect(RemoteTaskStatus.downloading.isActive, isTrue);
      expect(RemoteTaskStatus.pending.isActive, isTrue);
      expect(RemoteTaskStatus.accepted.isActive, isTrue);
      expect(RemoteTaskStatus.completed.isActive, isFalse);
      expect(RemoteTaskStatus.completed.isTerminal, isTrue);
      expect(RemoteTaskStatus.failed.isTerminal, isTrue);
      expect(RemoteTaskStatus.canceled.isTerminal, isTrue);
      expect(RemoteTaskStatus.downloading.isTerminal, isFalse);
      expect(RemoteTaskStatus.paused.isTerminal, isFalse);
    });
  });

  group('RemoteTask.fromJson', () {
    test('parses a full RemoteTaskDto (camelCase)', () {
      final r = RemoteTask.fromJson({
        'id': 't1',
        'fromDevice': 'devA',
        'toDevice': 'devB',
        'url': 'https://x/f.iso',
        'saveDir': '/volume1/dl',
        'fileName': 'f.iso',
        'status': 'downloading',
        'totalBytes': 1000,
        'downloadedBytes': 420,
        'speed': 8200000,
        'progress': 0.42,
        'error': null,
        'createdAt': '2026-07-20T00:00:00Z',
        'updatedAt': '2026-07-20T00:01:00Z',
      });
      expect(r.id, 't1');
      expect(r.fromDevice, 'devA');
      expect(r.toDevice, 'devB');
      expect(r.saveDir, '/volume1/dl');
      expect(r.status, RemoteTaskStatus.downloading);
      expect(r.totalBytes, 1000);
      expect(r.downloadedBytes, 420);
      expect(r.speed, 8200000);
      expect(r.progress, closeTo(0.42, 1e-9));
    });

    test('tolerates missing optional fields (partial dispatch payload)', () {
      final r = RemoteTask.fromJson({'id': 't2', 'toDevice': 'devB'});
      expect(r.id, 't2');
      expect(r.toDevice, 'devB');
      expect(r.status, RemoteTaskStatus.pending);
      expect(r.downloadedBytes, 0);
      expect(r.speed, 0);
      expect(r.progress, 0);
      expect(r.totalBytes, isNull);
      expect(r.fileName, '');
      expect(r.saveDir, isNull);
    });
  });

  group('RemoteTask.copyWith — SSE 增量绝不丢字段', () {
    RemoteTask base() => RemoteTask.fromJson({
      'id': 't1',
      'fromDevice': 'devA',
      'toDevice': 'devB',
      'url': 'https://x/f.iso',
      'saveDir': '/dl',
      'fileName': 'f.iso',
      'status': 'downloading',
      'totalBytes': 1000,
      'downloadedBytes': 100,
      'speed': 500,
      'progress': 0.1,
      'createdAt': 'c',
      'updatedAt': 'u1',
    });

    test('progress delta preserves identity / url / saveDir / owner / name', () {
      final next = base().copyWith(
        downloadedBytes: 600,
        speed: 900,
        progress: 0.6,
      );
      expect(next.id, 't1');
      expect(next.url, 'https://x/f.iso');
      expect(next.saveDir, '/dl');
      expect(next.toDevice, 'devB');
      expect(next.fromDevice, 'devA');
      expect(next.fileName, 'f.iso');
      expect(next.createdAt, 'c');
      // 只更新传入字段：
      expect(next.downloadedBytes, 600);
      expect(next.speed, 900);
      expect(next.progress, closeTo(0.6, 1e-9));
      // 未传入的保持不变：
      expect(next.status, RemoteTaskStatus.downloading);
      expect(next.totalBytes, 1000);
    });

    test('status transition keeps accumulated progress fields', () {
      final done = base().copyWith(status: RemoteTaskStatus.completed);
      expect(done.status, RemoteTaskStatus.completed);
      expect(done.downloadedBytes, 100);
      expect(done.totalBytes, 1000);
      expect(done.url, 'https://x/f.iso');
    });
  });

  group('CloudDevice online / current flags', () {
    test('default to false when server omits them', () {
      final d = CloudDevice.fromJson({'id': '1', 'deviceId': 'd1'});
      expect(d.isOnline, isFalse);
      expect(d.isCurrent, isFalse);
    });

    test('parse server-provided presence flags', () {
      final d = CloudDevice.fromJson({
        'id': '1',
        'deviceId': 'd1',
        'isOnline': true,
        'isCurrent': true,
      });
      expect(d.isOnline, isTrue);
      expect(d.isCurrent, isTrue);
    });
  });

  group('ProgressReport.toJson', () {
    test('serializes a batch item payload', () {
      final j = const ProgressReport(
        taskId: 't1',
        downloadedBytes: 5,
        speed: 9,
        progress: 0.5,
      ).toJson();
      expect(j['taskId'], 't1');
      expect(j['downloadedBytes'], 5);
      expect(j['speed'], 9);
      expect(j['progress'], 0.5);
    });
  });
}
