// Tests for the desktop task-group UI's pure logic: group aggregation
// (via GroupEntity, already covered by list_entity.dart §3.2), sparkline
// sampling, member clustering + directory-row synthesis, path-chain
// compression, and orphan-groupId degradation.
//
// Source: lib/src/models/download_controller.dart (partitionTasksByGroup/
// buildGroupEntity/groupMemberDirPath/flattenGroupMembers), lib/src/models/
// task_group.dart (DownloadGroup/sampleSparkline/compressPathChain),
// lib/src/models/list_entity.dart (GroupEntity/GroupMemberEntity/
// GroupDirEntity/GroupMemberCounts). Pure functions, no DownloadController
// instantiation needed (it requires rinf FFI, see list_sections_test.dart).

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/download_controller.dart';
import 'package:flux_down/src/models/download_task.dart';
import 'package:flux_down/src/models/list_entity.dart';
import 'package:flux_down/src/models/task_group.dart';

DownloadTask _task({
  required String id,
  String fileName = 'file.zip',
  String saveDir = '/root/Group',
  TaskStatus status = TaskStatus.downloading,
  int downloaded = 0,
  int total = 1000,
  int speed = 0,
  String groupId = '',
  String queueId = '',
}) {
  return DownloadTask(
    id: id,
    url: 'https://example.com/$fileName',
    fileName: fileName,
    saveDir: saveDir,
    status: status,
    downloadedBytes: downloaded,
    totalBytes: total,
    speed: speed,
    groupId: groupId,
    queueId: queueId,
  );
}

DownloadGroup _group({
  String id = 'g1',
  String name = 'My Group',
  String saveDir = '/root/Group',
  String sourceUrl = 'https://example.com/album',
}) {
  return DownloadGroup(
    id: id,
    name: name,
    sourceUrl: sourceUrl,
    saveDir: saveDir,
    createdAt: DateTime(2026, 1, 1),
  );
}

