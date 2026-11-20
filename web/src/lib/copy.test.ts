import { expect, test } from 'bun:test'
import { copyText } from './copy'

test('copyText prefers Clipboard API and normalizes legacy and DOM failures', async () => {
  const navDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
  const docDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'document')
  let legacyCalls = 0
  let modernValue = ''
  let mode: 'success' | 'reject' | 'absent' = 'success'
  let legacyResult = true
  let mountingFails = false
  const textarea = { value: '', style: {}, setAttribute() {}, select() {}, remove() {} }
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: {
      get clipboard() {
        if (mode === 'absent') return undefined
        return {
          async writeText(value: string) {
            if (mode === 'reject') throw new Error('denied')
            modernValue = value
          },
        }
      },
    },
  })
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: {
      createElement: () => textarea,
      body: { appendChild() { if (mountingFails) throw new Error('body unavailable') } },
      execCommand() { legacyCalls += 1; return legacyResult },
    },
  })
  try {
    expect(await copyText('modern')).toBe(true)
    expect(modernValue).toBe('modern')
    expect(legacyCalls).toBe(0)

    mode = 'reject'
    expect(await copyText('denied')).toBe(true)
    expect(textarea.value).toBe('denied')

    mode = 'absent'
    const fallback = copyText('plain HTTP')
    expect(legacyCalls).toBe(2) // Before the promise is awaited.
    expect(await fallback).toBe(true)
    expect(textarea.value).toBe('plain HTTP')

    legacyResult = false
    expect(await copyText('no selection')).toBe(false)
    mountingFails = true
    expect(await copyText('no body')).toBe(false)
    expect(legacyCalls).toBe(3)
  } finally {
    if (navDescriptor) Object.defineProperty(globalThis, 'navigator', navDescriptor)
    else Reflect.deleteProperty(globalThis, 'navigator')
    if (docDescriptor) Object.defineProperty(globalThis, 'document', docDescriptor)
    else Reflect.deleteProperty(globalThis, 'document')
  }
})
