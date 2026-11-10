// 可移植插件视图协议类型（native/protocol/src/plugin_view/mod.rs，wire 为 camelCase JSON）。

import type { JsonValue } from './common';

/** 宿主内置跳转目标（`ctx.navigate` / `ViewEffect.navigate`）。 */
export const NAV_BUILTIN_TARGETS = ['downloads', 'rss', 'webhooks'] as const;

/** `pluginViewClosed` 的 reason：插件调用了 `ctx.close()`。 */
export const VIEW_CLOSE_REASON_REQUESTED = 'requested';

/** 帧附带的宿主副作用；navigate 的 `target` 为路由 id（内置 id 或 `plugin:<identity>:<view>`）。未知 kind 忽略。 */
export type ViewEffect = { kind: 'navigate'; target: string } | { kind: string };

/** 归一化后的视图节点；`id` 由 daemon 分配（根为 `root`，子节点 = 父 id + `/` + key 或下标）。 */
export interface ViewNode {
  id: string;
  /** 节点 kind（见 `plugin_view_schema.json`）。 */
  k: string;
  p: Record<string, JsonValue>;
  c: ViewNode[];
}

/** 增量更新 op（`op` 为标记字段）。 */
export type ViewOp =
  /** 整个子树替换；`id` 为被替换节点的 id，根节点即 `root`。 */
  | { op: 'replace'; id: string; node: ViewNode }
  /** prop 增删改。 */
  | { op: 'props'; id: string; set: Record<string, JsonValue>; unset: string[] }
  /** 子节点序列变化：`order` 为变化后完整的子 id 序列，`insert` 只含新出现的子树。 */
  | { op: 'children'; id: string; order: string[]; insert: ViewNode[] }
  /** 键控数组（`table.rows` / `list.items`）按 key 增量更新：先 remove，再 upsert，最后按 order 重排。 */
  | { op: 'rows'; id: string; upsert: JsonValue[]; remove: string[]; order?: string[] }
  /** 渲染失败：把 `message` 写到根节点 `p.__error`；下一帧不再带它时由 `props.unset` 清除。 */
  | { op: 'error'; message: string };

/** `pluginViewFrame` 事件载荷（`DaemonEvent::PluginViewFrame`）。 */
export interface PluginViewFrameDto {
  session: string;
  seq: number;
  ops: ViewOp[];
  /** 宿主副作用（先于 ops 执行，不受 seq 判定影响）；缺省为空。 */
  effects?: ViewEffect[];
}

/** `daemon.plugin.view.open` 参数。 */
export interface PluginViewOpenParams {
  identity: string;
  view: string;
  props?: JsonValue;
  taskId?: string;
  /** 界面语言（BCP 47）；缺省时插件 `ctx.locale` 为空串。 */
  locale?: string;
}

/** `daemon.plugin.view.open` 返回。 */
export interface PluginViewOpenedDto {
  session: string;
  seq: number;
  tree: ViewNode;
}

/** 视图事件；字段对应 schema `events.*.payload`。 */
export interface ViewEventDto {
  type: string;
  value?: JsonValue;
  key?: string;
  action?: string;
}

/** `daemon.plugin.view.event` 参数。 */
export interface PluginViewEventParams {
  session: string;
  /** 触发事件的节点 id。 */
  node: string;
  event: ViewEventDto;
}

/** `daemon.plugin.view.close` / `daemon.plugin.view.snapshot` 参数。 */
export interface PluginViewSessionParams {
  session: string;
}

/** `daemon.plugin.view.snapshot` 返回：会话当前序号与整棵树。 */
export interface PluginViewSnapshotDto {
  seq: number;
  tree: ViewNode;
}

/** `daemon.plugin.view.keepalive` 参数；90 s 无续期的会话会被关闭。 */
export interface PluginViewKeepaliveParams {
  sessions: string[];
}

/** `daemon.plugin.command` 参数。 */
export interface PluginCommandParams {
  identity: string;
  /** manifest `contributes.commands[].id`。 */
  command: string;
  taskIds?: string[];
}

/** `daemon.plugin.invoke` 参数。 */
export interface PluginInvokeParams {
  identity: string;
  /** `flux.rpc.handle` 注册的处理器名。 */
  name: string;
  args?: JsonValue;
}

/** `daemon.plugin.pageInvoke` 参数：页面脚本调用以 `{ page: true }` 注册的处理器。 */
export interface PluginPageInvokeParams {
  identity: string;
  name: string;
  args?: JsonValue;
  /** 发起调用的页面地址。 */
  pageUrl: string;
}

/** `daemon.plugin.pageInvoke` 结果：处理器返回值与本次调用期间插件开始的任务。 */
export interface PluginPageInvokeResultDto {
  value: JsonValue;
  createdTaskIds: string[];
  resumedTaskIds: string[];
}

/** 根节点上承载渲染错误的 prop 名（与 Rust `VIEW_ERROR_PROP` 一致）。 */
export const VIEW_ERROR_PROP = '__error';

/** 键控数组所在的 prop（与 `plugin_view_schema.json` 的 `keyedArray` 一致）。 */
const KEYED_PROP: Readonly<Record<string, string>> = { table: 'rows', list: 'items' };

