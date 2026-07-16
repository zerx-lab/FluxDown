import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:launch_at_startup/launch_at_startup.dart';

import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/settings_provider.dart';

/// Repro for zerx-lab/FluxDown#98: on Linux (.deb install) the
/// "Associate .torrent files" toggle cannot be turned off.
///
/// The .deb registers `com.fluxdown.app.desktop` system-wide (root-owned),
/// so after `disassociate()` strips the per-user mimeapps.list override,
/// `xdg-mime query default application/x-bittorrent` STILL resolves FluxDown.
/// The actor immediately re-queries and sends `FileAssociationStatus
/// { is_associated: true }`, which clobbers the user's OFF choice and snaps
/// the switch back on within ~50ms.
///
/// Expected behavior: an explicit user opt-out wins over a live query that
/// can never go false, and it survives a restart via a persisted config key.
void main() {
  final binding = TestWidgetsFlutterBinding.ensureInitialized();

  // SettingsProvider's constructor syncs auto-startup state over the
  // `launch_at_startup` method channel; mock it so the async sync completes.
  launchAtStartup.setup(
    appName: 'FluxDownTest',
    appPath: Platform.resolvedExecutable,
  );
  binding.defaultBinaryMessenger.setMockMethodCallHandler(
    const MethodChannel('launch_at_startup'),
    (call) async => call.method == 'launchAtStartupIsEnabled' ? false : null,
  );

  ConfigEntry entry(String key, String value) =>
      ConfigEntry(key: key, value: value);

  // Calls SettingsProvider.setFileAssociation tolerating the ArgumentError
  // from `sendSignalToRust` (the Rust dylib is not loaded under
  // `flutter test`). All in-memory state mutations must complete before the
  // fire-and-forget signal sends, so the observable state is unaffected.
  void userToggles(SettingsProvider settings, bool enable) {
    try {
      settings.setFileAssociation(enable);
    } on ArgumentError {
      // rinf native library unavailable in the test VM.
    }
  }

  test(
    'user toggle-off sticks even when the live query still reports associated',
    () {
      final settings = SettingsProvider(enableFileAssoc: false);
      addTearDown(settings.dispose);

      // Association is on (e.g. user enabled it earlier; Rust confirmed).
      userToggles(settings, true);
      settings.handleFileAssociationStatus(true);
      expect(settings.torrentAssociated, isTrue);

      // User turns the toggle OFF.
      userToggles(settings, false);
      expect(settings.torrentAssociated, isFalse);

      // Linux reality: disassociate() cannot remove the root-owned
      // system-wide registration, so the immediate re-query reports true.
      settings.handleFileAssociationStatus(true);
      expect(
        settings.torrentAssociated,
        isFalse,
        reason: 'a user-requested OFF must not be clobbered by a live query '
            'that can never go false on a .deb install',
      );
    },
  );

  test('persisted opt-out survives restart (config reload + startup query)',
      () {
    // Fresh provider simulating an app restart after the user opted out.
    final settings = SettingsProvider(enableFileAssoc: false);
    addTearDown(settings.dispose);

    settings.applyLoadedConfig([entry('torrent_assoc_user_disabled', 'true')]);
    // The startup association check still sees the system registration.
    settings.handleFileAssociationStatus(true);
    expect(
      settings.torrentAssociated,
      isFalse,
      reason: 'the opt-out persisted before restart must keep the toggle OFF',
    );
  });

  test('without an opt-out the live status remains authoritative', () {
    final settings = SettingsProvider(enableFileAssoc: false);
    addTearDown(settings.dispose);

    // Fresh install: an installer-made association is reported and shown.
    settings.handleFileAssociationStatus(true);
    expect(settings.torrentAssociated, isTrue);

    // User opts in, but the OS later reports the association was lost
    // (possible on Windows/macOS) -> reflect reality.
    userToggles(settings, true);
    settings.handleFileAssociationStatus(false);
    expect(settings.torrentAssociated, isFalse);

    // And a later true report turns it back on (no stale opt-out in the way).
    settings.handleFileAssociationStatus(true);
    expect(settings.torrentAssociated, isTrue);
  });
}