void main() {
  group('DownloadGroup.displayName', () {
    test('uses name when non-empty', () {
      expect(_group(name: 'Album').displayName, 'Album');
    });

    test('falls back to save dir basename when name is empty', () {
      expect(
        _group(name: '', saveDir: '/root/Downloads/MyAlbum').displayName,
        'MyAlbum',
      );
      expect(
        _group(name: '', saveDir: 'C:\\Downloads\\MyAlbum').displayName,
        'MyAlbum',
      );
    });
  });

  group('sampleSparkline', () {
    test('passes through unchanged when <= maxBars', () {
      final items = List.generate(24, (i) => i);
      expect(sampleSparkline(items, maxBars: 24), items);
      expect(sampleSparkline([1, 2, 3], maxBars: 24), [1, 2, 3]);
    });

    test('samples exactly maxBars items when over the limit', () {
      final items = List.generate(100, (i) => i);
      final sampled = sampleSparkline(items, maxBars: 24);
      expect(sampled.length, 24);
      // step = 100/24; sampled[i] = items[(i*step).floor()]
      final step = 100 / 24;
      for (var i = 0; i < 24; i++) {
        expect(sampled[i], (i * step).floor());
      }
    });

    test('empty input yields empty output', () {
      expect(sampleSparkline(<int>[]), isEmpty);
    });
  });

  group('compressPathChain', () {
    test('empty path (group root) returns empty string', () {
      expect(compressPathChain(''), '');
    });

    test('<=3 segments renders the full chain', () {
      expect(compressPathChain('videos'), 'videos /');
      expect(compressPathChain('videos/season1'), 'videos / season1 /');
      expect(
        compressPathChain('a/b/c'),
        'a / b / c /',
      );
    });

    test('>3 segments compresses to first / … / last', () {
      expect(
        compressPathChain('a/b/c/d'),
        'a / … / d /',
      );
      expect(
        compressPathChain('videos/2024/summer/clips/raw'),
        'videos / … / raw /',
      );
    });

    test('backslash paths are normalized the same as forward slashes', () {
      expect(compressPathChain(r'a\b\c\d'), 'a / … / d /');
    });
  });

  group('groupMemberDirPath', () {
    final group = _group(saveDir: '/root/Group');

    test('member saveDir == group saveDir → root ("")', () {
      final t = _task(id: 't1', saveDir: '/root/Group');
      expect(groupMemberDirPath(t, group), '');
    });

    test('member saveDir under group saveDir → relative subdirectory', () {
      final t = _task(id: 't1', saveDir: '/root/Group/videos/season1');
      expect(groupMemberDirPath(t, group), 'videos/season1');
    });

    test('backslash-separated saveDir is normalized', () {
      final winGroup = _group(saveDir: r'C:\Downloads\Group');
      final t = _task(id: 't1', saveDir: r'C:\Downloads\Group\videos');
      expect(groupMemberDirPath(t, winGroup), 'videos');
    });

    test('unrelated saveDir (no prefix match) defensively falls back to root', () {
      final t = _task(id: 't1', saveDir: '/somewhere/else');
      expect(groupMemberDirPath(t, group), '');
    });
  });

  group('partitionTasksByGroup', () {
    test('groups tasks with a known groupId, leaves the rest ungrouped', () {
      final tasks = [
        _task(id: 't1', groupId: 'g1'),
        _task(id: 't2', groupId: 'g1'),
        _task(id: 't3', groupId: ''),
      ];
      final result = partitionTasksByGroup(tasks, {'g1'});
      expect(result.byGroup.keys, ['g1']);
      expect(result.byGroup['g1']!.map((t) => t.id), ['t1', 't2']);
      expect(result.ungrouped.map((e) => e.id), ['t3']);
    });

    test('orphan groupId (unknown to the group table) degrades to flat task', () {
      final tasks = [
        _task(id: 't1', groupId: 'deleted-group'),
        _task(id: 't2', groupId: 'g1'),
      ];
      final result = partitionTasksByGroup(tasks, {'g1'});
      expect(result.ungrouped.map((e) => e.id), ['t1']);
      expect(result.byGroup.keys, ['g1']);
    });

    test('empty task list yields empty partition', () {
      final result = partitionTasksByGroup(const [], {'g1'});
      expect(result.ungrouped, isEmpty);
      expect(result.byGroup, isEmpty);
    });
  });

  group('buildGroupEntity + GroupEntity aggregation', () {
    test('aggregates queueId from the first member and wraps all members', () {
      final members = [
        _task(id: 't1', queueId: 'work', total: 100, downloaded: 50),
        _task(id: 't2', queueId: 'work', total: 100, downloaded: 100, status: TaskStatus.completed),
      ];
      final entity = buildGroupEntity(_group(), members);
      expect(entity.groupId, 'g1');
      expect(entity.queueId, 'work');
      expect(entity.members.length, 2);
      expect(entity.totalBytes, 200);
      expect(entity.downloadedBytes, 150);
    });

    test('status: any active member → downloading', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.paused),
        _task(id: 't2', status: TaskStatus.downloading),
      ]);
      expect(entity.statusBucket, TaskStatus.downloading);
    });

    test('status: no active, any error → error', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.completed),
        _task(id: 't2', status: TaskStatus.error),
      ]);
      expect(entity.statusBucket, TaskStatus.error);
    });

    test('status: all completed → completed', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.completed),
        _task(id: 't2', status: TaskStatus.completed),
      ]);
      expect(entity.statusBucket, TaskStatus.completed);
    });

    test('status: otherwise (mix of paused/completed, no active/error) → paused', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.completed),
        _task(id: 't2', status: TaskStatus.paused),
      ]);
      expect(entity.statusBucket, TaskStatus.paused);
    });

    test('dominant category: most-frequent extension wins, ties broken by bytes', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', fileName: 'a.mp4', total: 10),
        _task(id: 't2', fileName: 'b.mp4', total: 10),
        _task(id: 't3', fileName: 'c.srt', total: 5),
      ]);
      expect(entity.categoryKey, FileCategory.video);
    });

    test('speed: sums only active members, ignores paused/done speed leftovers', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.downloading, speed: 100),
        _task(id: 't2', status: TaskStatus.paused, speed: 999),
        _task(id: 't3', status: TaskStatus.downloading, speed: 50),
      ]);
      expect(entity.speedBytesPerSec, 150);
    });
  });

  group('GroupMemberCounts.of', () {
    test('tallies members per status bucket', () {
      final entity = buildGroupEntity(_group(), [
        _task(id: 't1', status: TaskStatus.completed),
        _task(id: 't2', status: TaskStatus.downloading),
        _task(id: 't3', status: TaskStatus.preparing),
        _task(id: 't4', status: TaskStatus.pending),
        _task(id: 't5', status: TaskStatus.paused),
        _task(id: 't6', status: TaskStatus.error),
        _task(id: 't7', status: TaskStatus.error),
      ]);
      final counts = GroupMemberCounts.of(entity.members);
      expect(counts.total, 7);
      expect(counts.done, 1);
      expect(counts.downloading, 2); // downloading + preparing
      expect(counts.pending, 1);
      expect(counts.paused, 1);
      expect(counts.failed, 2);
    });
  });

  group('flattenGroupMembers (membersHtml clustering)', () {
    final group = _group(saveDir: '/root/Group');

    test(
      'root files attach directly (no dir header); non-root files get one '
      'dir header each, sorted by full path',
      () {
        final members = [
          _task(id: 'root', fileName: 'readme.txt', saveDir: '/root/Group', total: 10),
          _task(id: 'v2', fileName: 'ep2.mp4', saveDir: '/root/Group/videos', total: 20),
          _task(id: 'v1', fileName: 'ep1.mp4', saveDir: '/root/Group/videos', total: 30),
          _task(id: 'sub', fileName: 'ep1.srt', saveDir: '/root/Group/subs', total: 1),
        ];

        final flat = flattenGroupMembers(
          group: group,
          members: members,
          isDirCollapsed: (_) => false,
        );

        // Expected order (sorted by dir/fileName full path):
        // "readme.txt" < "subs/ep1.srt" < "videos/ep1.mp4" < "videos/ep2.mp4"
        expect(flat.length, 6); // 4 members + 2 dir headers (subs, videos)
        expect(flat[0], isA<GroupMemberEntity>());
        expect((flat[0] as GroupMemberEntity).task.id, 'root');
        expect((flat[0] as GroupMemberEntity).dirPath, '');

        expect(flat[1], isA<GroupDirEntity>());
        expect((flat[1] as GroupDirEntity).path, 'subs');
        expect((flat[1] as GroupDirEntity).fileCount, 1);
        expect(flat[2], isA<GroupMemberEntity>());
        expect((flat[2] as GroupMemberEntity).task.id, 'sub');

        expect(flat[3], isA<GroupDirEntity>());
        expect((flat[3] as GroupDirEntity).path, 'videos');
        expect((flat[3] as GroupDirEntity).fileCount, 2);
        expect((flat[3] as GroupDirEntity).totalDirBytes, 50);
        expect(flat[4], isA<GroupMemberEntity>());
        expect((flat[4] as GroupMemberEntity).task.id, 'v1');
        expect(flat[5], isA<GroupMemberEntity>());
        expect((flat[5] as GroupMemberEntity).task.id, 'v2');
      },
    );

    test('collapsed directory hides its member rows but keeps the dir header', () {
      final members = [
        _task(id: 'v1', fileName: 'ep1.mp4', saveDir: '/root/Group/videos'),
        _task(id: 'v2', fileName: 'ep2.mp4', saveDir: '/root/Group/videos'),
        _task(id: 'root', fileName: 'readme.txt', saveDir: '/root/Group'),
      ];

      final flat = flattenGroupMembers(
        group: group,
        members: members,
        isDirCollapsed: (path) => path == 'videos',
      );

      // Root member still shown; "videos" dir header present but its two
      // member rows are hidden.
      expect(flat.whereType<GroupMemberEntity>().length, 1);
      expect(flat.whereType<GroupDirEntity>().length, 1);
      expect((flat.whereType<GroupDirEntity>().first).fileCount, 2);
    });

    test('empty member list yields empty flatten result', () {
      final flat = flattenGroupMembers(
        group: group,
        members: const [],
        isDirCollapsed: (_) => false,
      );
      expect(flat, isEmpty);
    });
  });
}