export class ViewApplyError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ViewApplyError';
  }
}

function rowKey(row: JsonValue): string | undefined {
  if (typeof row !== 'object' || row === null || Array.isArray(row)) return undefined;
  const key = (row as Record<string, JsonValue>).key;
  return typeof key === 'string' ? key : undefined;
}

/** 沿 id 路径更新目标节点（id 段不含 `/`，逐层匹配）；不可变，仅复制路径上的节点。 */
function updateAt(node: ViewNode, id: string, update: (target: ViewNode) => ViewNode): ViewNode {
  if (node.id === id) return update(node);
  const rest = id.startsWith(`${node.id}/`) ? id.slice(node.id.length + 1) : null;
  if (rest === null) throw new ViewApplyError(`no node with id ${JSON.stringify(id)}`);
  const slash = rest.indexOf('/');
  const childId = `${node.id}/${slash < 0 ? rest : rest.slice(0, slash)}`;
  const index = node.c.findIndex((child) => child.id === childId);
  const child = node.c[index];
  if (child === undefined) throw new ViewApplyError(`no node with id ${JSON.stringify(id)}`);
  const c = node.c.slice();
  c[index] = updateAt(child, id, update);
  return { ...node, c };
}

function applyChildren(node: ViewNode, order: string[], insert: ViewNode[]): ViewNode {
  const pool = new Map<string, ViewNode>(node.c.map((child) => [child.id, child]));
  for (const inserted of insert) {
    if (pool.has(inserted.id)) throw new ViewApplyError(`children of ${node.id}: inserted id ${inserted.id} already exists`);
    pool.set(inserted.id, inserted);
  }
  const next: ViewNode[] = [];
  for (const childId of order) {
    const child = pool.get(childId);
    if (child === undefined) throw new ViewApplyError(`children of ${node.id}: order names unknown or repeated child ${childId}`);
    pool.delete(childId);
    next.push(child);
  }
  const leftover = insert.find((inserted) => pool.has(inserted.id));
  if (leftover) throw new ViewApplyError(`children of ${node.id}: inserted node ${leftover.id} is missing from order`);
  return { ...node, c: next };
}

function applyRows(node: ViewNode, upsert: JsonValue[], remove: string[], order: string[] | undefined): ViewNode {
  const name = KEYED_PROP[node.k];
  if (name === undefined) throw new ViewApplyError(`node ${node.id} of kind ${node.k} has no keyed array`);
  const current = node.p[name];
  if (!Array.isArray(current)) throw new ViewApplyError(`node ${node.id}: prop ${name} is not an array`);
  let rows = current.slice();
  if (remove.length > 0) {
    const doomed = new Set(remove);
    const before = rows.length;
    rows = rows.filter((row) => {
      const key = rowKey(row);
      return key === undefined || !doomed.has(key);
    });
    if (before - rows.length !== doomed.size) throw new ViewApplyError(`node ${node.id}: remove names a row key that does not exist`);
  }
  const index = new Map<string, number>();
  rows.forEach((row, i) => {
    const key = rowKey(row);
    if (key !== undefined) index.set(key, i);
  });
  for (const row of upsert) {
    const key = rowKey(row);
    if (key === undefined) throw new ViewApplyError(`node ${node.id}: upsert row has no string key`);
    const position = index.get(key);
    if (position === undefined) {
      index.set(key, rows.length);
      rows.push(row);
    } else {
      rows[position] = row;
    }
  }
  if (order) {
    if (order.length !== rows.length) {
      throw new ViewApplyError(`node ${node.id}: order has ${order.length} keys but there are ${rows.length} rows`);
    }
    const byKey = new Map<string, JsonValue>();
    for (const row of rows) {
      const key = rowKey(row);
      if (key !== undefined) byKey.set(key, row);
    }
    rows = order.map((key) => {
      const row = byKey.get(key);
      if (row === undefined) throw new ViewApplyError(`node ${node.id}: order names unknown or repeated row key ${key}`);
      byKey.delete(key);
      return row;
    });
  }
  return { ...node, p: { ...node.p, [name]: rows } };
}

function applyOp(tree: ViewNode, op: ViewOp): ViewNode {
  switch (op.op) {
    case 'error':
      return { ...tree, p: { ...tree.p, [VIEW_ERROR_PROP]: op.message } };
    case 'replace':
      return updateAt(tree, op.id, () => op.node);
    case 'props':
      return updateAt(tree, op.id, (node) => {
        const p = { ...node.p };
        for (const name of op.unset) delete p[name];
        for (const [name, value] of Object.entries(op.set)) p[name] = value;
        return { ...node, p };
      });
    case 'children':
      return updateAt(tree, op.id, (node) => applyChildren(node, op.order, op.insert));
    case 'rows':
      return updateAt(tree, op.id, (node) => applyRows(node, op.upsert, op.remove, op.order));
  }
}

/** 按序应用增量 op，返回新树（不修改入参）；失败抛 `ViewApplyError`，调用方应改为取 snapshot。 */
export function applyViewOps(tree: ViewNode, ops: readonly ViewOp[]): ViewNode {
  let next = tree;
  for (const op of ops) next = applyOp(next, op);
  return next;
}
