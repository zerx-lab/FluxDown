// SplitEventData.receivedAt supplies a real-time timestamp for the detail
// panel's Log tab even though the coordinator signal itself carries no
// wall-clock time — verifies the constructor default and explicit override.
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/download_task.dart';

void main() {
  test('receivedAt defaults to now() when not supplied', () {
    final before = DateTime.now();
    final evt = SplitEventData(
      parentIndex: 0,
      parentNewEnd: 999,
      childIndex: 1,
      childStart: 1000,
      childEnd: 1999,
      isProactive: true,
      totalSegments: 2,
    );
    final after = DateTime.now();
    expect(
      evt.receivedAt.isAfter(before.subtract(const Duration(seconds: 1))),
      isTrue,
    );
    expect(
      evt.receivedAt.isBefore(after.add(const Duration(seconds: 1))),
      isTrue,
    );
  });

  test('receivedAt honors an explicit override', () {
    final fixed = DateTime(2026, 1, 1, 12, 30);
    final evt = SplitEventData(
      parentIndex: 0,
      parentNewEnd: 999,
      childIndex: 1,
      childStart: 1000,
      childEnd: 1999,
      isProactive: false,
      totalSegments: 2,
      receivedAt: fixed,
    );
    expect(evt.receivedAt, fixed);
  });
}
