import { describe, expect, test } from 'bun:test'
import { builtinDefaults, categoriesFromPreference } from '../model/categories'
import { btCategoryClassifier, btFileMatches, btSelectionCanConfirm, btSelectionName, btSelectionSaveDir } from './btSelection'

describe('btSelectionSaveDir', () => {
  test('未改、空白、与预填相同都不下发', () => {
    expect(btSelectionSaveDir('/data', '/data')).toBeUndefined()
    expect(btSelectionSaveDir('/data', '  /data  ')).toBeUndefined()
    expect(btSelectionSaveDir('/data', '   ')).toBeUndefined()
    expect(btSelectionSaveDir('', '  ')).toBeUndefined()
    expect(btSelectionSaveDir(' /data ', '/data')).toBeUndefined()
  })

  test('修剪后非空且不同于预填才下发', () => {
    expect(btSelectionSaveDir('/data', ' /new ')).toBe('/new')
    expect(btSelectionSaveDir('', '/new')).toBe('/new')
  })
})

describe('btSelectionName', () => {
  test('未改、空白都不下发（沿用种子内名称）', () => {
    expect(btSelectionName('Pack', 'Pack')).toBeUndefined()
    expect(btSelectionName('Pack', '  Pack ')).toBeUndefined()
    expect(btSelectionName('Pack', '   ')).toBeUndefined()
  })

  test('改过才下发修剪后的名字', () => {
    expect(btSelectionName('Pack', ' 我的剧集 ')).toBe('我的剧集')
    expect(btSelectionName('', 'Pack')).toBe('Pack')
  })
})

describe('btSelectionCanConfirm', () => {
  test('预填非空且输入被清空时禁用；空预填允许空输入', () => {
    expect(btSelectionCanConfirm('/data', '  ', 1)).toBe(false)
    expect(btSelectionCanConfirm('', '', 1)).toBe(true)
    expect(btSelectionCanConfirm('/data', '/other', 1)).toBe(true)
  })

  test('没有勾选文件时禁用', () => {
    expect(btSelectionCanConfirm('/data', '/data', 0)).toBe(false)
  })
})

test('BT filters intersect full path, minimum size (binary MB) and any selected category', () => {
  const min = 10 * 1024 * 1024
  const files = [
    { index: 8, path: 'Season/Trailer.MKV', size: min },
    { index: 23, path: 'Extras/Trailer.MP3', size: min + 1 },
    { index: 35, path: 'Extras/Trailer.mp4', size: min - 1 },
    { index: 37, path: 'Trailer/episode.mkv', size: 15_000_000 },
    { index: 42, path: 'Season/Trailer.srt', size: min },
    { index: 50, path: 'Season/other.mkv', size: min },
  ]
  const categories = new Set(['builtin_video', 'builtin_audio'])
  const classify = btCategoryClassifier(builtinDefaults())
  expect(files.filter(({ path, size }) =>
    btFileMatches(path, size, classify(path), 'trailer', min, categories),
  ).map(({ index }) => index)).toEqual([8, 23, 37])
  expect(btFileMatches('Season/Trailer.srt', min, 'other', 'srt', 0, new Set())).toBe(true)
})

test('dots in directories and leading dots do not determine the file category', () => {
  const classify = btCategoryClassifier(builtinDefaults())
  expect(classify('Season.mkv/README')).toBe('builtin_other')
  expect(classify('Extras/.mkv')).toBe('builtin_other')
})

test('BT category filtering honors saved custom categories and edited built-in extensions', () => {
  const video = { ...builtinDefaults()[1]!, extensions: ['avi'], position: 3 }
  const subtitles = { ...video, id: 'subtitles', builtinType: null, name: 'Subtitles', position: 2, extensions: ['.SRT', 'avi'] }
  const regex = { ...subtitles, id: 'regex', position: 1, matchMode: 'regex' }
  const classify = btCategoryClassifier(categoriesFromPreference([video, subtitles, regex]))
  const paths = ['Season/Track.srt', 'Season/Track.AVI', 'Season/Track.mkv']
  const selected = new Set(['subtitles'])

  expect(paths.filter((path) => btFileMatches(path, 1, classify(path), '', 0, selected))).toEqual(paths.slice(0, 2))
  expect(classify(paths[2]!)).toBe('builtin_other')
})
