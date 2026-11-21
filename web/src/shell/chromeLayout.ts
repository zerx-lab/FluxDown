// 外壳布局模型（纯函数）：
// 标题栏 / 活动栏 / 状态栏共六个区域（zone），条目可在允许的区域间拖动重排，也可手动隐藏（`fixed` 条目除外）。
// normalize / move / setHidden 的语义由共享用例 `crates/shell/tests/fixtures/chrome_layout_cases.json` 约束，
// GPUI 与 Web 两端都必须通过；改语义要同时改两端与用例。

/** 区域线上键，规范顺序（归一化时按此顺序消费存储值）。 */
export const CHROME_ZONES = ['titleLeading', 'titleTrailing', 'railTop', 'railBottom', 'statusLeading', 'statusTrailing'] as const
export type ChromeZone = (typeof CHROME_ZONES)[number]

/** 区域族：条目声明自己能放进哪些族的区域。 */
export type ChromeFamily = 'rail' | 'title' | 'status'

export interface ChromeItemSpec {
  id: string
  defaultZone: ChromeZone
  families: readonly ChromeFamily[]
  /** 固定条目：可移动，但不能被隐藏。 */
  fixed?: boolean
  /** 默认隐藏：存储布局里还没有摆放它（新用户、新增条目）时以隐藏状态加入，可在自定义布局里显示。 */
  defaultHidden?: boolean
}

/** 完整布局：六个区域一个不缺；`hidden` 为手动隐藏的条目 id（目录顺序，与摆放位置无关）。 */
export type ChromeLayout = Readonly<Record<ChromeZone, readonly string[]>> & { readonly hidden: readonly string[] }

/** 序列化后的存储值：六个区域必写，`hidden` 仅在非空时写入。 */
export type StoredChromeLayout = Record<ChromeZone, string[]> & { hidden?: string[] }

export function zoneFamily(zone: ChromeZone): ChromeFamily {
  if (zone.startsWith('title')) return 'title'
  if (zone.startsWith('rail')) return 'rail'
  return 'status'
}

