import { useCallback, useSyncExternalStore } from 'react'

export type FileSizeUnits = 'legacy' | 'iec'

export const FILE_SIZE_UNITS_PREF = 'appearance.file_size_units'
const CHANGE_EVENT = 'fluxdown:file-size-units-change'
const LEGACY_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const
const IEC_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const

let current: FileSizeUnits = 'legacy'
let initialized = false

function initialize(): void {
  if (initialized) return
  initialized = true
  try {
    current = localStorage.getItem(FILE_SIZE_UNITS_PREF) === 'iec' ? 'iec' : 'legacy'
  } catch {
    // Keep the in-memory default when storage is unavailable.
  }
}

function read(): FileSizeUnits {
  initialize()
  return current
}

function onStorage(event: StorageEvent): void {
  if (event.storageArea !== localStorage || (event.key !== FILE_SIZE_UNITS_PREF && event.key !== null)) return
  const next = event.newValue === 'iec' ? 'iec' : 'legacy'
  if (next === current) return
  current = next
  initialized = true
  window.dispatchEvent(new Event(CHANGE_EVENT))
}

function subscribe(listener: () => void): () => void {
  initialize()
  window.addEventListener(CHANGE_EVENT, listener)
  window.addEventListener('storage', onStorage)
  return () => {
    window.removeEventListener(CHANGE_EVENT, listener)
    window.removeEventListener('storage', onStorage)
  }
}

export function getFileSizeUnits(): FileSizeUnits {
  return read()
}

export function setFileSizeUnits(value: FileSizeUnits): void {
  current = value
  initialized = true
  try {
    localStorage.setItem(FILE_SIZE_UNITS_PREF, value)
  } catch {
    // Keep the in-memory display preference effective when storage is unavailable.
  }
  window.dispatchEvent(new Event(CHANGE_EVENT))
}

export function useFileSizeUnits(): FileSizeUnits {
  return useSyncExternalStore(subscribe, read, () => 'legacy')
}

export function byteUnitFor(index: number, preference: FileSizeUnits): string {
  const units = preference === 'iec' ? IEC_UNITS : LEGACY_UNITS
  return units[index] ?? units[units.length - 1]
}

/** Selects the display suffix only; all byte scaling remains binary (1024). */
export function byteUnit(index: number, units = getFileSizeUnits()): string {
  return byteUnitFor(index, units)
}

/** Bind a byte formatter to the preference so React can invalidate formatted output. */
export function useByteFormatter(format: (bytes: number, units: FileSizeUnits) => string): (bytes: number) => string {
  const units = useFileSizeUnits()
  return useCallback((bytes: number) => format(bytes, units), [format, units])
}
