// Tests for the config-sync catalog (lib/src/services/cloud/sync_catalog.dart)
// —— covers the pure encode/decode helpers directly, catalog-shape invariants
// required by the sync contract (local://sync-contract.md: key naming,
// uniqueness, JSON-encodability), and apply()'s tolerant-parsing behavior.
//
// Note on scope: entries backed by [SettingsProvider] setters call
// `SaveConfig(...).sendSignalToRust()` (Rinf FFI), which needs the compiled
// native `hub` library that isn't loadable under plain `flutter test`. So the
// "successful apply" path is exercised only via [ThemeProvider]/
// [LocaleNotifier]-backed entries (pure KvStore, no FFI); the SettingsProvider
// entries are only exercised through the *skip* path, where the setter is
// never reached. ConfigSyncService itself (network + singleton) is not
// unit-tested here — see task notes.

import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart' show Color, ThemeMode;
import 'package:flutter_test/flutter_test.dart';

import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/models/settings_provider.dart';
import 'package:flux_down/src/services/cloud/sync_catalog.dart';
import 'package:flux_down/src/theme/theme_provider.dart';

/// Contract key format: `^[a-z0-9_]+(\.[a-z0-9_]+)*$`, length 1..128.
final _kKeyPattern = RegExp(r'^[a-z0-9_]+(\.[a-z0-9_]+)*$');