/** 条目能否放进该区域（按族判定）。 */
export function itemAllowedIn(spec: ChromeItemSpec, zone: ChromeZone): boolean {
  return spec.families.includes(zoneFamily(zone))
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * 存储值 → 完整布局：
 * 1. 非普通对象 → 默认布局；
 * 2. 按规范区域顺序，数组区域内仅保留「字符串、目录内 id、尚未放置、允许放入该区域」的元素；未知区域键与非数组值忽略；
 * 3. 未被放置的目录条目按目录顺序追加到各自默认区域末尾；
 * 4. `hidden` 须为数组，仅保留「字符串、目录内、非 fixed」的 id，去重并按目录顺序输出；存储值里没摆放的 `defaultHidden` 条目也加入。
 */
export function normalizeLayout(stored: unknown, catalog: readonly ChromeItemSpec[]): ChromeLayout {
  const specs = new Map(catalog.map((spec) => [spec.id, spec]))
  const placed = new Set<string>()
  const zones: Record<ChromeZone, string[]> = {
    titleLeading: [],
    titleTrailing: [],
    railTop: [],
    railBottom: [],
    statusLeading: [],
    statusTrailing: [],
  }
  let hiddenStored: unknown[] = []
  if (isPlainObject(stored)) {
    for (const zone of CHROME_ZONES) {
      const items = stored[zone]
      if (!Array.isArray(items)) continue
      for (const id of items) {
        if (typeof id !== 'string' || placed.has(id)) continue
        const spec = specs.get(id)
        if (!spec || !itemAllowedIn(spec, zone)) continue
        placed.add(id)
        zones[zone].push(id)
      }
    }
    if (Array.isArray(stored.hidden)) hiddenStored = stored.hidden
  }
  // 未摆放条目插到默认区域第一个目录位置靠后的条目之前（没有则追加），新增内置入口不会落到插件入口之后。
  const storedPlaced = new Set(placed)
  const order = new Map(catalog.map((spec, index) => [spec.id, index]))
  catalog.forEach((spec, index) => {
    if (placed.has(spec.id)) return
    const zone = zones[spec.defaultZone]
    const at = zone.findIndex((id) => (order.get(id) ?? -1) > index)
    zone.splice(at === -1 ? zone.length : at, 0, spec.id)
  })
  const hiddenSet = new Set(hiddenStored)
  const hidden = catalog
    .filter((spec) => !spec.fixed && (hiddenSet.has(spec.id) || (spec.defaultHidden === true && !storedPlaced.has(spec.id))))
    .map((spec) => spec.id)
  return { ...zones, hidden }
}

export function defaultLayout(catalog: readonly ChromeItemSpec[]): ChromeLayout {
  return normalizeLayout(null, catalog)
}

/** 条目是否被手动隐藏。 */
export function isHidden(layout: ChromeLayout, id: string): boolean {
  return layout.hidden.includes(id)
}

function sameLayout(left: ChromeLayout, right: ChromeLayout): boolean {
  return CHROME_ZONES.every((zone) => {
    const a = left[zone]
    const b = right[zone]
    return a.length === b.length && a.every((id, index) => id === b[index])
  })
}

/**
 * 把 `id` 移到 `zone`（隐藏状态不受影响）：
 * - `id` 未知或不允许放入 `zone` → 不变；`target === id` → 不变；
 * - `target` 非空且当前在 `zone` 内：按移除前的下标 `ti` 取位，移除 `id` 后插到 `ti`
 *   （同区域向后拖落在 target 之后，向前 / 跨区域落在 target 之前）；
 * - 否则（`null` 或 target 不在该区域）移除后追加到 `zone` 末尾。
 */
export function moveItem(
  layout: ChromeLayout,
  catalog: readonly ChromeItemSpec[],
  id: string,
  zone: ChromeZone,
  target: string | null,
): { layout: ChromeLayout; changed: boolean } {
  const spec = catalog.find((item) => item.id === id)
  if (!spec || !itemAllowedIn(spec, zone) || target === id) return { layout, changed: false }
  const ti = target === null ? -1 : layout[zone].indexOf(target)
  const zones = {} as Record<ChromeZone, string[]>
  for (const name of CHROME_ZONES) zones[name] = layout[name].filter((item) => item !== id)
  if (ti >= 0) zones[zone].splice(ti, 0, id)
  else zones[zone].push(id)
  const next: ChromeLayout = { ...zones, hidden: layout.hidden }
  const changed = !sameLayout(layout, next)
  return { layout: changed ? next : layout, changed }
}

/**
 * 显示 / 隐藏 `id`：未知条目、`fixed` 条目、状态未变 → 不变；
 * 隐藏时按目录顺序插入 `hidden`，显示时移除。摆放位置不变。
 */
export function setHidden(
  layout: ChromeLayout,
  catalog: readonly ChromeItemSpec[],
  id: string,
  hidden: boolean,
): { layout: ChromeLayout; changed: boolean } {
  const spec = catalog.find((item) => item.id === id)
  if (!spec || spec.fixed || isHidden(layout, id) === hidden) return { layout, changed: false }
  const nextHidden = hidden
    ? catalog.filter((item) => item.id === id || layout.hidden.includes(item.id)).map((item) => item.id)
    : layout.hidden.filter((item) => item !== id)
  return { layout: { ...layout, hidden: nextHidden }, changed: true }
}

/** 序列化为存储值：始终写全六个区域，`hidden` 仅在非空时写入。 */
export function serializeLayout(layout: ChromeLayout): StoredChromeLayout {
  const stored: StoredChromeLayout = {
    titleLeading: [...layout.titleLeading],
    titleTrailing: [...layout.titleTrailing],
    railTop: [...layout.railTop],
    railBottom: [...layout.railBottom],
    statusLeading: [...layout.statusLeading],
    statusTrailing: [...layout.statusTrailing],
  }
  if (layout.hidden.length > 0) stored.hidden = [...layout.hidden]
  return stored
}
