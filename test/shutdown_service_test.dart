// Tests for ShutdownService (lib/src/services/shutdown_service.dart) — the
// "shutdown after all downloads finish" service.
//
// SAFETY: ShutdownService.instance is a real singleton whose countdown, when
// it reaches zero, calls a platform shutdown command (`shutdown /s ...` on
// Windows). Every test in this file MUST inject `debugShutdownExecutor`
// *before* anything can start a countdown, so the very first thing setUp
// does is stub it out. Never assign `null` to it (that would restore the
// real platform executor) — tearDown only calls `unbind()`, which does not
// touch the executor field.
//
// Countdown ticks come from `Timer.periodic(Duration(seconds: 1), ...)`
// inside the service. Any test that can trigger a countdown runs inside
// `fakeAsync` so the timer is virtual and `async.elapse(...)` drives it
// deterministically instead of waiting on real wall-clock time.

// ignore: depend_on_referenced_packages
import 'package:fake_async/fake_async.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/services/shutdown_service.dart';

/// Minimal stand-in for the real activeCount source (DownloadController),
/// which cannot be instantiated in unit tests (it wires up native signal
/// streams in its constructor). `fire()` mimics DownloadController notifying
/// listeners whenever its active task count changes.
class _FakeSource extends ChangeNotifier {
  void fire() => notifyListeners();
}

