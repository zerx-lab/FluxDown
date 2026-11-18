// 设置页共享表单套件（WebSettingsA 提供；各分类分区目录 `sections/<name>/` 统一从这里导入）。
export { SettingsCustomRow, SettingsPage, SettingsRow, SettingsSection } from './layout'
export type { SettingsTabDef } from './layout'
export {
  DaemonEnumRow,
  DaemonNumberRow,
  DaemonSwitchRow,
  DaemonTextRow,
  DropdownField,
  NumberField,
  PrefDropdownRow,
  PrefSwitchRow,
  TextField,
} from './fields'
export { WIDTH_DROPDOWN, WIDTH_INPUT, WIDTH_INPUT_WIDE, WIDTH_NUMBER, camel, formatNumber, optionalText } from './util'
export {
  SETTINGS_ERROR_KEYS,
  clearSettingsError,
  flushSettings,
  normalizeDaemonValue,
  setDaemon,
  setDaemonBool,
  setDaemonNumber,
  setPref,
  useDaemonBool,
  useDaemonNumber,
  useDaemonValue,
  usePrefBoolean,
  usePrefNumber,
  usePrefRaw,
  usePrefString,
  useSettingsError,
  useSettingsReadOnly,
} from './writeStore'
export type { SettingsError, SettingsErrorKind } from './writeStore'
