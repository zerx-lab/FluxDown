import { describe, expect, test } from 'bun:test'
import { fileHashDisplay, fileHashMatchesChecksum, hashAlgorithmLabel } from './fileHash'

const HEX = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
const HEX_UPPER = '0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF'
const OTHER = 'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210'
const MD5 = '0123456789abcdef0123456789abcdef'
const SHA512 = 'ab'.repeat(64)

describe('hashAlgorithmLabel', () => {
  test('已知算法映射到固定技术名，未知算法原样大写', () => {
    expect(hashAlgorithmLabel('md5')).toBe('MD5')
    expect(hashAlgorithmLabel('sha1')).toBe('SHA-1')
    expect(hashAlgorithmLabel('sha256')).toBe('SHA-256')
    expect(hashAlgorithmLabel('sha512')).toBe('SHA-512')
    expect(hashAlgorithmLabel('blake3')).toBe('BLAKE3')
  })
})

describe('fileHashMatchesChecksum', () => {
  test('共享用例', () => {
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, `SHA-256=${HEX_UPPER}`)).toBe(true)
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, `sha256=sha256:${HEX}`)).toBe(true)
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, `sha256=${OTHER}`)).toBe(false)
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, `md5=${MD5}`)).toBe(null)
    expect(fileHashMatchesChecksum('', `sha256=${HEX}`)).toBe(null)
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, '')).toBe(null)
  })

  test('校验和的算法前缀只在归一化后与算法相同时去掉', () => {
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, `sha256=md5:${HEX}`)).toBe(false)
    expect(fileHashMatchesChecksum(`sha1=${MD5}`, `SHA-1=sha-1:${MD5}`)).toBe(true)
    expect(fileHashMatchesChecksum(`sha256=${HEX}`, 'sha256=sha256:')).toBe(null)
  })
})

describe('fileHashDisplay', () => {
  test('长摘要原样返回，已知算法用规范名', () => {
    expect(fileHashDisplay(`sha512=${SHA512}`)).toEqual({ algo: 'sha512', label: 'SHA-512', hex: SHA512 })
    expect(SHA512).toHaveLength(128)
    expect(fileHashDisplay(`SHA-256=${HEX}`)?.label).toBe('SHA-256')
    expect(fileHashDisplay('blake3=abc')).toEqual({ algo: null, label: 'BLAKE3', hex: 'abc' })
    expect(fileHashDisplay('')).toBeNull()
    expect(fileHashDisplay('not-a-spec')).toBeNull()
  })
})
