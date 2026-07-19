// Tests for the view system's pure bucketing (7 dims) + sorting (6 keys)
// tables, and the site-key/site-label extraction helpers they depend on.
//
// Source: lib/src/models/download_controller.dart (bucketEntities*/
// compareEntities*) + lib/src/models/download_task.dart (extractSiteKey/
// extractSiteLabel). Pure functions, no DownloadController instantiation
// needed (it requires rinf FFI, see scout-dart.md §13).

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/download_controller.dart';
import 'package:flux_down/src/models/download_queue.dart';
import 'package:flux_down/src/models/download_task.dart';
import 'package:flux_down/src/models/list_entity.dart';
import 'package:flux_down/src/models/view_prefs.dart';

DownloadTask _task({
  required String id,
  String fileName = 'file.zip',
  TaskStatus status = TaskStatus.downloading,
  int downloaded = 0,
  int total = 1000,
  int speed = 0,
  DateTime? createdAt,
  int queuePosition = -1,
  String queueId = '',
  String url = 'https://example.com/file.zip',
}) {
  return DownloadTask(
    id: id,
    url: url,
    fileName: fileName,
    saveDir: '/tmp',
    status: status,
    downloadedBytes: downloaded,
    totalBytes: total,
    speed: speed,
    createdAt: createdAt ?? DateTime.now(),
    queuePosition: queuePosition,
    queueId: queueId,
  );
}

