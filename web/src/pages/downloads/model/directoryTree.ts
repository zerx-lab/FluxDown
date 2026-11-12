import { compareNatural } from './viewPrefs'
export interface DirectoryItem {
  path: string
  root: string
  scope: string
  scope_label: string
  count: number
}

export type DirectoryKind = 'default' | 'external' | 'unknown' | 'scope' | 'folder'
export type DirectoryRow =
  | { kind: 'header'; directoryKind: DirectoryKind; key: string; label: string; path: string; depth: number; count: number }
  | { kind: 'item'; index: number; depth: number }

interface Node {
  name: string
  nameFold: string
  path: string
  parent: number | null
  direct: { index: number; count: number }[]
  children: number[]
  childByName: Map<string, number>
  count: number
}
type Category = 'default' | 'external' | 'unknown'
interface Tree {
  scope: string
  scopeLabel: string
  category: Category
  rootPath: string
  rootFold: string
  nodes: Node[]
}

const windowsPath = (path: string): boolean => /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\') || path.startsWith('//')

function normalizePath(path: string, windows: boolean): string {
  if (!path) return ''
  const slashes = windows ? path.replaceAll('\\', '/') : path
  const unc = windows && slashes.startsWith('//')
  const drive = windows && /^[A-Za-z]:/.test(slashes)
  const absolute = slashes.startsWith('/') || drive
  const prefix = unc ? '//' : drive ? slashes.slice(0, 2) : absolute ? '/' : ''
  const rest = unc ? slashes.replace(/^\/+/, '') : drive ? slashes.slice(2).replace(/^\/+/, '') : slashes.replace(/^\/+/, '')
  const parts: string[] = []
  for (const part of rest.split('/')) {
    if (!part || part === '.') continue
    if (part === '..') {
      if (parts.length && parts.at(-1) !== '..') parts.pop()
      else if (!absolute) parts.push(part)
    } else parts.push(part)
  }
  const joined = parts.join('/')
  if (unc) return `//${joined}`
  if (drive) return joined ? `${prefix}/${joined}` : `${prefix}/`
  if (absolute) return joined ? `/${joined}` : '/'
  return joined
}

function pathComponents(path: string, windows: boolean): string[] {
  if (!path) return []
  if (windows && path.startsWith('//')) {
    const parts = path.replace(/^\/+/, '').split('/').filter(Boolean)
    return parts.length < 2 ? parts : [`//${parts[0]}/${parts[1]}`, ...parts.slice(2)]
  }
  const drive = windows && /^[A-Za-z]:/.test(path)
  const rest = drive ? path.slice(3) : path.replace(/^\/+/, '')
  return [...(drive ? [path.slice(0, 2)] : []), ...rest.split('/').filter(Boolean)]
}

function isDescendant(path: string, root: string, windows: boolean): boolean {
  if (path === root) return true
  const absolute = path.startsWith('/') || (windows && /^[A-Za-z]:/.test(path))
  if (!absolute) return false
  const pathParts = pathComponents(path, windows)
  const rootParts = pathComponents(root, windows)
  return pathParts.length > rootParts.length && rootParts.every((part, index) => pathParts[index] === part)
}

function compareNames(leftFold: string, left: string, rightFold: string, right: string): number {
  return compareNatural(leftFold, rightFold) || (left < right ? -1 : left > right ? 1 : 0)
}


const compareLexical = (left: string, right: string): number => left < right ? -1 : left > right ? 1 : 0

function treeFor(trees: Tree[], scope: string, scopeLabel: string, category: Category, rootPath: string): number {
  const found = trees.findIndex((tree) => tree.scope === scope && tree.category === category && tree.rootPath === rootPath)
  if (found >= 0) return found
  trees.push({ scope, scopeLabel, category, rootPath, rootFold: rootPath.toLowerCase(), nodes: [{ name: '', nameFold: '', path: rootPath, parent: null, direct: [], children: [], childByName: new Map(), count: 0 }] })
  return trees.length - 1
}

function addPath(tree: Tree, path: string, root: string, index: number, count: number, windows: boolean): void {
  const pathParts = pathComponents(path, windows)
  const rootParts = pathComponents(root, windows)
  const relative = tree.category === 'external' ? pathParts : tree.category === 'default' ? pathParts.slice(rootParts.length) : []
  let nodeIndex = 0
  for (const name of relative) {
    const node = tree.nodes[nodeIndex]
    let child = node.childByName.get(name)
    if (child === undefined) {
      const childPath = node.path
        ? node.path === '/' || node.path.endsWith('/') ? `${node.path}${name}` : `${node.path}/${name}`
        : path.startsWith('/') && !name.startsWith('//') ? `/${name}` : name
      child = tree.nodes.length
      tree.nodes.push({ name, nameFold: name.toLowerCase(), path: childPath, parent: nodeIndex, direct: [], children: [], childByName: new Map(), count: 0 })
      node.children.push(child)
      node.childByName.set(name, child)
    }
    nodeIndex = child
  }
  tree.nodes[nodeIndex].direct.push({ index, count })
  let current: number | null = nodeIndex
  while (current !== null) {
    tree.nodes[current].count += count
    current = tree.nodes[current].parent
  }
}