void main() {
  late ShutdownService svc;
  late _FakeSource fakeSource;
  late int activeCount;
  late int shutdownCalls;

  setUp(() {
    svc = ShutdownService.instance;
    activeCount = 0;
    shutdownCalls = 0;
    // Stub the executor FIRST — before any bind/arm call can create a timer.
    svc.debugShutdownExecutor = () async {
      shutdownCalls++;
    };
    fakeSource = _FakeSource();
    svc.bindSource(fakeSource, () => activeCount);
  });

  tearDown(() {
    svc.unbind();
  });

  group('arm — active-task gate', () {
    test('rejects arming when there are no active tasks', () {
      activeCount = 0;
      final ok = svc.arm();
      expect(ok, isFalse);
      expect(svc.isArmed, isFalse);
    });

    test('arms and stores delay when tasks are active, without counting down', () {
      activeCount = 3;
      final ok = svc.arm(minutes: 5);
      expect(ok, isTrue);
      expect(svc.isArmed, isTrue);
      expect(svc.delayMinutes, 5);
      expect(svc.isCountingDown, isFalse);
      expect(svc.remainingSeconds, -1);
    });
  });

  group('countdown state machine', () {
    test('all tasks finishing starts the countdown at delayMinutes*60', () {
      fakeAsync((async) {
        activeCount = 1;
        expect(svc.arm(minutes: 5), isTrue);

        activeCount = 0;
        fakeSource.fire();

        expect(svc.isCountingDown, isTrue);
        expect(svc.remainingSeconds, 5 * 60);
      });
    });

    test('countdown reaching zero issues shutdown exactly once via fake executor', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 1);
        activeCount = 0;
        fakeSource.fire();
        expect(svc.isCountingDown, isTrue);

        async.elapse(const Duration(minutes: 1));

        expect(shutdownCalls, 1);
        expect(svc.isArmed, isFalse);
        expect(svc.isCountingDown, isFalse);

        // Timer already cancelled internally — further elapsing must not
        // trigger a second shutdown.
        async.elapse(const Duration(minutes: 5));
        expect(shutdownCalls, 1);
      });
    });

    test(
      'a new active task during countdown cancels it but stays armed; '
      'tasks reaching zero again restarts the countdown',
      () {
        fakeAsync((async) {
          activeCount = 1;
          svc.arm(minutes: 5);
          activeCount = 0;
          fakeSource.fire();
          expect(svc.isCountingDown, isTrue);

          async.elapse(const Duration(seconds: 10));
          expect(svc.remainingSeconds, 5 * 60 - 10);

          activeCount = 1;
          fakeSource.fire();
          expect(svc.isCountingDown, isFalse);
          expect(svc.isArmed, isTrue);

          activeCount = 0;
          fakeSource.fire();
          expect(svc.isCountingDown, isTrue);
          expect(svc.remainingSeconds, 5 * 60);
        });
      },
    );
  });

  group('cancel', () {
    test('cancels a standing-by (not yet counting down) arm', () {
      activeCount = 1;
      svc.arm();
      svc.cancel();
      expect(svc.isArmed, isFalse);
    });

    test('cancels an in-progress countdown without ever issuing shutdown', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 1);
        activeCount = 0;
        fakeSource.fire();
        expect(svc.isCountingDown, isTrue);

        svc.cancel();
        expect(svc.isCountingDown, isFalse);
        expect(svc.isArmed, isFalse);

        async.elapse(const Duration(minutes: 2));
        expect(shutdownCalls, 0);
      });
    });
  });

  group('setDelayMinutes', () {
    test('re-times an in-progress countdown to the new delay', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 5);
        activeCount = 0;
        fakeSource.fire();

        async.elapse(const Duration(seconds: 30));
        svc.setDelayMinutes(2);

        expect(svc.remainingSeconds, 120);
      });
    });

    test('clamps minutes to [0, 1440] both via arm and via setDelayMinutes', () {
      activeCount = 1;
      svc.arm(minutes: 0);
      expect(svc.delayMinutes, 0);

      svc.setDelayMinutes(10);
      expect(svc.delayMinutes, 10);

      svc.setDelayMinutes(-5);
      expect(svc.delayMinutes, 0);

      svc.setDelayMinutes(100000);
      expect(svc.delayMinutes, 1440);
    });
  });

  group('delayMinutes = 0 — immediate shutdown', () {
    test('arm(minutes: 0) succeeds while tasks are active, without shutting down', () {
      activeCount = 3;
      final ok = svc.arm(minutes: 0);
      expect(ok, isTrue);
      expect(svc.isArmed, isTrue);
      expect(svc.delayMinutes, 0);
      expect(svc.isCountingDown, isFalse);
      expect(shutdownCalls, 0);
    });

    test(
      'all tasks finishing with delayMinutes 0 shuts down immediately without '
      'ever counting down, exactly once',
      () {
        fakeAsync((async) {
          activeCount = 1;
          expect(svc.arm(minutes: 0), isTrue);

          activeCount = 0;
          fakeSource.fire();
          async.flushMicrotasks();

          expect(shutdownCalls, 1);
          expect(svc.isCountingDown, isFalse);
          expect(svc.remainingSeconds, -1);
          expect(svc.isArmed, isFalse);

          // A further notification must not issue a second shutdown.
          fakeSource.fire();
          async.flushMicrotasks();
          expect(shutdownCalls, 1);
        });
      },
    );
  });

  group('setDelayMinutes(0) during countdown', () {
    test('switches an in-progress countdown to immediate shutdown', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 5);
        activeCount = 0;
        fakeSource.fire();
        expect(svc.isCountingDown, isTrue);

        svc.setDelayMinutes(0);
        async.flushMicrotasks();

        expect(shutdownCalls, 1);
        expect(svc.isCountingDown, isFalse);
        expect(svc.remainingSeconds, -1);
        expect(svc.isArmed, isFalse);
      });
    });
  });

  group('remainingText', () {
    test('formats remaining countdown seconds as mm:ss', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 2); // 120s
        activeCount = 0;
        fakeSource.fire();

        async.elapse(const Duration(seconds: 30)); // 120 - 30 = 90
        expect(svc.remainingSeconds, 90);
        expect(svc.remainingText, '01:30');
      });
    });

    test('is 00:00 when not counting down', () {
      expect(svc.isCountingDown, isFalse);
      expect(svc.remainingText, '00:00');
    });
  });

  group('unbind', () {
    test('resets armed/countdown/canArm even if the task source is still active', () {
      fakeAsync((async) {
        activeCount = 1;
        svc.arm(minutes: 5);
        activeCount = 0;
        fakeSource.fire();
        expect(svc.isCountingDown, isTrue);

        // Flip activeCount back to non-zero to prove canArm becomes false
        // because the source is detached — not because there are no tasks.
        activeCount = 3;
        svc.unbind();

        expect(svc.isArmed, isFalse);
        expect(svc.isCountingDown, isFalse);
        expect(svc.canArm, isFalse);

        // No timer should remain alive to fire a shutdown later.
        async.elapse(const Duration(minutes: 10));
        expect(shutdownCalls, 0);
      });
    });
  });
}