void main() {
  group('site key/label extraction', () {
    test('registrable domain aggregates subdomains and strips www', () {
      expect(extractSiteKey('https://pan.baidu.com/s/abc'), 'baidu.com');
      expect(extractSiteKey('https://www.baidu.com/s/abc'), 'baidu.com');
      expect(extractSiteKey('https://baidu.com/s/abc'), 'baidu.com');
    });

    test('two-level public suffix (e.g. com.cn) keeps 3 labels as registrable domain', () {
      expect(extractSiteKey('https://download.example.com.cn/f'), 'example.com.cn');
      expect(extractSiteLabel('https://download.example.com.cn/f'), 'download.example.com.cn');
    });

    test('label keeps only the nearest one subdomain level, deeper chains collapse', () {
      expect(extractSiteLabel('https://a.b.pan.baidu.com/f'), 'pan.baidu.com');
      expect(extractSiteLabel('https://pan.baidu.com/f'), 'pan.baidu.com');
      expect(extractSiteLabel('https://github.com/f'), 'github.com');
    });

    test('magnet/torrent-file URLs collapse to the fixed "bt" bucket', () {
      expect(extractSiteKey('magnet:?xt=urn:btih:abcdef'), 'bt');
      expect(extractSiteKey('torrent-file:///tmp/a.torrent'), 'bt');
      expect(extractSiteLabel('magnet:?xt=urn:btih:abcdef'), isNotEmpty);
    });

    test('malformed/hostless URLs never produce an empty key', () {
      expect(extractSiteKey('not a url at all'), isNotEmpty);
    });
  });

  group('bucketEntities* (7 dims)', () {
    final now = DateTime.now();

    test('none: single flat bucket with no title', () {
      final entities = [
        TaskEntity(_task(id: '1')),
        TaskEntity(_task(id: '2')),
      ];
      final sections = bucketEntitiesNone(entities);
      expect(sections, hasLength(1));
      expect(sections.single.title, isNull);
      expect(sections.single.entities, hasLength(2));
    });

    test('smart: active tasks bucket first, historical tasks bucketed by date', () {
      final entities = [
        TaskEntity(_task(id: 'done-today', status: TaskStatus.completed, createdAt: now)),
        TaskEntity(_task(id: 'dl', status: TaskStatus.downloading, createdAt: now)),
        TaskEntity(_task(id: 'pend', status: TaskStatus.pending, createdAt: now)),
        TaskEntity(
          _task(
            id: 'old-paused',
            status: TaskStatus.paused,
            createdAt: now.subtract(const Duration(days: 40)),
          ),
        ),
      ];
      final sections = bucketEntitiesSmart(entities);
      expect(sections.first.key, 'smart:live');
      expect(sections.first.entities.map((e) => e.id), containsAll(['dl', 'pend']));
      // 历史桶不含活跃任务。
      final historicalIds = sections.skip(1).expand((s) => s.entities).map((e) => e.id);
      expect(historicalIds, containsAll(['done-today', 'old-paused']));
      expect(historicalIds, isNot(contains('dl')));
    });

    test('date: purely time-bucketed, no active-first split', () {
      final entities = [
        TaskEntity(_task(id: 'dl', status: TaskStatus.downloading, createdAt: now)),
        TaskEntity(_task(id: 'done', status: TaskStatus.completed, createdAt: now)),
      ];
      final sections = bucketEntitiesByDate(entities);
      // 两者都在「今天」档，同一个桶（无 smart:live 分离）。
      expect(sections, hasLength(1));
      expect(sections.single.key, 'date:0');
      expect(sections.single.entities, hasLength(2));
    });

    test('status: fixed order [downloading,pending,paused,error,completed], empty buckets dropped', () {
      final entities = [
        TaskEntity(_task(id: 'c', status: TaskStatus.completed)),
        TaskEntity(_task(id: 'e', status: TaskStatus.error)),
        TaskEntity(_task(id: 'd', status: TaskStatus.downloading)),
        TaskEntity(_task(id: 'p', status: TaskStatus.preparing)), // 并入下载中桶
      ];
      final sections = bucketEntitiesByStatus(entities);
      expect(sections.map((s) => s.key), ['status:downloading', 'status:error', 'status:completed']);
      expect(sections[0].entities.map((e) => e.id), containsAll(['d', 'p']));
    });

    test('type: fixed TYPE_ORDER, only non-empty buckets present', () {
      final entities = [
        TaskEntity(_task(id: 'v', fileName: 'movie.mp4')),
        TaskEntity(_task(id: 'a', fileName: 'song.mp3')),
        TaskEntity(_task(id: 'o', fileName: 'noext')),
      ];
      final sections = bucketEntitiesByType(entities);
      expect(sections.map((s) => s.key), ['type:video', 'type:audio', 'type:other']);
    });

    test('queue: default queue ("") first, then named queues in given order', () {
      final queues = [
        const DownloadQueue(
          queueId: 'work',
          name: 'Work',
          speedLimitKbps: 0,
          maxConcurrent: 0,
          defaultSaveDir: '',
          position: 1,
        ),
        const DownloadQueue(
          queueId: 'later',
          name: 'Later',
          speedLimitKbps: 0,
          maxConcurrent: 0,
          defaultSaveDir: '',
          position: 0,
        ),
      ];
      final entities = [
        TaskEntity(_task(id: 'w', queueId: 'work')),
        TaskEntity(_task(id: 'default', queueId: '')),
        TaskEntity(_task(id: 'l', queueId: 'later')),
      ];
      final sections = bucketEntitiesByQueue(entities, queues);
      expect(sections.map((s) => s.key), ['queue:', 'queue:work', 'queue:later']);
    });

    test('site: buckets sorted by member count descending', () {
      final entities = [
        TaskEntity(_task(id: '1', url: 'https://a.com/1')),
        TaskEntity(_task(id: '2', url: 'https://b.com/1')),
        TaskEntity(_task(id: '3', url: 'https://b.com/2')),
        TaskEntity(_task(id: '4', url: 'https://b.com/3')),
      ];
      final sections = bucketEntitiesBySite(entities);
      expect(sections.first.entities, hasLength(3)); // b.com 3 个排最前
      expect(sections.last.entities, hasLength(1)); // a.com 1 个排最后
    });
  });

  group('compareEntities (6 keys x direction)', () {
    final base = DateTime(2026, 1, 1);

    test('smart: status tier then createdAt ascending, direction ignored', () {
      final active = TaskEntity(_task(id: 'a', status: TaskStatus.downloading, createdAt: base));
      final pending = TaskEntity(_task(id: 'b', status: TaskStatus.pending, createdAt: base));
      expect(compareEntities(ViewSortKey.smart, SortDir.asc, active, pending), lessThan(0));
      expect(compareEntities(ViewSortKey.smart, SortDir.desc, active, pending), lessThan(0));

      final older = TaskEntity(
        _task(id: 'c', status: TaskStatus.completed, createdAt: base),
      );
      final newer = TaskEntity(
        _task(id: 'd', status: TaskStatus.completed, createdAt: base.add(const Duration(days: 1))),
      );
      expect(compareEntities(ViewSortKey.smart, SortDir.desc, older, newer), lessThan(0));
    });

    test('smart: pending entities tie-break by queue position', () {
      final p1 = TaskEntity(_task(id: 'p1', status: TaskStatus.pending, queuePosition: 2));
      final p2 = TaskEntity(_task(id: 'p2', status: TaskStatus.pending, queuePosition: 1));
      expect(compareEntities(ViewSortKey.smart, SortDir.asc, p1, p2), greaterThan(0));
    });

    test('created: asc/desc honored', () {
      final older = TaskEntity(_task(id: 'a', createdAt: base));
      final newer = TaskEntity(_task(id: 'b', createdAt: base.add(const Duration(hours: 1))));
      expect(compareEntities(ViewSortKey.created, SortDir.asc, older, newer), lessThan(0));
      expect(compareEntities(ViewSortKey.created, SortDir.desc, older, newer), greaterThan(0));
    });

    test('name: lexicographic, direction honored', () {
      final a = TaskEntity(_task(id: '1', fileName: 'alpha.zip'));
      final b = TaskEntity(_task(id: '2', fileName: 'beta.zip'));
      expect(compareEntities(ViewSortKey.name, SortDir.asc, a, b), lessThan(0));
      expect(compareEntities(ViewSortKey.name, SortDir.desc, a, b), greaterThan(0));
    });

    test('size: total bytes, direction honored', () {
      final small = TaskEntity(_task(id: '1', total: 100));
      final big = TaskEntity(_task(id: '2', total: 900));
      expect(compareEntities(ViewSortKey.size, SortDir.desc, small, big), greaterThan(0));
      expect(compareEntities(ViewSortKey.size, SortDir.asc, small, big), lessThan(0));
    });

    test('progress: downloaded/total ratio, direction honored', () {
      final low = TaskEntity(_task(id: '1', downloaded: 100, total: 1000));
      final high = TaskEntity(_task(id: '2', downloaded: 900, total: 1000));
      expect(compareEntities(ViewSortKey.progress, SortDir.desc, low, high), greaterThan(0));
    });

    test('speed: bytes/sec, direction honored', () {
      final slow = TaskEntity(_task(id: '1', speed: 10, status: TaskStatus.downloading));
      final fast = TaskEntity(_task(id: '2', speed: 999, status: TaskStatus.downloading));
      expect(compareEntities(ViewSortKey.speed, SortDir.desc, slow, fast), greaterThan(0));
      expect(compareEntities(ViewSortKey.speed, SortDir.asc, slow, fast), lessThan(0));
    });
  });

  group('orderSections (inter-bucket ordering, sort drives global narrative)', () {
    final now = DateTime.now();

    test('smart key keeps canonical bucket order', () {
      final entities = <ListEntity>[
        TaskEntity(_task(id: 'c', status: TaskStatus.completed, downloaded: 1000, createdAt: now)),
        TaskEntity(_task(id: 'e', status: TaskStatus.error, createdAt: now)),
        TaskEntity(_task(id: 'd', status: TaskStatus.downloading, createdAt: now)),
      ];
      final ordered = orderSections(
        bucketEntitiesByStatus(entities), ViewSortKey.smart, SortDir.desc);
      expect(ordered.map((s) => s.key),
          ['status:downloading', 'status:error', 'status:completed']);
    });

    test('explicit key reorders buckets by top-row extremum (progress desc puts completed first)', () {
      final entities = <ListEntity>[
        TaskEntity(_task(id: 'e', status: TaskStatus.error, downloaded: 0)),
        TaskEntity(_task(id: 'c', status: TaskStatus.completed, downloaded: 1000)),
        TaskEntity(_task(id: 'd', status: TaskStatus.downloading, downloaded: 500)),
      ];
      final sections = bucketEntitiesByStatus(entities);
      for (final s in sections) {
        s.entities.sort(
            (a, b) => compareEntities(ViewSortKey.progress, SortDir.desc, a, b));
      }
      final ordered = orderSections(sections, ViewSortKey.progress, SortDir.desc);
      expect(ordered.map((s) => s.key),
          ['status:completed', 'status:downloading', 'status:error']);
    });

    test('smart:live bucket stays pinned under explicit sort', () {
      final entities = <ListEntity>[
        TaskEntity(_task(id: 'a', status: TaskStatus.completed, fileName: 'aaa.zip', createdAt: now)),
        TaskEntity(_task(id: 'z', status: TaskStatus.downloading, fileName: 'zzz.zip', createdAt: now)),
      ];
      final sections = bucketEntitiesSmart(entities);
      for (final s in sections) {
        s.entities.sort(
            (a, b) => compareEntities(ViewSortKey.name, SortDir.asc, a, b));
      }
      final ordered = orderSections(sections, ViewSortKey.name, SortDir.asc);
      expect(ordered.first.key, 'smart:live');
    });

    test('ties preserve bucketizer canonical order', () {
      final entities = <ListEntity>[
        TaskEntity(_task(id: 'c', status: TaskStatus.completed, fileName: 'same.zip')),
        TaskEntity(_task(id: 'e', status: TaskStatus.error, fileName: 'same.zip')),
      ];
      final ordered = orderSections(
          bucketEntitiesByStatus(entities), ViewSortKey.name, SortDir.asc);
      expect(ordered.map((s) => s.key), ['status:error', 'status:completed']);
    });

    test('date buckets read chronologically under created asc', () {
      final entities = <ListEntity>[
        TaskEntity(_task(id: 't', status: TaskStatus.completed, createdAt: now)),
        TaskEntity(_task(
            id: 'o',
            status: TaskStatus.completed,
            createdAt: now.subtract(const Duration(days: 400)))),
      ];
      final sections = bucketEntitiesByDate(entities);
      for (final s in sections) {
        s.entities.sort(
            (a, b) => compareEntities(ViewSortKey.created, SortDir.asc, a, b));
      }
      final ordered = orderSections(sections, ViewSortKey.created, SortDir.asc);
      expect(ordered.map((s) => s.key), ['date:4', 'date:0']);
    });
  });
}