function emitTree(tree: Tree, isCollapsed: (key: string) => boolean, depthOffset: number, rows: DirectoryRow[]): void {
  const kind: DirectoryKind = tree.category
  const key = kind === 'default' ? `directory:${tree.scope}:${tree.rootPath}` : `category:${tree.scope}:${kind}`
  rows.push({ kind: 'header', directoryKind: kind, key, label: '', path: tree.rootPath, depth: depthOffset, count: tree.nodes[0].count })
  if (isCollapsed(key)) return
  type Event = { type: 'visit'; node: number; depth: number } | { type: 'folder'; child: number; end: number; depth: number; label: string }
  const stack: Event[] = [{ type: 'visit', node: 0, depth: depthOffset + 1 }]
  while (stack.length) {
    const event = stack.pop()
    if (!event) continue
    if (event.type === 'folder') {
      const path = tree.nodes[event.end].path
      const folderKey = `directory:${tree.scope}:${path}`
      rows.push({ kind: 'header', directoryKind: 'folder', key: folderKey, label: event.label, path, depth: event.depth, count: tree.nodes[event.child].count })
      if (!isCollapsed(folderKey)) stack.push({ type: 'visit', node: event.end, depth: event.depth + 1 })
      continue
    }
    const node = tree.nodes[event.node]
    for (const item of node.direct) rows.push({ kind: 'item', index: item.index, depth: event.depth })
    const children = [...node.children].sort((left, right) => compareNames(tree.nodes[left].nameFold, tree.nodes[left].name, tree.nodes[right].nameFold, tree.nodes[right].name))
    const folders = children.map((child) => {
      let end = child
      const parts = [tree.nodes[end].name]
      while (true) {
        const currentKey = `directory:${tree.scope}:${tree.nodes[end].path}`
        if (isCollapsed(currentKey) || tree.nodes[end].direct.length > 0 || tree.nodes[end].children.length !== 1) break
        const next = tree.nodes[end].children[0]
        const nextKey = `directory:${tree.scope}:${tree.nodes[next].path}`
        if (isCollapsed(nextKey)) break
        end = next
        parts.push(tree.nodes[end].name)
      }
      return { child, end, label: parts.join(' / ') }
    })
    for (const folder of folders.reverse()) stack.push({ type: 'folder', ...folder, depth: event.depth })
  }
}

/** Flatten filtered task-directory items into stable, collapsible rows. */
export function flattenDirectories(items: readonly DirectoryItem[], collapsed: (key: string) => boolean): DirectoryRow[] {
  const trees: Tree[] = []
  items.forEach((item, index) => {
    const windows = windowsPath(item.path) || windowsPath(item.root)
    const path = normalizePath(item.path, windows)
    const root = normalizePath(item.root, windows)
    const category: Category = !path ? 'unknown' : !root || !isDescendant(path, root, windows) ? 'external' : 'default'
    const rootPath = category === 'default' ? root : ''
    const treeIndex = treeFor(trees, item.scope, item.scope_label, category, rootPath)
    addPath(trees[treeIndex], path, rootPath, index, item.count, windows)
  })
  const categoryOrder: Record<Category, number> = { default: 0, external: 1, unknown: 2 }
  trees.sort((left, right) => compareLexical(left.scope, right.scope)
    || categoryOrder[left.category] - categoryOrder[right.category]
    || compareNames(left.rootFold, left.rootPath, right.rootFold, right.rootPath))
  const scopeCounts = new Map<string, { label: string; count: number }>()
  for (const tree of trees) {
    if (!tree.scope) continue
    const current = scopeCounts.get(tree.scope)
    if (current) current.count += tree.nodes[0].count
    else scopeCounts.set(tree.scope, { label: tree.scopeLabel, count: tree.nodes[0].count })
  }
  const rows: DirectoryRow[] = []
  let lastScope = ''
  for (const tree of trees) {
    let depthOffset = 0
    if (tree.scope) {
      if (lastScope !== tree.scope) {
        const scoped = scopeCounts.get(tree.scope)
        if (scoped) rows.push({ kind: 'header', directoryKind: 'scope', key: `scope:${tree.scope}`, label: scoped.label, path: '', depth: 0, count: scoped.count })
        lastScope = tree.scope
      }
      if (collapsed(`scope:${tree.scope}`)) continue
      depthOffset = 1
    }
    emitTree(tree, collapsed, depthOffset, rows)
  }
  return rows
}