/// [SettingsProvider]'s constructor fires an un-awaited
/// `_syncAutoStartupState()` that calls `launch_at_startup`'s noop backend,
/// which throws `UnsupportedError` outside a real platform integration.
/// That failure is irrelevant to sync-catalog behavior; run construction in
/// a guarded zone so the async leak can't bleed into a *later* test's zone
/// (a known `flutter_test` flakiness pattern with fire-and-forget futures).
SettingsProvider _newSettingsProvider() {
  late SettingsProvider provider;
  runZonedGuarded(() {
    provider = SettingsProvider(enableFileAssoc: false);
  }, (error, stack) {});
  return provider;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('pure encode/decode helpers', () {
    test('decodeBool accepts only real booleans', () {
      expect(decodeBool(true), isTrue);
      expect(decodeBool(false), isFalse);
      expect(decodeBool('true'), isNull);
      expect(decodeBool(1), isNull);
      expect(decodeBool(null), isNull);
    });

    test('decodeInt accepts int and num, rejects everything else', () {
      expect(decodeInt(5), 5);
      expect(decodeInt(5.0), 5);
      expect(decodeInt(5.9), 5); // truncates, matches int.toInt() semantics
      expect(decodeInt('5'), isNull);
      expect(decodeInt(true), isNull);
      expect(decodeInt(null), isNull);
    });

    test('encodeThemeMode/decodeThemeMode round-trip every enum value', () {
      for (final mode in ThemeMode.values) {
        expect(decodeThemeMode(encodeThemeMode(mode)), mode);
      }
      expect(decodeThemeMode('nonsense'), isNull);
      expect(decodeThemeMode(''), isNull);
    });

    test('encodeThemeSelection prefers custom id over builtin', () {
      expect(
        encodeThemeSelection(customId: 'abc', builtin: BuiltinThemeId.defaultDark),
        'custom:abc',
      );
      expect(
        encodeThemeSelection(customId: null, builtin: BuiltinThemeId.defaultDark),
        'builtin:defaultDark',
      );
    });

    test('decodeThemeSelection parses both forms and round-trips', () {
      for (final id in BuiltinThemeId.values) {
        final sel = decodeThemeSelection('builtin:${id.name}');
        expect(sel, isNotNull);
        expect(sel!.builtinId, id);
        expect(sel.customId, isNull);
      }
      final custom = decodeThemeSelection('custom:xyz-123');
      expect(custom, isNotNull);
      expect(custom!.customId, 'xyz-123');
      expect(custom.builtinId, isNull);
    });

    test('decodeThemeSelection rejects malformed or unknown input', () {
      expect(decodeThemeSelection('bogus'), isNull);
      expect(decodeThemeSelection('custom:'), isNull); // empty id
      expect(decodeThemeSelection('builtin:notARealTheme'), isNull);
      expect(decodeThemeSelection(''), isNull);
    });
  });

  group('catalog shape', () {
    late SettingsProvider settings;
    late ThemeProvider theme;
    late LocaleNotifier locale;
    late List<SyncEntry> catalog;

    setUp(() {
      settings = _newSettingsProvider();
      theme = ThemeProvider();
      locale = LocaleNotifier();
      catalog = buildSyncCatalog(settings: settings, theme: theme, locale: locale);
    });

    tearDown(() {
      settings.dispose();
    });

    test('has exactly the 47 keys listed in the sync contract v1', () {
      // 5 appearance + 6 general + 7 ui + 13 download + 11 bt（5 + 6 做种）+ 5 ed2k.
      expect(catalog.length, 47);
    });

    test('every key matches the contract key pattern and length limit', () {
      for (final entry in catalog) {
        expect(
          _kKeyPattern.hasMatch(entry.key),
          isTrue,
          reason: '"${entry.key}" does not match ${_kKeyPattern.pattern}',
        );
        expect(entry.key.length, inInclusiveRange(1, 128));
      }
    });

    test('no duplicate keys', () {
      final keys = catalog.map((e) => e.key).toList();
      expect(keys.toSet().length, keys.length);
    });

    test('every read() value round-trips through jsonEncode/jsonDecode', () {
      for (final entry in catalog) {
        final value = entry.read();
        late String encoded;
        expect(
          () => encoded = jsonEncode(value),
          returnsNormally,
          reason: '${entry.key} produced a non-JSON-encodable value: $value',
        );
        expect(jsonDecode(encoded), value);
      }
    });

    test('every key falls under one of the six documented categories', () {
      const prefixes = {'appearance', 'general', 'ui', 'download', 'bt', 'ed2k'};
      for (final entry in catalog) {
        expect(prefixes, contains(entry.key.split('.').first));
      }
    });
  });

  group('apply() tolerant parsing — skip path (never reaches a setter)', () {
    late SettingsProvider settings;
    late ThemeProvider theme;
    late LocaleNotifier locale;
    late List<SyncEntry> catalog;

    setUp(() {
      settings = _newSettingsProvider();
      theme = ThemeProvider();
      locale = LocaleNotifier();
      catalog = buildSyncCatalog(settings: settings, theme: theme, locale: locale);
    });

    tearDown(() {
      settings.dispose();
    });

    SyncEntry entryFor(String key) => catalog.firstWhere((e) => e.key == key);

    test('bool entry silently skips a wrong-typed value instead of throwing', () {
      final entry = entryFor('general.auto_check_update');
      final before = settings.autoCheckUpdate;
      expect(() => entry.apply('not-a-bool'), returnsNormally);
      expect(settings.autoCheckUpdate, before);
      expect(() => entry.apply(null), returnsNormally);
      expect(settings.autoCheckUpdate, before);
    });

    test('int entry silently skips a non-numeric value', () {
      final entry = entryFor('download.max_concurrent_tasks');
      final before = settings.maxConcurrentTasks;
      expect(() => entry.apply('7'), returnsNormally);
      expect(settings.maxConcurrentTasks, before);
      expect(() => entry.apply(null), returnsNormally);
      expect(settings.maxConcurrentTasks, before);
    });

    test('string entry silently skips a non-string value', () {
      final entry = entryFor('bt.custom_trackers');
      final before = settings.btCustomTrackers;
      expect(() => entry.apply(42), returnsNormally);
      expect(settings.btCustomTrackers, before);
    });

    test('appearance.dark_theme skips an unresolvable custom theme id', () {
      final entry = entryFor('appearance.dark_theme');
      final before = theme.selectedDarkTheme;
      expect(() => entry.apply('custom:does-not-exist'), returnsNormally);
      expect(theme.selectedDarkTheme, before);
      expect(theme.isCustomDarkActive, isFalse);
    });

    test('appearance.dark_theme skips a malformed value', () {
      final entry = entryFor('appearance.dark_theme');
      final before = theme.selectedDarkTheme;
      expect(() => entry.apply('bogus'), returnsNormally);
      expect(() => entry.apply(123), returnsNormally);
      expect(theme.selectedDarkTheme, before);
    });

    test('appearance.theme_mode skips an unknown mode string', () {
      final entry = entryFor('appearance.theme_mode');
      final before = theme.themeMode;
      expect(() => entry.apply('nonsense'), returnsNormally);
      expect(theme.themeMode, before);
    });

    test('general.locale skips an undiscovered locale code', () {
      final entry = entryFor('general.locale');
      final before = locale.preference;
      expect(() => entry.apply('xx-not-real'), returnsNormally);
      expect(locale.preference, before);
    });
  });

  group('apply() tolerant parsing — successful path (theme/locale, no FFI)', () {
    late ThemeProvider theme;
    late LocaleNotifier locale;
    late SettingsProvider settings;
    late List<SyncEntry> catalog;

    setUp(() {
      settings = _newSettingsProvider();
      theme = ThemeProvider();
      locale = LocaleNotifier();
      catalog = buildSyncCatalog(settings: settings, theme: theme, locale: locale);
    });

    tearDown(() {
      settings.dispose();
    });

    SyncEntry entryFor(String key) => catalog.firstWhere((e) => e.key == key);

    test('appearance.theme_mode applies a valid mode', () {
      entryFor('appearance.theme_mode').apply('dark');
      expect(theme.themeMode, ThemeMode.dark);
    });

    test('appearance.color_scheme applies a known scheme', () {
      entryFor('appearance.color_scheme').apply('green');
      expect(theme.colorScheme, AppColorScheme.green);
    });

    test('appearance.custom_color applies an ARGB int', () {
      entryFor('appearance.custom_color').apply(0xFF112233);
      expect(theme.customColor, const Color(0xFF112233));
      expect(theme.colorScheme, AppColorScheme.custom);
    });

    test('appearance.dark_theme applies a known builtin id', () {
      entryFor('appearance.dark_theme').apply('builtin:nord');
      expect(theme.selectedDarkTheme, BuiltinThemeId.nord);
      expect(theme.isCustomDarkActive, isFalse);
    });
  });
}
