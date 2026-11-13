---
title: 视图、命令与 UI API
description: 可移植插件视图、命令与常驻 main 脚本的参考：节点、属性、事件、限制、生命周期与各客户端支持矩阵。
section: plugins
order: 12
sourceHash: "d9ffd59b929e"
---

本页是插件界面的参考资料：常驻的 **`main` 脚本**、`flux.ui` / `flux.commands` / `flux.rpc` / `flux.events` / `flux.timers` 这组 API、每种节点的属性、事件与限制，以及各客户端分别能显示什么。分步骤的教程见[用可移植视图构建界面](/docs/zh/plugins/ui-guide/)和[原生桌面界面](/docs/zh/plugins/desktop-ui-guide/)。

插件不需要自带任何 UI 代码也能展示界面。`main` 脚本返回一棵由**数据节点** `{ k, p, c }`（类型、属性、子节点）组成的树，桌面端（GPUI）和 Web 端用各自的原生控件、按当前主题渲染它。树就是数据，所以同一个视图在所有支持可移植视图的客户端上都能用，这种视图叫**可移植视图**。视图还可以另有**桌面实现**，见[桌面实现](#桌面实现)。

## 客户端支持

| `location` | 桌面端（GPUI） | Web 端 | 移动端 |
|---|---|---|---|
| `activity` | 可移植与桌面实现均可。活动栏中的一个页面。 | 仅可移植。页面地址为 `/plugins/<identity>/<viewId>`。 | 不渲染 |
| `settings` | 可移植与桌面实现均可。插件设置对话框中的一个标签页。 | 仅可移植。插件设置对话框中的标签页（仅当插件已启用、已加载且授权齐全时出现）。 | 不渲染 |
| `taskDetail` | 可移植与桌面实现均可。任务详情面板与独立任务详情窗口中的标签页。 | 仅可移植。 | 不渲染 |
| `window` | **仅桌面实现。** 从插件行打开。 | 没有入口。 | 不渲染 |
| `auth` | 可移植与桌面实现均可。插件**登录**对话框的全部内容（替换默认的站点 / 登录信息表单）；对话框标题仍为「登录 <插件名>」。 | 仅可移植。同左。 | 不渲染，使用默认登录流程 |

- 只有**已启用、已加载、授权已齐**的插件才会贡献视图和命令。
- Web 端只显示 `portable` 为 true 的视图。声明了 `desktop` 但没有 `portable: true` 的视图在 Web 端不会出现；直接访问它的地址会看到「视图会话已结束」占位，而不是「仅桌面端」卡片。
- 移动端保留了协议，但会忽略插件视图事件。
- 桌面端上，如果视图同时有两种实现，且桌面脚本文件在本机可读，就使用桌面脚本，否则使用可移植视图（见[回退规则](#回退规则)）。

## 声明视图与命令

```json
{
  "identity": "acme@task-board",
  "name": "Task Board",
  "version": "1.0.0",
  "description": "Lists download tasks in an activity-bar page.",
  "permissions": ["tasks"],
  "main": { "entry": "main.js", "events": ["onDone"] },
  "contributes": {
    "views": [
      { "id": "board", "title": "Task Board", "location": "activity" },
      { "id": "options", "title": "Options", "location": "settings" },
      { "id": "files", "title": "Files", "location": "taskDetail" }
    ],
    "commands": [
      { "id": "refresh", "title": "Refresh task board", "palette": true, "taskMenu": true }
    ]
  }
}
```

| 视图字段 | 规则 |
|---|---|
| `id` | 匹配 `^[a-z][a-z0-9-]{0,31}$`，在插件的视图中唯一。 |
| `title` | 1–40 个字符，用作页面 / 标签页 / 窗口标题。 |
| `location` | `activity`、`settings`、`taskDetail`、`window` 或 `auth`。每个插件至多一个 `settings` 视图、至多一个 `auth` 视图。`auth` 要求顶层声明 `auth`（见[自定义登录界面](/docs/zh/plugins/auth-guide/#自定义登录界面)）；旧版本应用会拒绝未知 location，请设置 `minAppVersion`。 |
| `portable` | 只能与 `desktop` 同时出现。`true` 表示 `main` 也实现了该视图。没有 `desktop` 的视图恒为可移植。 |
| `desktop` | `{ entry, networkHosts?, width?, height? }`，需要 `desktop` 权限，见[原生桌面界面](/docs/zh/plugins/desktop-ui-guide/)。 |

视图至多 16 个，命令至多 32 个。有可移植实现的视图（没有 `desktop`，或 `portable: true`）以及任何命令都要求声明 `main`。命令的 `id` 与视图 `id` 规则相同，`title` 为 1–40 个字符。全部校验规则见 [manifest 参考](/docs/zh/plugins/manifest/#contributes)。

## `main` 脚本

`main` 是一个**经典脚本**（不是 ES 模块），在 daemon 内的一个 QuickJS 虚拟机里执行一次，求值期间通过 `flux.*` 注册一切。除下文的 API 外，`main` 与其他入口脚本一样拥有 `flux.fetch`、`flux.storage`、`flux.fs`、`flux.logger`、`flux.info`、`flux.settings`；声明了对应权限时还有 `flux.auth`、`flux.ffmpeg` / `flux.ffprobe` 和 `flux.ytdlp`（见 [API 参考](/docs/zh/plugins/api-reference/)）。

### 生命周期

- **启动。** `main` 按需启动：打开视图、执行命令、收到 `invoke`，或 `main.events` 中列出的事件触发时。设置 `main.background: true`（需要 `background` 权限）则随 daemon 启动并常驻。
- **空闲回收。** 非常驻的宿主在没有打开的视图会话、没有定时器、没有进行中的调用时，5 分钟后关闭。
- **所有逻辑共用一个虚拟机。** 全局变量在调用之间保留。该插件的全部视图会话、命令、`invoke`、事件处理函数和定时器共用这一个虚拟机，并且**一次只执行一个**；某个处理函数里一个很慢的 `await` 会拖慢其他所有调用。
- **预算。** 每次调用最多 2 秒 CPU、30 秒墙钟时间；虚拟机内存最多 64 MiB。
- **致命失败。** 调用超出 CPU / 墙钟预算或内存上限时，宿主会被丢弃：JavaScript 状态全部丢失，定时器取消，该插件所有打开的视图会话以 `pluginUnloaded` 结束。客户端显示「视图会话已结束」和**重新打开**按钮；重新打开会启动新宿主并重新执行 `init`。连续三次失败会触发熔断并禁用插件（见[权限与审计](/docs/zh/plugins/permissions/)）。
- **重新加载。** 插件的 manifest 或 `main` 源码变化时，以及插件被禁用时，宿主同样会被丢弃（开发模式插件：点插件行的**重新加载**，它会重读 manifest 和全部脚本）。已打开的会话以 `pluginUnloaded` 结束。

### 示例

上面的 manifest 加上 `main.js`：

```js
const { h } = flux.ui;
const STATUS = ["Pending", "Downloading", "Paused", "Completed", "Error", "Preparing"];
const open = new Set(); // 已打开会话的 state

async function reload(state) {
  state.tasks = await flux.tasks.list(); // 需要 `tasks` 权限
}

async function refreshAll() {
  for (const state of open) {
    await reload(state);
    state.ctx.refresh();
  }
}

flux.ui.view("board", {
  async init(ctx) {
    const state = { ctx, tasks: [], filter: "" };
    await reload(state);
    open.add(state);
    return state;
  },

  render(state) {
    const needle = state.filter.toLowerCase();
    const rows = state.tasks
      .filter((t) => t.fileName.toLowerCase().includes(needle))
      .map((t) => ({
        key: t.taskId, // 字符串、非空、唯一
        name: t.fileName,
        size: t.totalBytes,
        done: t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : 0,
        status: STATUS[t.status] ?? String(t.status),
      }));
    return h("stack", { gap: "md", padding: "md" },
      h("heading", { text: "Task board" }),
      h("stack", { key: "toolbar", direction: "row", gap: "sm", align: "center" },
        h("input", { key: "filter", value: state.filter, placeholder: "Filter by name" }),
        h("button", { key: "refresh", text: "Refresh", variant: "primary" })),
      h("table", {
        key: "tasks",
        columns: [
          { key: "name", title: "Name" },
          { key: "size", title: "Size", kind: "bytes" },
          { key: "done", title: "Done", kind: "percent" },
          { key: "status", title: "Status", kind: "badge" },
        ],
        rows,
        rowActions: [
          { id: "pause", label: "Pause" },
          { id: "resume", label: "Resume" },
          { id: "remove", label: "Remove", tone: "danger" },
        ],
        emptyText: "No tasks",
      }));
  },

  async event(state, ev) {
    if (ev.node === "root/toolbar/filter" && ev.type === "change") {
      state.filter = String(ev.value);
    } else if (ev.node === "root/toolbar/refresh" && ev.type === "press") {
      await reload(state);
    } else if (ev.node === "root/tasks" && ev.type === "rowAction") {
      if (ev.action === "pause") await flux.tasks.pause(ev.key);
      if (ev.action === "resume") await flux.tasks.resume(ev.key);
      if (ev.action === "remove") await flux.tasks.remove(ev.key, false);
      await reload(state);
    }
  },

  dispose(state) {
    open.delete(state);
  },
});

flux.commands.register("refresh", refreshAll);
flux.events.on("onDone", refreshAll);
```

`flux.tasks.list()` 返回的任务对象使用 camelCase 字段（`taskId`、`fileName`、`totalBytes`、`downloadedBytes`、`status`、`url` 等），**没有 `id` 字段**。只有 manifest 声明了 `tasks` 权限时才有 `flux.tasks`，否则 `flux.tasks` 是 `undefined`。

## `flux.ui.view`

```ts
flux.ui.view(id: string, def: {
  init?(ctx): State | Promise<State>;
  render(state, ctx): Node | Promise<Node>;
  event?(state, ev, ctx): void | State | Promise<void | State>;
  dispose?(state): void | Promise<void>;
})
```

`id` 必须是非空字符串，并与已声明的视图 id 一致；重复注册同一个 id 会替换先前的定义。`render` 必填。打开一个已声明但没有注册的视图会失败：`view not registered: <id>`。

| 回调 | 何时执行 | 说明 |
|---|---|---|
| `init(ctx)` | 客户端打开会话时。 | 返回初始 state；返回 `undefined` 则保持默认的 `{}`。 |
| `render(state, ctx)` | `init` 之后、每次 `event` 之后、`ctx.refresh()` 之后，以及每个 `flux.events.on` 处理函数之后（对所有已打开的会话）。 | 必须返回节点。返回 `null` / `undefined` 会失败：`render() returned no node`。把它当作 `state` 的纯函数。 |
| `event(state, ev, ctx)` | 用户操作了会发出事件的节点时。 | 原地修改 `state`，或返回一个值来替换 state。之后会自动渲染。 |
| `dispose(state)` | 会话关闭时。 | 宿主被丢弃时不会调用。 |

### `ctx`

| 字段 | 取值 |
|---|---|
| `session` | 这个打开的会话的唯一 id。 |
| `props` | 目前恒为 `{}`：桌面端和 Web 端都不会传 props。 |
| `taskId` | `taskDetail` 视图所属的任务；其他位置为 `null`。 |
| `locale` | 会话打开时客户端的界面语言（如 `zh-CN`、`en`）；客户端未提供时为空字符串。用户之后切换语言不会更新它。 |
| `refresh()` | 请求重新渲染该会话。把 `ctx` 保存起来（例如放进 `state`），就能在定时器、命令或事件处理函数里触发刷新。 |
| `close()` | 关闭承载该视图的容器并结束会话：登录对话框（`auth`）、设置对话框（`settings`）或独立窗口。嵌入页面的视图（活动栏页、任务详情标签页）没有容器，改为显示「视图会话已结束」和**重新打开**按钮。重复调用不做任何事。可以在 `init` 里调用（例如用户已经登录）：客户端显示视图后立即关闭。 |
| `navigate(target)` | 返回 Promise。让主窗口到前台并切换到某个页面。`target` 为 `"downloads"`、`"rss"`、`"webhooks"`，或用 `{ view: "<id>" }` 指向本插件的某个 `activity` 视图。未知目标会以 `navigate: unknown target …` reject；`{ view }` 不是本插件的 `activity` 视图时以 `navigate: view '<id>' is not an activity view of this plugin` reject；在 `close()` 之后调用会以 `navigate: the view has been closed; call navigate() before close()` reject。可以在 `init` 里调用。目标页面在该客户端不可用（例如 Web 端无法显示仅桌面实现的视图）时，客户端忽略该请求并记录警告日志。只影响打开该会话的那个客户端。 |

`ViewContext`、`ViewEvent`、`ViewDefinition` 这些 TypeScript 类型由 `fluxdown-plugin-sdk` 导出（见 [SDK 参考](/docs/zh/plugins/sdk/)）。

示例：一个活动栏页面添加下载后跳到下载列表：

```js
flux.ui.view("add", {
  init: (ctx) => ({ ctx, url: "" }),
  render: (state) =>
    h("stack", { gap: "sm", padding: "md" },
      h("input", { key: "url", value: state.url, placeholder: "https://…" }),
      h("button", { key: "add", text: "Add and show downloads", variant: "primary" })),
  async event(state, ev) {
    if (ev.node === "root/url" && ev.type === "change") state.url = String(ev.value);
    if (ev.node === "root/add" && ev.type === "press") {
      await flux.tasks.create({ request: { url: state.url } }); // 需要 `tasks` 权限
      await state.ctx.navigate("downloads");
    }
  },
});
```

### 事件对象

`event(state, ev, ctx)` 收到的 `ev`：

| 字段 | 出现于 | 含义 |
|---|---|---|
| `ev.node` | 总是 | 触发事件的节点 id，例如 `root/toolbar/refresh`。 |
| `ev.type` | 总是 | `press`、`change`、`submit`、`rowAction` 或 `select`。 |
| `ev.value` | `change`、`submit` | `switch` / `checkbox` 为 `boolean`，其余为 `string`。 |
| `ev.key` | `rowAction`、`select` | 行 / 列表项的 key。 |
| `ev.action` | `rowAction` | 被点击的行操作的 `id`。 |

**事件分发。** 给每个可交互节点设置 `key`，按 `ev.node` 匹配；对表格、列表这类集合，则按 `ev.type` 加 `ev.key` / `ev.action` 匹配。节点 id 由根（`root`）和各级祖先的 key 拼成：`root/toolbar/filter` 表示根的直接子节点 `toolbar` 里 key 为 `filter` 的节点。没有 key 的子节点用它在兄弟中的位置（`root/0/1`），布局一变就会错位，所以凡是会触发事件的节点都应该设 key。表格的行和列表项的事件由**表格 / 列表节点**发出，用 `ev.key` 区分是哪一行。

### 事件时序

1. 客户端发出事件，daemon 把它入队并立即返回。
2. 同一会话的事件按发送顺序依次处理，并且与该插件的所有其他调用串行执行（见[生命周期](#生命周期)）。
3. 处理函数之后执行 `render`。同一会话的渲染节流为每 100 毫秒一次；期间再请求的渲染会被合并，只发送最新的一棵树。
4. daemon 把新树与上一棵比较，只把**差异**发给客户端，因此大表格也很省。
5. 会话 90 秒收不到保活就会被关闭；客户端每 30 秒发送一次保活。

如果 `event` 或之后的 `render` 抛出异常、或产生了非法的树，视图会在顶部显示带错误信息的错误条，会话保持打开；下一次成功渲染后错误条消失。如果**首帧**（`init` 之后的第一次渲染）失败，打开会话本身就失败：客户端显示加载失败页，并带**重新打开**按钮。

## `main` 的其他 API

| API | 说明 |
|---|---|
| `flux.ui.h(kind, props, ...children)` | 构造 `{ k, p, c }`。子节点会被展平；`null`、`undefined`、`true`、`false` 会被丢弃。字符串和数字**不是**合法的子节点。`kind` 是函数时，以 `{ ...props, children }` 调用它（组件）。 |
| `flux.ui.Fragment` | 不产生节点，只用来分组子节点；不能作为视图根。 |
| `flux.ui.toast(level, text)` | `info`、`success`、`warning` 或 `error`（其他值按 `info` 显示）。它不属于任何会话，会广播给**所有已连接的客户端**（桌面端与 Web 端都显示，移动端忽略）。 |
| `flux.commands.register(id, handler)` | `handler({ taskIds })`。执行未注册的命令会失败：`command not registered: <id>`。 |
| `flux.rpc.handle(name, handler, opts?)` | 处理函数收到 `(args, ctx)`，可返回任何可 JSON 序列化的值（`undefined` 变成 `null`）。供你自己的桌面视图用 `fluxdown.invoke(name, args)` 调用（`ctx = { caller: "desktop" }`）。传入 `opts = { page: true }` 后，你的[页面脚本](/docs/zh/plugins/page-scripts-guide/)也能调用它（`ctx = { caller: "page", pageUrl }`，属于不可信输入）。未知名称会失败：`rpc not registered: <name>`；页面调用没有 `page: true` 的处理函数会失败：`rpc not exposed to page: <name>`。 |
| `flux.events.on(name, handler)` | `name` 是 `main.events` 中的某一项（`onStart`、`onDone`、`onError`、`onCancel`、`onMetaProbed`）。同一个名称可以有多个监听者；某个监听者抛错只记一条警告日志，不影响其他监听者。之后所有打开的会话都会重新渲染。载荷见[钩子指南](/docs/zh/plugins/hooks-guide/)。 |
| `flux.timers.setTimeout / setInterval / clear` | 同时存在的定时器至多 64 个（`too many timers (max 64)`）。有视图会话打开时间隔被钳制到 ≥ 1 秒，否则 ≥ 60 秒。定时器本身不会让视图重新渲染，需要调用 `ctx.refresh()`。 |
| `flux.tasks.*` | `list()`、`get(id)`、`create(params)`、`pause(id)`、`resume(id)`、`remove(id, deleteFiles?)`，需要 `tasks` 权限。`create` 的参数就是 `daemon.task.create` 的参数（例如 `{ request: { url, startPaused } }`），每个插件每分钟至多 20 次。 |
| `flux.settings.update({ key: "value" })` | 写入插件自己的设置（字符串值，按 `manifest.settings` 校验）。 |

## 节点参考

每个节点都是一个普通对象 `{ k, p, c }`：`k` 是类型（字符串），`p` 是属性（对象，可省略），`c` 是子节点（数组，可省略）。出现其他字段会被拒绝（`unknown node field`）。`h` 函数与 [SDK](/docs/zh/plugins/sdk/#jsx) 的 JSX 运行时产出的正是这种形状。JSX 运行时会丢弃 `null`、`undefined` 和 `false` 子节点（不丢弃 `true`）；无论哪种写法，字符串或数字子节点都会被拒绝，报错 `a node must be an object {k, p, c}`——文字请放进 `text` 节点。

### 校验规则

| 规则 | 说明 |
|---|---|
| 未知属性 | 被拒绝：`<kind> unknown prop "x"; allowed props: …`。 |
| 必填属性 | 缺失时报错：`<kind> missing required prop "x"`。必填属性在下面的表中标出。 |
| 默认值 | 省略的可选属性取表中所示的默认值。 |
| 类型 | 严格。`string` 属性不接受数字（`text: 5` 会失败，请写 `String(5)`）；`bool` 属性不接受 `"true"`；整数必须是整数；枚举必须完全匹配。 |
| `null` | 值为 `null` 的属性等同于未提供（使用默认值，或报「缺少必填属性」）。唯一允许以 `null` 作为值的是 `progress.value`（不确定进度）。数组项字段里的 `null` 同样视为缺失。 |
| 子节点 | 只有容器（`stack`、`scroll`、`card`、`section`）接受子节点。 |
| `key` | 每个节点都可以带，校验后不再属于 `p`。字符串必须非空且不含 `/`；数字必须是整数。同级之间必须唯一。 |
| 节点 id | 根为 `root`，子节点为 `<父 id>/<key>`（没有 key 时用下标）。 |
| 表格行 / 列表项 | `key` **必填**，必须是非空**字符串**（数字 key 会被拒绝），且在数组内唯一。 |
| 数组项 | 未知字段被拒绝；表格 `rows` 例外，它在 `key` 之外携带任意单元格值。 |
| 限制 | 节点总数 5,000，深度 64，表格 10,000 行，列表 5,000 项，图片 `src` 1 MiB，单帧 1 MiB（更大的更新改为整树替换）。 |

凡是会触发事件的节点、表格行和列表项都应使用稳定的 key：这样更新才是增量的，焦点和滚动位置也才能保持。

违反规则的树不会显示。首帧之后，视图会在错误条里显示错误信息，信息里指出出错的节点和问题，例如：

```
invalid view node root/tasks: <table> prop "rows"[0].key: expected a string, got a number
invalid view node root/toolbar: duplicate sibling id: another child of root already uses it (keys must be unique; a child without key uses its position as key)
```

### 公共取值

| 枚举 | 取值 |
|---|---|
| tone | `neutral`、`accent`、`success`、`warning`、`danger`、`info`、`muted` |
| gap（也用于 `padding`） | `none`、`xs`、`sm`、`md`、`lg`、`xl` |
| size | `xs`、`sm`、`md`、`lg`、`xl` |
| direction | `row`、`column` |
| align | `start`、`center`、`end`、`stretch` |
| justify | `start`、`center`、`end`、`between` |
| weight | `normal`、`medium`、`semibold` |
| button variant | `primary`、`outline`、`ghost`、`danger` |
| input kind | `text`、`password`、`number` |
| column kind | `text`、`bytes`、`speed`、`percent`、`progress`、`time`、`badge` |

tone 跟随当前主题：`neutral` → `colors.foreground`，`accent` → `colors.primary`，`success` → `colors.success`，`warning` → `colors.warning`，`danger` → `colors.destructive`，`info` → `colors.info`，`muted` → `colors.mutedForeground`。未知的 tone 不合法；可移植视图里没有办法直接指定颜色值。

### 布局与容器

| 类型 | 属性（默认值） | 子节点 |
|---|---|---|
| `stack` | `direction`（column）、`gap`（sm）、`align`（stretch）、`justify`（start）、`wrap`（false）、`padding`（none）、`grow`（false） | 有 |
| `scroll` | `maxHeight`，数字 ≥ 0（不限制） | 有 |
| `card` | `title`、`padding`（md） | 有 |
| `section` | `title` **必填**、`description` | 有 |
| `divider` | — | 无 |

### 内容

| 类型 | 属性（默认值） | 说明 |
|---|---|---|
| `text` | `text` **必填**、`tone`（neutral）、`size`（sm）、`weight`（normal）、`mono`（false）、`truncate`（false） | 一段文字。 |
| `heading` | `text` **必填**、`level` 整数 1–3（2） | |
| `markdown` | `text` **必填** | 只支持段落和代码块。 |
| `code` | `text` **必填** | 等宽代码块。 |
| `link` | `text` **必填**、`href` **必填** | `href` 必须是格式正确、不含空白的 `http://` 或 `https://` 地址。 |
| `image` | `src` **必填**、`alt`（""）、`width` ≥ 0、`height` ≥ 0 | `src` 为 `data:image/png;base64,…`、`data:image/jpeg;base64,…` 或 `data:image/webp;base64,…`，base64 内容非空，整体不超过 1 MiB。不接受远程 URL。 |
| `badge` | `text` **必填**、`tone`（neutral） | |
| `progress` | `value` 为 0–1 的数字或 `null`（`null`）、`label` | `null` 显示不确定进度条。 |
| `spinner` | `label` | |
| `kv` | `items` 数组（`[]`） | 每项：`key` **字符串、必填**，`value` **字符串、必填**。 |
| `callout` | `text` **必填**、`tone`（info）、`title` | |
| `empty` | `title` **必填**、`description` | |

### 控件

| 类型 | 属性（默认值） | 事件 |
|---|---|---|
| `button` | `text` **必填**、`variant`（outline）、`disabled`（false）、`loading`（false） | `press` |
| `switch` | `label`、`checked`（false）、`disabled`（false） | `change`——`ev.value` 为布尔值 |
| `checkbox` | `label`、`checked`（false）、`disabled`（false） | `change`——布尔值 |
| `select` | `options` **必填**、`value`、`placeholder`、`disabled`（false） | `change`——`ev.value` 为所选选项的 `value` |
| `input` | `value`（""）、`placeholder`、`kind`（text）、`disabled`（false） | `change`（带去抖，见下）、`submit` |
| `textarea` | `value`（""）、`placeholder`、`rows` 整数 1–50（4）、`disabled`（false） | `change`（带去抖） |
| `tabs` | `items` **必填**、`value` **必填** | `change`——所选项的 `value` |

`select.options` 与 `tabs.items` 的每一项：`value` **字符串、必填**，`label` **字符串、必填**。`tabs` 只绘制标签条，所选 `value` 对应的内容由你自己渲染。

### 数据

| 类型 | 属性（默认值） | 事件 |
|---|---|---|
| `table` | `columns` **必填**、`rows`（`[]`，≤ 10,000）、`rowActions`、`emptyText` | `rowAction`（`ev.key`、`ev.action`）、`select`（`ev.key`） |
| `list` | `items`（`[]`，≤ 5,000） | `select`（`ev.key`） |

- `table.columns[]`：`key` **字符串、必填**，`title` **字符串、必填**，`kind`（text），`width` 数字 ≥ 0。
- `table.rows[]`：`key` **非空字符串、必填、唯一**；其余字段是单元格值，按列的 `key` 取用。
- `table.rowActions[]`：`id` **字符串、必填**，`label` **字符串、必填**，`tone`。
- `list.items[]`：`key` **非空字符串、必填、唯一**，`title` **字符串、必填**，`description`，`badge`（字符串），`tone`。

列的 `kind` 决定单元格值如何格式化：`text`（字符串）、`bytes`（字节数）、`speed`（每秒字节数）、`percent`（0 到 1 的数字）、`progress`（0 到 1 的数字，画成进度条）、`time`（unix 毫秒，按查看者本地时区显示）、`badge`（字符串，或 `{ text, tone }`）。

## 受控值与文本输入

所有控件都是**受控**的：你渲染出的属性才是唯一的事实来源，事件只是「请求修改」。把事件里的值存进 state，下一帧再渲染回去。

- `switch`、`checkbox`、`tabs` 在两端都显示你渲染的 `checked` / `value`；处理函数不更新 state，控件就会弹回旧值。
- `select` 也应当这样处理。（Web 端会弹回；桌面端可能在你的 `value` 变化之前一直保留用户的选择，所以不要依赖任何一种行为。）
- `input` 与 `textarea` 在用户输入时会保留他输入的内容：重新渲染不会覆盖正在聚焦的输入框，也不会覆盖与用户最近一次发出的值相同的内容。失焦后才采用帧里的值。
- `input` / `textarea` 的 `change` **去抖 300 毫秒**，失焦时立即发送。
- 在 `input` 里按回车，会先把尚未发出的 `change` 冲刷出去，再发 `submit`。所以处理函数总会先看到带最终文本的 `change`，再看到 `submit`；`submit` 的 `ev.value` 是当时的文本。
- `kind: "number"` 的 `input`，`ev.value` 仍然是**字符串**。
- `textarea` 没有 `submit` 事件。

## 命令

命令在 `contributes.commands` 中声明，用 `flux.commands.register(id, handler)` 实现。

| 标志 | 出现位置 | `taskIds` |
|---|---|---|
| `palette: true` | 桌面端：命令面板，标题为 `<插件名>: <命令标题>`。Web 端：插件活动页页头里的一个按钮。 | 桌面端：下载页当前选中的任务 id（没有选中则为空）。Web 端：恒为 `[]`。 |
| `taskMenu: true` | 下载页的任务右键菜单（桌面端与 Web 端）。 | 选中的任务 id。 |

处理函数与其他调用一样在 `main` 里执行。抛出异常时客户端会弹出错误通知；成功时不会有任何提示，除非你自己调用 `flux.ui.toast`。

## 打开、关闭与重新打开

- 桌面端的 `activity` 页面在第一次打开该页面时创建，并在主窗口存续期间缓存。Web 端的地址是 `/plugins/<identity>/<viewId>`（两段都经 URL 编码）。
- 每个打开的视图在 daemon 端有一个会话。它会在客户端关闭视图、保活停止 90 秒（原因 `expired`），或插件宿主被丢弃（原因 `pluginUnloaded`）时结束。客户端显示「视图会话已结束」和**重新打开**；重新打开会创建新会话并重新执行 `init`，所以状态会丢失。
- daemon 重启或连接中断后，客户端会先尝试重新同步会话；如果 daemon 已不认识它，就重新打开一个新的。
- 以下情况打开会失败并显示错误页：插件不存在（`plugin not found: <identity>`）、已禁用（`plugin disabled: <identity>`）、没有 `main`（`plugin has no main: <identity>`）、视图未声明（`view not declared: <id>`）或视图只有桌面实现（`view is desktop-only: <id>`）。

## 桌面实现

视图可以声明 `desktop`（权限 `desktop`）：一个由桌面端在自身进程内运行的 ES 模块，用来做更丰富、完全原生的界面。桌面脚本以**完全信任**运行，发布前请阅读[权限与审计](/docs/zh/plugins/permissions/)；详细教程见[原生桌面界面](/docs/zh/plugins/desktop-ui-guide/)。

| 声明 | 桌面端 | Web 端 |
|---|---|---|
| 只有 `desktop` | 桌面脚本 | 不显示 |
| `desktop` + `portable: true` | 桌面脚本（脚本文件不可读时用可移植视图） | `main` 提供的可移植视图 |
| 没有 `desktop` | 可移植视图 | 可移植视图 |

`fluxdown` 模块只在桌面脚本里可用：`identity()`、`taskId()`、`call(method, params)`、`invoke(name, args)`、`theme()`、`locale()`、`toast(level, text)`，详见[原生桌面界面](/docs/zh/plugins/desktop-ui-guide/#fluxdown-模块)。

### 回退规则

桌面端在创建视图时为每个视图选择实现：

1. 视图有 `desktop`，并且脚本文件在**本机**的包路径上存在，就使用桌面脚本。如果它随后加载失败，或挂载时抛出异常，显示的是错误面板，**不会**自动回退到可移植视图。
2. 否则（没有 `desktop`，或包不可读，例如桌面端连接的是另一台机器上的 daemon），视图是可移植的就使用可移植视图。
3. 否则显示「该视图仅在桌面端可用」的提示。
