---
title: 构建可移植界面
description: 一步步教程：编写能同时在桌面端与 Web 端运行的插件界面——常驻 main 脚本、视图、事件、表单、实时任务数据、命令、设置页与任务详情页签。
section: plugins
order: 7
sourceHash: "60cfa046e6c4"
---

插件无需携带任何界面代码就能提供真正的 UI。常驻的 **`main` 脚本**返回一棵由纯数据节点（`stack`、`button`、`table` 等）组成的树；桌面端与 Web 端用各自的原生控件和当前主题渲染这棵树。树只是数据，所以同一份实现可以在所有会渲染插件视图的客户端上运行。本文一步步构建这样的界面，最后给出完整插件：实时任务表格、快速添加表单、两个命令、一个设置页和一个任务详情页签。

这是**可移植**路线。如果需要完全原生的桌面控件、独立窗口，或要使用桌面端的本机资源，请接着阅读[构建桌面原生界面](/docs/zh/plugins/desktop-ui-guide/)——同一个视图可以同时提供两种实现。完整的节点种类与属性请查阅[视图参考](/docs/zh/plugins/views/)。如果想用 TypeScript 和 JSX 编写同样的代码，见 [TypeScript 项目](/docs/zh/plugins/typescript-project/)。

**准备工作：**带插件开发模式的 FluxDown（以开发模式安装文件夹的方法见[编写第一个插件](/docs/zh/plugins/your-first-plugin/)）和一个文本编辑器。示例是纯 JavaScript，无需构建。

## 1. `main` 脚本如何运行

请先通读本节，后面的内容都建立在它之上。

**一个脚本，一个常驻 VM。**`main.js` 是*经典脚本*（不能用 `import` / `export`）。插件的宿主启动时，daemon 会对它求值一次；求值期间它通过 `flux.*` 注册视图、命令、RPC 处理器、事件监听和定时器。此后顶层变量会在宿主存活期间一直保留，并被该插件的所有视图与命令共享。

**两类状态。**

| 状态 | 存放位置 | 生命周期 |
|---|---|---|
| 会话状态 | 视图 `init` 返回、并传给 `render` / `event` 的对象 | 一个已打开的视图。视图关闭时丢弃；Web 端离开该视图所在页面就会关闭它。 |
| 宿主状态 | `main.js` 顶层的变量 | 宿主存活期间（见下）。宿主被释放后丢失。 |

需要跨越宿主重启保留的数据，请放进 `flux.storage`（见 [API 参考](/docs/zh/plugins/api-reference/)）或插件设置。

**所有调用逐个串行执行。**宿主是单个 actor。打开视图、UI 事件、刷新、命令、定时器触发和下载事件都会排队，每一个执行完毕后才轮到下一个。由此得到两条结论：

- 一个耗时的 `await`（`flux.fetch`、长时间的任务循环）会拖慢该插件的**所有**视图和命令。处理函数要尽量短。
- 在处理函数内部用 `ctx.refresh()` 请求的重新渲染，会在该处理函数**返回之后**才发生。同一个处理函数里无法先显示一帧「加载中」再继续。

**何时启动、何时停止。**

- 宿主按需启动：打开视图、执行命令、收到列在 `main.events` 中的事件，或桌面视图调用 `fluxdown.invoke` 时。
- 普通宿主在**空闲五分钟**后被释放——即没有打开的视图会话、没有待触发的定时器、也没有进行中的调用。释放后所有宿主状态丢失，下次使用时会重新对 `main.js` 求值。因此只要存在一个活动的 `setInterval`，宿主就不会被回收。
- 声明 `"main": { "background": true }`（需要 `background` 权限）时，宿主随 daemon 启动，并且不会因空闲被回收。

**预算。**每次调用——对 `main.js` 求值、渲染、事件、命令、定时器触发——最多可用 30 秒墙钟时间、2 秒 CPU 时间（等待 `flux.fetch`、`flux.tasks.*` 等宿主调用的时间不计入 CPU）和 64 MiB 内存。一旦某次调用超限：

1. 宿主连同其全部 JS 状态一起被丢弃。
2. 该插件所有已打开的视图随之结束；客户端显示「视图会话已结束」和 **重新打开** 按钮。出错的那次调用会报告 `脚本执行超时` 或 `脚本超出内存上限`。
3. 失败计入熔断器：连续三次失败，插件会被自动禁用；一次成功调用会清零计数。

如果客户端 90 秒没有发送保活（例如浏览器标签页被挂起），视图会话同样会结束，客户端同样提供 **重新打开**。

**重新加载。**修改开发模式插件的 `manifest.json` 或 `main.js` 后，请在 设置 → 扩展 → 插件 中点击该插件行上的 **重新加载** 按钮（提示文字「重新加载（重读 manifest 与脚本）」）。如果 manifest 或 `main.js` 有变化，运行中的宿主会被替换：已打开的视图结束，需要重新打开。

## 2. 第一个视图

一个视图需要两样东西：manifest 中的声明，以及 `main.js` 中使用相同 id 的 `flux.ui.view` 注册。

`manifest.json`：

```json
{
  "identity": "example@hello-ui",
  "name": "Hello UI",
  "version": "0.1.0",
  "main": { "entry": "main.js" },
  "contributes": {
    "views": [{ "id": "hello", "title": "Hello", "location": "activity" }]
  }
}
```

`main.js`：

```js
const { h } = flux.ui;

flux.ui.view("hello", {
  render: (state, ctx) =>
    h("stack", { gap: "md", padding: "md" },
      h("heading", { text: "Hello from main.js" }),
      h("text", { text: `Session ${ctx.session}`, tone: "muted", size: "xs" })),
});
```

把两个文件放进一个文件夹，以开发模式安装并批准权限对话框，活动栏里就会出现 **Hello** 入口（各 `location` 的显示位置见[第 9 节](#9-各内容出现在哪里)）。打开它会启动宿主、建立会话并调用你的 `render`。

各部分的含义：

- `view(id, definition)` —— `id` 必须是 `contributes.views` 中已声明视图的 `id`（小写字母、数字和 `-`，最长 32 个字符，以字母开头）。定义里 `render` 必填，`init`、`event`、`dispose` 可选，它们都可以是 `async`。
- `init(ctx)` 构造会话的初始状态，默认是 `{}`；`render(state, ctx)` 把状态变成一棵树；`dispose(state)` 在会话关闭时执行。
- `h(kind, props, ...children)` 构造一个节点，等价于 `{ k: kind, p: props, c: [children] }`。子节点会被展平，`null`、`undefined`、`true` 和 `false` 会被丢弃，所以 `cond ? node : null` 与 `list.map(...)` 都可以直接用。第一个参数也可以是函数，此时会以 `{ ...props, children }` 调用它，因此可以用普通函数编写自己的组件。
- 根必须是一个节点，不能是 `undefined`。如果 `render` 什么也没返回，视图会以 `render() returned no node` 失败。

传给 `init`、`render`、`event` 的 `ctx` 对象：

| 字段 | 含义 |
|---|---|
| `session` | 这个已打开视图会话的唯一 id。 |
| `taskId` | `taskDetail` 视图所关联的任务；其他位置恒为 `null`。 |
| `locale` | 视图打开时客户端的界面语言。用户之后切换语言不会更新它。 |
| `props` | 为「带参数打开视图」预留。当前客户端始终传入 `{}`。 |
| `refresh()` | 请求重新渲染本会话。请求会被合并；同一会话每 100 ms 最多渲染一次。 |
| `close()` | 关闭承载该视图的对话框或独立窗口并结束会话。嵌入页面的视图（活动栏页、任务详情标签页）只会显示「已结束」和**重新打开**按钮。重复调用无副作用；可以在 `init` 里调用。 |
| `navigate(target)` | `"downloads"`、`"rss"`、`"webhooks"` 或 `{ view }`（本插件的某个 `activity` 视图）。让主窗口到前台并切换页面；目标非法或在 `close()` 之后调用时，返回的 Promise 会 reject。可以在 `init` 里调用。客户端无法显示目标页面时忽略该请求并记录警告日志。只影响打开该视图的那个客户端。示例：`await flux.tasks.create(...)` 之后调用 `await ctx.navigate("downloads")`。 |

> **子节点只能是节点，不能是字符串。**`h("stack", {}, "hello")` 是非法的。文本要放进 `text` 节点：`h("text", { text: "hello" })`。要求字符串的属性也必须传字符串：`text: 5` 会被拒绝，请用 `String(5)`。

### TypeScript 与 JSX

用 [`fluxdown-plugin-sdk`](/docs/zh/plugins/typescript-project/) 以 TSX 编写同样的视图：

```tsx
import { defineMain } from "fluxdown-plugin-sdk/runtime";

defineMain((flux) => {
  flux.ui.view<{ clicks: number }>("hello", {
    init: () => ({ clicks: 0 }),
    render: (state) => (
      <stack gap="md" padding="md">
        <heading text="Hello" />
        <button key="inc" text={`Clicked ${state.clicks} times`} variant="primary" />
      </stack>
    ),
    event: (state, ev) => {
      if (ev.type === "press") state.clicks += 1;
    },
  });
});
```

每个小写标签就是一种节点，`key` 成为节点的 key，`null` / `false` / `undefined` 子节点会被丢弃。文本仍然是属性——写成 `<text text="…" />`，而不是 JSX 文本子节点。属性和事件的形状会按 daemon 校验所用的同一份 schema 做类型检查。下文的内容对 JSX 同样适用。

## 3. 处理事件

可交互的节点会向视图的 `event(state, ev, ctx)` 发送**事件**。`event` 返回后，宿主会再次调用 `render`，并只向客户端发送*差异*。

```js
flux.ui.view("hello", {
  init: () => ({ clicks: 0 }),
  render: (state) =>
    h("stack", { gap: "md", padding: "md" },
      h("button", { key: "inc", text: `Clicked ${state.clicks} times`, variant: "primary" }),
      h("button", { key: "reset", text: "Reset" })),
  event: (state, ev) => {
    if (ev.type !== "press") return;
    if (ev.node === "root/inc") state.clicks += 1;
    if (ev.node === "root/reset") state.clicks = 0;
  },
});
```

第 3、4 节的代码片段都假定已有第 2 节的 `const { h } = flux.ui;`，并会替换 `hello` 视图，可以依次试用。

`ev` 的字段：

| 字段 | 出现在 | 含义 |
|---|---|---|
| `node` | 全部 | 触发事件的节点 id。 |
| `type` | 全部 | `press`、`change`、`submit`、`rowAction` 或 `select`。 |
| `value` | `change`、`submit` | 新值：字符串；`switch` / `checkbox` 为布尔值。 |
| `key` | `rowAction`、`select` | 行或列表项的 `key`。 |
| `action` | `rowAction` | 被点击的行操作的 `id`。 |

| 节点 | 事件 |
|---|---|
| `button` | `press` |
| `switch`、`checkbox` | `change`，`value` 为布尔值 |
| `select`、`tabs` | `change`，`value` 为字符串 |
| `input` | `change`（去抖）、`submit`（回车），`value` 为字符串 |
| `textarea` | `change`（去抖），`value` 为字符串 |
| `table` | `rowAction`（`key`、`action`）、`select`（`key`：被点击的行） |
| `list` | `select`（`key`） |

**节点 id 与 key。**根节点的 id 是 `root`。子节点的 id 是 `<父 id>/<key>`，没有 `key` 时是 `<父 id>/<下标>`。同级节点的 key 不能重复。由此得出两条规则：

- 给每个会触发事件的节点、以及每个条件出现的节点都加上 `key`。`null` 子节点会在**计算位置之前**被移除，所以没有 key 的节点，其 id 会随前面兄弟节点的出现或消失而变化。
- 完整 id 取决于嵌套层级（如 `root/add/form-0/url`），所以在较大的视图里不要比较整个 id，而是按最后一段分发，并保证可交互节点的 key 在整个视图内唯一：

```js
const keyOf = (ev) => ev.node.slice(ev.node.lastIndexOf("/") + 1);

event: async (state, ev) => {
  switch (`${ev.type}:${keyOf(ev)}`) {
    case "press:save": /* … */ return;
    case "change:name": state.name = ev.value; return;
    case "rowAction:tasks": /* ev.key, ev.action */ return;
  }
},
```

**`event` 的返回值。**修改 `state` 并且什么也不返回（常规写法）。如果返回了非 `undefined` 的值，它会**替换**整个状态——所以像 `(state, ev) => state.count += 1` 这样的箭头函数会返回一个数字，悄悄地把你的状态替换成数字。

**出错时。**如果 `event` 或 `render` 抛出异常，或者树不符合 schema，客户端会在**上一棵合法的树**上方显示错误条，会话保持打开；下一次成功渲染会清除它。异常发生前你对 `state` 做的修改会保留。如果第一次渲染就失败，视图无法打开：客户端会显示错误信息和 **重新打开** 按钮。

**其他触发重新渲染的方式。**视图自己的 `event` 之后，宿主会自动重新渲染。对于并非来自用户事件的变化——定时器、命令、下载事件——请对你保存下来的 ctx 调用 `ctx.refresh()`（见第 5 节）。

## 4. 表单

输入控件是**受控**的：客户端显示最新那棵树里写的值。你需要在 `change` 事件里把每个值存进 `state`，再渲染回去。

```js
flux.ui.view("hello", {
  init: () => ({ name: "", style: "plain", loud: false, result: "" }),
  render: (state) =>
    h("stack", { gap: "sm", padding: "md" },
      h("input", { key: "name", value: state.name, placeholder: "Your name" }),
      h("select", {
        key: "style",
        value: state.style,
        options: [{ value: "plain", label: "Plain" }, { value: "formal", label: "Formal" }],
      }),
      h("switch", { key: "loud", label: "Shout", checked: state.loud }),
      h("button", { key: "greet", text: "Greet", variant: "primary" }),
      state.result ? h("callout", { key: "result", tone: "success", text: state.result }) : null),
  event: (state, ev) => {
    switch (`${ev.type}:${ev.node.split("/").pop()}`) {
      case "change:name": state.name = ev.value; return;
      case "change:style": state.style = ev.value; return;
      case "change:loud": state.loud = ev.value; return;
      case "submit:name": // Enter in the input
      case "press:greet": {
        const hello = state.style === "formal" ? "Good day" : "Hello";
        const text = `${hello}, ${state.name || "stranger"}!`;
        state.result = state.loud ? text.toUpperCase() : text;
        return;
      }
    }
  },
});
```

各控件的行为：

| 控件 | 行为 |
|---|---|
| `switch`、`checkbox`、`select`、`tabs` | 严格显示最新那棵树里的 `checked` / `value`。如果你不保存新值并渲染回去，控件会回弹到旧值。 |
| `input`、`textarea` | 用户的输入立即显示。最后一次按键 300 ms 后发出 `change`，失去焦点时立即发出。`input` 在回车时还会发出 `submit`，并且之前一定先冲刷出一次 `change`。 |
| `kind: "number"` 的 `input` | `value` 仍然是**字符串**，需要自己转换。 |
| 所有控件 | `disabled: true` 会让控件置灰。`button` 还有 `loading: true`。 |

**获得焦点的输入框不会被覆盖。**输入框正在聚焦，或它最后输入的值还在等待你的渲染结果时，新树里不同的 `value` 会被忽略，待失去焦点后才应用。要在代码里清空或重置某个字段，请改变该字段（或包裹它的容器）的 `key`——客户端会据此创建一个全新的控件。最终插件就是在提交成功后用表单计数器改变容器的 key：

```js
h("stack", { key: `form-${state.formVersion}`, gap: "sm" },
  h("input", { key: "url", value: state.url }),
  /* … */)
// after a successful submit:
state.url = "";
state.formVersion += 1;
```

**在处理函数里校验**，并用 `callout` 节点展示问题。不要因为控件本身有限制就信任它给出的值。

## 5. 实时数据：任务、事件与定时器

### 读取并控制下载

只有 manifest 中有 `"permissions": ["tasks"]` 时才存在 `flux.tasks`。没有该权限时 `flux.tasks` 是 `undefined`，第一次访问就会在你的代码里抛出 `TypeError`。用户安装插件时必须批准该权限；在此之前，插件的视图和命令不会出现在任何位置。

| 调用 | 结果 |
|---|---|
| `flux.tasks.list()` | 任务对象数组。 |
| `flux.tasks.get(id)` | 一个任务对象。 |
| `flux.tasks.create({ request: { url, startPaused? } })` | `{ taskId }`。每个插件每分钟最多创建 20 个，超出后抛出 `tasks.create rate limited`。 |
| `flux.tasks.pause(id)`、`flux.tasks.resume(id)` | 暂停或继续。 |
| `flux.tasks.remove(id, deleteFiles)` | 删除任务；`deleteFiles` 默认为 `false`。 |

任务对象（驼峰命名）包含以下字段（不限于此）：

| 字段 | 类型 | 含义 |
|---|---|---|
| `taskId` | string | 任务 id，用作行的 `key`。 |
| `url`、`fileName`、`saveDir` | string | 来源、文件名和保存目录。 |
| `status` | number | `0` 等待中、`1` 下载中、`2` 已暂停、`3` 已完成、`4` 失败、`5` 准备中。 |
| `downloadedBytes`、`totalBytes` | number | 进度。大小未知时 `totalBytes` 不是正数。 |
| `errorMessage` | string | 失败原因；否则为空。 |
| `createdAt` | string | 创建时间，字符串形式的 Unix **秒**。 |

创建、暂停、继续和删除任务都会记入插件的审计日志，见[权限与审计](/docs/zh/plugins/permissions/)。

### 带行操作的表格

`table` 接收 `columns`、`rows` 和可选的 `rowActions`。一行是一个对象，带一个**字符串** `key`（唯一、非空），以及每个列 `key` 对应的值。列的 `kind` 决定值的格式：`bytes`、`speed`、`percent`、`progress` 接收数字（后两者取 0–1），`time` 接收 Unix **毫秒**，`badge` 接收字符串或 `{ text, tone }`，`text`（默认）可显示任意值。

```js
const STATUS = {
  0: { text: "Pending", tone: "muted" },
  1: { text: "Downloading", tone: "accent" },
  2: { text: "Paused", tone: "warning" },
  3: { text: "Completed", tone: "success" },
  4: { text: "Failed", tone: "danger" },
  5: { text: "Preparing", tone: "info" },
};

const toRow = (t) => ({
  key: t.taskId,
  name: t.fileName || t.url,
  size: t.totalBytes > 0 ? t.totalBytes : null,
  progress: t.totalBytes > 0 ? Math.min(1, t.downloadedBytes / t.totalBytes) : null,
  status: STATUS[t.status] || { text: `Status ${t.status}`, tone: "neutral" },
  created: (Number(t.createdAt) || 0) * 1000,
});

h("table", {
  key: "tasks",
  columns: [
    { key: "name", title: "Name" },
    { key: "size", title: "Size", kind: "bytes", width: 100 },
    { key: "progress", title: "Progress", kind: "progress", width: 140 },
    { key: "status", title: "Status", kind: "badge", width: 120 },
    { key: "created", title: "Added", kind: "time", width: 170 },
  ],
  rows: tasks.slice(0, 500).map(toRow),
  rowActions: [
    { id: "pause", label: "Pause" },
    { id: "resume", label: "Resume", tone: "success" },
    { id: "remove", label: "Remove", tone: "danger" },
  ],
  emptyText: "No tasks match this filter.",
})
```

点击行操作会得到 `{ type: "rowAction", key: <行 key>, action: <操作 id> }`；点击行本身会得到 `{ type: "select", key }`。表格最多 10,000 行，整棵树最多 5,000 个节点，所以较长的列表要自行截断。

### 让数据保持最新

daemon 不会向插件推送下载进度事件，所以变化有三个来源：

1. **用户事件**——你的 `event` 处理函数更新数据，随后的重新渲染是自动的。
2. **下载事件**——把它们列在 `main.events` 里（`onStart`、`onDone`、`onError`、`onCancel`、`onMetaProbed`），并用 `flux.events.on(name, handler)` 监听。它们对每个任务都会触发。处理函数返回后，宿主会重新渲染所有打开的视图。没有列在 `main.events` 里的事件永远不会投递，也不会有任何报错。
3. **定时器**——进度靠轮询。定时器本身不会触发重新渲染，需要对应该更新的会话调用 `ctx.refresh()`。

```js
const sessions = new Map(); // session id -> ctx
let pollTimer = null;

function refreshAll() {
  for (const ctx of sessions.values()) ctx.refresh();
}

function startPolling(seconds) {
  if (pollTimer !== null) return;
  pollTimer = flux.timers.setInterval(async () => {
    await loadTasks();
    refreshAll();
  }, seconds * 1000);
}

function stopPolling() {
  if (pollTimer === null) return;
  flux.timers.clear(pollTimer);
  pollTimer = null;
}

// in the view definition:
//   init(ctx)      { sessions.set(ctx.session, ctx); startPolling(3); … return { session: ctx.session, … }; }
//   dispose(state) { sessions.delete(state.session); if (sessions.size === 0) stopPolling(); }
```

这些片段里的 `tasks` 和 `loadTasks()` 分别是宿主级变量和对 `flux.tasks.list()` 的封装；两者都在文末的完整插件里定义。

定时器规则：

| API | 行为 |
|---|---|
| `flux.timers.setTimeout(fn, ms)` | 一次性；返回数字 id。不会被钳制。 |
| `flux.timers.setInterval(fn, ms)` | 重复执行。插件至少有一个打开的视图会话时，间隔至少为 **1 秒**，否则至少 **60 秒**。每次重新设定定时器时都会重新判断；第一次钳制会记一条日志。 |
| `flux.timers.clear(id)` | 取消两种定时器。 |
| 限制 | 每个插件最多 64 个存活的定时器。回调可以是 `async`；其中的异常只记日志，不会中止 interval。 |

只在视图打开时轮询（在 `init` 里启动、在 `dispose` 里停止），可以让宿主在用户离开后进入空闲并被回收。

## 6. 命令

命令出现在命令面板和任务右键菜单中，并在 `main` 里执行代码。

Manifest（需要 `main`；最多 32 个命令，标题 1–40 个字符，id 规则与视图相同）：

```json
"contributes": {
  "commands": [
    { "id": "pause-selected", "title": "Pause selected tasks", "palette": true, "taskMenu": true },
    { "id": "refresh-board", "title": "Refresh task board", "palette": true }
  ]
}
```

处理函数：

```js
flux.commands.register("pause-selected", async ({ taskIds }) => {
  if (taskIds.length === 0) {
    flux.ui.toast("warning", "Select at least one task first.");
    return;
  }
  let paused = 0;
  for (const id of taskIds) {
    try {
      await flux.tasks.pause(id);
      paused += 1;
    } catch (e) {
      flux.logger.warn(`pause ${id} failed: ${e.message}`);
    }
  }
  flux.ui.toast(paused === taskIds.length ? "success" : "warning", `Paused ${paused} of ${taskIds.length} task(s).`);
});
```

- 处理函数收到 `{ taskIds }`——一个字符串数组。**务必处理空数组。**
  - *任务右键菜单*（`taskMenu: true`）：选中的本机任务。
  - *桌面命令面板*（`palette: true`）：主窗口下载页当前选中的任务；面板条目的标题是 `<插件名>: <命令标题>`。
  - *Web 端*没有命令面板：`palette` 命令会变成插件活动页页头的按钮，执行时 `taskIds` 为 `[]`。因此如果插件的唯一界面是 `palette` 命令，又没有活动视图，它在 Web 端就没有入口。
- 如果处理函数抛出异常，客户端会用错误 toast 显示错误信息。成功时正常返回即可。
- `flux.ui.toast(level, text)` 会在所有已连接的桌面端与 Web 客户端显示通知。`level` 取 `info`、`success`、`warning` 或 `error`，其他值按 `info` 处理。
- manifest 中声明了、却没有注册的命令会以 `command not registered: <id>` 失败。

## 7. 设置页

设置有两层，可以组合使用：

- manifest 中的 `settings[]` 声明有类型的字段（见 [Manifest 参考](/docs/zh/plugins/manifest/)）。FluxDown 据此生成表单——**基础设置**页签——并把值以 `flux.settings.<key>` 暴露出来，类型对应：字符串、数字、布尔值。
- `"location": "settings"` 的视图（每个插件至多一个）会在它旁边增加一个 **插件设置** 页签，由你的 `main` 渲染。适合自定义布局、校验、预览或操作。

```json
"contributes": {
  "views": [{ "id": "preferences", "title": "Preferences", "location": "settings" }]
},
"settings": [
  { "key": "refreshSeconds", "title": "Refresh interval (seconds)", "type": "number", "min": 1, "max": 60, "default": "3" },
  { "key": "notifyOnDone", "title": "Notify when a download finishes", "type": "boolean", "default": "true" }
]
```

请至少声明一个 `settings[]` 字段：`flux.settings.update` 只接受已声明的键，而且 Web 端只会为声明了字段的插件在插件行上显示设置按钮。设置对话框通过 **设置 → 扩展 → 插件** 中插件行上的齿轮按钮打开。

用 `flux.settings.update(entries)` 写入值：

```js
async function savePreferences(state) {
  try {
    await flux.settings.update({
      refreshSeconds: state.refreshSeconds.trim(), // values must be strings,
      notifyOnDone: String(state.notifyOnDone),    // even for numbers and booleans
    });
    state.notice = { tone: "success", text: "Saved." };
  } catch (e) {
    state.notice = { tone: "danger", text: e.message };
  }
}
```

- 值必须是**字符串**：`"5"`、`"true"`。传非字符串值会导致调用失败。
- 调用会按 manifest 规则（类型、`min` / `max`、`select` 选项、`pattern`）校验每一项，并且是**全有或全无**：任何一项不合法，整批都会被拒绝，什么也不会写入。错误信息是引擎的原文，例如 `'refreshSeconds' 大于上限 60` 或 `未知设置项 'x'`，请原样展示给用户。
- 保存的值会在当前调用结束后才进入 `flux.settings`。在同一个处理函数里 `await` 之后立刻读取 `flux.settings.x`，得到的仍是旧值：请把刚保存的值存进自己的状态，或显式传递（最终插件重启轮询定时器时就是这样做的）。
- 用户在**基础设置**页签里做的修改同样会就地更新 `flux.settings`，但不会自动触发已打开视图的重新渲染。

## 8. 任务详情页签

`"location": "taskDetail"` 的视图会成为任务详情面板里的一个页签。视图会针对选中的任务打开，`ctx.taskId` 就是该任务的 id。

```json
{ "id": "detail", "title": "Board", "location": "taskDetail" }
```

```js
flux.ui.view("detail", {
  async init(ctx) {
    track(ctx); // register for polling, see section 5
    await loadTasks();
    return { session: ctx.session };
  },
  render(state, ctx) {
    const task = tasks.find((t) => t.taskId === ctx.taskId);
    if (!task) return h("empty", { title: "Task not found", description: "It may have been removed." });
    return h("stack", { gap: "sm", padding: "md" },
      h("heading", { text: task.fileName || "Unnamed task", level: 3 }),
      h("button", { key: "pause", text: "Pause" }));
  },
  async event(state, ev, ctx) {
    if (ev.type === "press" && ctx.taskId) await flux.tasks.pause(ctx.taskId);
  },
  dispose(state) {
    untrack(state.session);
  },
});
```

务必处理任务已不存在的情况：渲染一个 `empty` 节点，而不是返回 `undefined`。下面的完整插件使用共享的任务列表；如果想单独读取一次最新数据，可以改用 `flux.tasks.get(ctx.taskId)`。

## 9. 各内容出现在哪里

| 贡献内容 | 桌面端（GPUI） | Web 端 | 移动端 |
|---|---|---|---|
| `activity` 视图 | 活动栏中的一个入口，以视图标题命名；入口图标为 manifest `icon` 声明的 SVG（未声明时为默认图标）。 | 活动栏中的一个入口，图标同样取自 `icon` 声明的 SVG（未声明时为默认图标）；页面地址是 `/plugins/<identity>/<viewId>`（每一段都经过 URL 编码，所以 `@` 变成 `%40`）。页头显示插件名、视图标题，以及以按钮形式出现的 `palette` 命令。 | 不渲染 |
| `taskDetail` 视图 | 任务详情面板和独立任务详情窗口中的一个页签。 | 任务详情面板中的一个页签。 | 不渲染 |
| `settings` 视图 | 插件设置对话框中的 **插件设置** 页签。 | 同左。 | 不渲染 |
| `palette` 命令 | 命令面板，显示为 `<插件名>: <标题>`。 | 活动页页头的按钮。 | — |
| `taskMenu` 命令 | 任务右键菜单。 | 任务右键菜单。 | — |
| `window` 视图 | 仅桌面实现；见[桌面界面](/docs/zh/plugins/desktop-ui-guide/)。 | 不显示。 | 不渲染 |
| `auth` 视图 | 插件**登录**对话框的内容（见[自定义登录界面](/docs/zh/plugins/auth-guide/#自定义登录界面)）。 | 同左（仅可移植）。 | 不渲染 |

移动端保留了协议，但目前不渲染插件视图。只有已启用、加载成功并且已获用户完整批准的插件，其贡献内容才会出现。

如果一个视图同时声明了 `desktop` 入口和 `"portable": true`，它会在能运行桌面实现的地方使用桌面实现，在其他地方回退到你的 `main` 视图；没有 `portable: true` 时，Web 端根本不会显示它。见[构建桌面原生界面](/docs/zh/plugins/desktop-ui-guide/)。

## 10. 常见错误

| 现象 | 原因与解决 |
|---|---|
| `invalid view node root/0: a node must be an object {k, p, c}` | 把字符串（或数字）当作子节点传入了。请把文本包进 `text` 节点。 |
| `invalid view node root/1: <text> prop "text": expected a string, got a number` | 属性是严格类型的，数字不会自动转换。请用 `String(n)`。`kv` 的条目值以及 `list` / `select` 的标签同样适用。 |
| `<table> prop "rows"[0].key: expected a string, got a number` 或 `key must not be empty` | 行和列表项的 key 必须是非空字符串。请用 `String(id)` 或任务 id。 |
| `duplicate sibling id: another child of root already uses it …` 或 `duplicate key "…"` | 两个同级节点（或两行）的 `key` 相同。请让 key 唯一。 |
| `<button> unknown prop "label"; allowed props: …` | 未知属性会被拒绝。请到[视图参考](/docs/zh/plugins/views/)核对该节点。 |
| `<text> missing required prop "text"` | 缺少必填属性；参考页对每一个都有标注。 |
| `render() returned no node` | `render` 返回了 `undefined` / `null`（常见原因是带花括号的箭头函数漏写 `return`）。请返回一个节点，例如 `h("empty", …)`。 |
| `view not registered: <id>` | manifest 声明了该视图，但 `main.js` 没有用该 id 调用过 `flux.ui.view`（或 id 拼写有误）。 |
| 在 `main.js` 中注册的视图始终不出现 | 它没有在 `contributes.views` 中声明，或插件未启用 / 未获批准。 |
| 访问 `flux.tasks` 时 `TypeError` | manifest 缺少 `"permissions": ["tasks"]`。 |
| `flux.events.on("onDone", …)` 从不触发 | `main.events` 中缺少 `onDone`。 |
| 按钮的 `loading` 状态始终不显示 | 处理函数里的 `await` 在下一次渲染前就已结束；见第 1 节。先完成工作，再渲染结果。 |
| 状态突然变成数字或字符串 | `event` 返回了一个值。请使用不带 `return 值` 的花括号函数体。 |
| 已清空的输入框仍保留文字 | 输入框仍有焦点；请改变它（或其容器）的 `key` 来重置，见第 4 节。 |
| 视图显示「视图会话已结束」 | 宿主已被释放：预算超限、插件被重新加载或禁用，或保活中断。点击 **重新打开**。日志和更多错误信息见[调试与排错](/docs/zh/plugins/debugging/)。 |
| 内存持续增长 | 保存在模块级集合里的 ctx 从未被移除。请在 `dispose` 里删除。 |

排查问题时用 `flux.logger.info(...)` / `warn` / `error` 输出诊断信息；日志落在哪里见[调试与排错](/docs/zh/plugins/debugging/)。

## 完整插件

把两个文件保存到同一个文件夹，以开发模式安装，批准 `tasks` 权限，然后从活动栏打开 **Task Board**。在表单里添加一个下载并观察表格更新；通过插件行上的齿轮按钮打开 **插件设置**；打开某个任务的详情面板，查看 **Board** 页签；从命令面板或任务右键菜单执行 **Pause selected tasks**。

`manifest.json`：

```json
{
  "identity": "example@taskboard",
  "name": "Task Board",
  "version": "1.0.0",
  "description": "A live task table with a quick-add form, commands and a settings page.",
  "permissions": ["tasks"],
  "main": { "entry": "main.js", "events": ["onDone", "onError"] },
  "contributes": {
    "views": [
      { "id": "board", "title": "Task Board", "location": "activity" },
      { "id": "detail", "title": "Board", "location": "taskDetail" },
      { "id": "preferences", "title": "Preferences", "location": "settings" }
    ],
    "commands": [
      { "id": "pause-selected", "title": "Pause selected tasks", "palette": true, "taskMenu": true },
      { "id": "refresh-board", "title": "Refresh task board", "palette": true }
    ]
  },
  "settings": [
    {
      "key": "refreshSeconds",
      "title": "Refresh interval (seconds)",
      "description": "How often the Task Board reloads while it is open.",
      "type": "number",
      "min": 1,
      "max": 60,
      "default": "3"
    },
    {
      "key": "notifyOnDone",
      "title": "Notify when a download finishes",
      "type": "boolean",
      "default": "true"
    }
  ]
}
```

`main.js`：

```js
// Task Board — resident `main` script (a classic script: no import/export).
const { h } = flux.ui;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
const MAX_ROWS = 500;

// TaskDto.status codes.
const STATUS = {
  0: { text: "Pending", tone: "muted" },
  1: { text: "Downloading", tone: "accent" },
  2: { text: "Paused", tone: "warning" },
  3: { text: "Completed", tone: "success" },
  4: { text: "Failed", tone: "danger" },
  5: { text: "Preparing", tone: "info" },
};
const FILTER_STATUSES = { all: null, active: [0, 1, 5], paused: [2], done: [3], failed: [4] };
const FILTERS = [
  { value: "all", label: "All" },
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
  { value: "done", label: "Completed" },
  { value: "failed", label: "Failed" },
];
const MODES = [
  { value: "now", label: "Start now" },
  { value: "paused", label: "Add paused" },
];
const COLUMNS = [
  { key: "name", title: "Name" },
  { key: "size", title: "Size", kind: "bytes", width: 100 },
  { key: "progress", title: "Progress", kind: "progress", width: 140 },
  { key: "status", title: "Status", kind: "badge", width: 120 },
  { key: "created", title: "Added", kind: "time", width: 170 },
];
const ROW_ACTIONS = [
  { id: "pause", label: "Pause" },
  { id: "resume", label: "Resume", tone: "success" },
  { id: "remove", label: "Remove", tone: "danger" },
];

// ---------------------------------------------------------------------------
// Host-wide state: shared by every view session, lost when the host is reclaimed
// ---------------------------------------------------------------------------
let tasks = [];
let loadError = "";
const sessions = new Map(); // session id -> ctx of the views that show live data
let pollTimer = null;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
const errorText = (e) => String((e && e.message) || e);
const keyOf = (ev) => ev.node.slice(ev.node.lastIndexOf("/") + 1);

function formatBytes(n) {
  if (!(n > 0)) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

const statusOf = (task) => STATUS[task.status] || { text: `Status ${task.status}`, tone: "neutral" };
const fractionOf = (task) =>
  task.totalBytes > 0 ? Math.min(1, task.downloadedBytes / task.totalBytes) : null;

function countOf(codes) {
  return tasks.filter((t) => codes.includes(t.status)).length;
}

function toRow(task) {
  return {
    key: task.taskId, // must be a non-empty string, unique within the table
    name: task.fileName || task.url,
    size: task.totalBytes > 0 ? task.totalBytes : null,
    progress: fractionOf(task),
    status: statusOf(task), // badge cell: { text, tone }
    created: (Number(task.createdAt) || 0) * 1000, // createdAt is Unix seconds; `time` wants ms
  };
}

async function loadTasks() {
  try {
    tasks = await flux.tasks.list();
    loadError = "";
  } catch (e) {
    loadError = errorText(e);
  }
}

function refreshAll() {
  for (const ctx of sessions.values()) ctx.refresh();
}

// Poll only while a live view is open, so the host can go idle afterwards.
function startPolling(seconds) {
  if (pollTimer !== null) return;
  pollTimer = flux.timers.setInterval(async () => {
    await loadTasks();
    refreshAll();
  }, Math.max(1, seconds || 3) * 1000);
}

function stopPolling() {
  if (pollTimer === null) return;
  flux.timers.clear(pollTimer);
  pollTimer = null;
}

function track(ctx) {
  sessions.set(ctx.session, ctx);
  startPolling(Number(flux.settings.refreshSeconds));
}

function untrack(session) {
  sessions.delete(session);
  if (sessions.size === 0) stopPolling();
}

// ---------------------------------------------------------------------------
// View: "board" (activity bar)
// ---------------------------------------------------------------------------
async function addDownload(state) {
  const url = state.url.trim();
  if (!/^https?:\/\//i.test(url)) {
    state.notice = { tone: "warning", text: "Enter a full http:// or https:// URL." };
    return;
  }
  try {
    const { taskId } = await flux.tasks.create({
      request: { url, startPaused: state.mode === "paused" },
    });
    state.notice = { tone: "success", text: `Added task ${taskId}.` };
    state.url = "";
    state.formVersion += 1; // new key => the client builds a fresh, empty input
    await loadTasks();
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
}

async function runRowAction(state, taskId, action) {
  try {
    if (action === "pause") await flux.tasks.pause(taskId);
    else if (action === "resume") await flux.tasks.resume(taskId);
    else if (action === "remove") await flux.tasks.remove(taskId, false); // keep the files
    state.notice = { tone: "info", text: `${action}: ${taskId}` };
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
  await loadTasks();
}

flux.ui.view("board", {
  async init(ctx) {
    track(ctx);
    await loadTasks();
    return { url: "", mode: "now", filter: "all", formVersion: 0, notice: null, session: ctx.session };
  },

  render(state) {
    const allowed = FILTER_STATUSES[state.filter];
    const visible = tasks.filter((t) => allowed === null || allowed.includes(t.status));
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h(
        "stack",
        { key: "summary", direction: "row", gap: "sm", align: "center" },
        h("heading", { text: "Task board", level: 2 }),
        h("badge", { key: "total", text: `${tasks.length} total`, tone: "accent" }),
        h("badge", { key: "active", text: `${countOf(FILTER_STATUSES.active)} active`, tone: "info" }),
      ),
      loadError
        ? h("callout", { key: "load-error", tone: "danger", title: "Could not load tasks", text: loadError })
        : null,
      state.notice ? h("callout", { key: "notice", tone: state.notice.tone, text: state.notice.text }) : null,
      h(
        "card",
        { key: "add", title: "Add a download" },
        h(
          "stack",
          { key: `form-${state.formVersion}`, gap: "sm" },
          h("input", { key: "url", value: state.url, placeholder: "https://example.com/file.zip" }),
          h(
            "stack",
            { direction: "row", gap: "sm", align: "center" },
            h("select", { key: "mode", value: state.mode, options: MODES }),
            h("button", { key: "add", text: "Add download", variant: "primary" }),
          ),
        ),
      ),
      h("tabs", { key: "filter", value: state.filter, items: FILTERS }),
      h("table", {
        key: "tasks",
        columns: COLUMNS,
        rows: visible.slice(0, MAX_ROWS).map(toRow),
        rowActions: ROW_ACTIONS,
        emptyText: "No tasks match this filter.",
      }),
      visible.length > MAX_ROWS
        ? h("text", { key: "more", text: `Showing the first ${MAX_ROWS} of ${visible.length} tasks.`, tone: "muted", size: "xs" })
        : null,
    );
  },

  async event(state, ev) {
    switch (`${ev.type}:${keyOf(ev)}`) {
      case "change:url":
        state.url = ev.value;
        return;
      case "change:mode":
        state.mode = ev.value;
        return;
      case "change:filter":
        state.filter = ev.value;
        return;
      case "submit:url":
        state.url = ev.value;
        await addDownload(state);
        return;
      case "press:add":
        await addDownload(state);
        return;
      case "rowAction:tasks":
        await runRowAction(state, ev.key, ev.action);
        return;
    }
  },

  dispose(state) {
    untrack(state.session);
  },
});

// ---------------------------------------------------------------------------
// View: "detail" (task detail tab; ctx.taskId is the selected task)
// ---------------------------------------------------------------------------
flux.ui.view("detail", {
  async init(ctx) {
    track(ctx);
    await loadTasks();
    return { session: ctx.session };
  },

  render(state, ctx) {
    const task = tasks.find((t) => t.taskId === ctx.taskId);
    if (!task) {
      return h("empty", { title: "Task not found", description: "It may have been removed." });
    }
    const status = statusOf(task);
    const fraction = fractionOf(task);
    return h(
      "stack",
      { gap: "sm", padding: "md" },
      h(
        "stack",
        { direction: "row", gap: "sm", align: "center" },
        h("heading", { text: task.fileName || "Unnamed task", level: 3 }),
        h("badge", { text: status.text, tone: status.tone }),
      ),
      h("progress", {
        value: fraction,
        label: fraction === null ? "Size unknown" : `${Math.round(fraction * 100)}%`,
      }),
      h("kv", {
        items: [
          { key: "Task id", value: task.taskId },
          { key: "URL", value: task.url },
          { key: "Saved to", value: task.saveDir },
          {
            key: "Downloaded",
            value: `${formatBytes(task.downloadedBytes)} of ${task.totalBytes > 0 ? formatBytes(task.totalBytes) : "unknown"}`,
          },
          { key: "Error", value: task.errorMessage || "None" },
        ],
      }),
      h(
        "stack",
        { direction: "row", gap: "sm" },
        h("button", { key: "pause", text: "Pause" }),
        h("button", { key: "resume", text: "Resume", variant: "primary" }),
      ),
    );
  },

  async event(state, ev, ctx) {
    if (ev.type !== "press" || !ctx.taskId) return;
    try {
      if (keyOf(ev) === "pause") await flux.tasks.pause(ctx.taskId);
      if (keyOf(ev) === "resume") await flux.tasks.resume(ctx.taskId);
    } catch (e) {
      flux.ui.toast("error", errorText(e));
    }
    await loadTasks();
  },

  dispose(state) {
    untrack(state.session);
  },
});

// ---------------------------------------------------------------------------
// View: "preferences" (the plugin's settings dialog)
// ---------------------------------------------------------------------------
async function savePreferences(state) {
  try {
    // Values must be strings, even for number and boolean settings.
    await flux.settings.update({
      refreshSeconds: state.refreshSeconds.trim(),
      notifyOnDone: String(state.notifyOnDone),
    });
    state.notice = { tone: "success", text: "Saved." };
    if (pollTimer !== null) {
      // flux.settings still holds the old values until this call returns, so pass the new one.
      stopPolling();
      startPolling(Number(state.refreshSeconds));
    }
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
}

flux.ui.view("preferences", {
  init() {
    return {
      refreshSeconds: String(flux.settings.refreshSeconds),
      notifyOnDone: flux.settings.notifyOnDone === true,
      notice: null,
    };
  },

  render(state) {
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h("text", { text: "Refresh interval while the board is open (1–60 seconds).", tone: "muted" }),
      h("input", { key: "refresh", value: state.refreshSeconds, placeholder: "3", kind: "number" }),
      h("switch", { key: "notify", label: "Notify when a download finishes", checked: state.notifyOnDone }),
      state.notice ? h("callout", { key: "notice", tone: state.notice.tone, text: state.notice.text }) : null,
      h("button", { key: "save", text: "Save", variant: "primary" }),
    );
  },

  async event(state, ev) {
    switch (`${ev.type}:${keyOf(ev)}`) {
      case "change:refresh":
        state.refreshSeconds = ev.value;
        return;
      case "change:notify":
        state.notifyOnDone = ev.value;
        return;
      case "submit:refresh":
        state.refreshSeconds = ev.value;
        await savePreferences(state);
        return;
      case "press:save":
        await savePreferences(state);
        return;
    }
  },
});

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------
flux.commands.register("pause-selected", async ({ taskIds }) => {
  if (taskIds.length === 0) {
    flux.ui.toast("warning", "Select at least one task first.");
    return;
  }
  let paused = 0;
  for (const id of taskIds) {
    try {
      await flux.tasks.pause(id);
      paused += 1;
    } catch (e) {
      flux.logger.warn(`pause ${id} failed: ${errorText(e)}`);
    }
  }
  flux.ui.toast(paused === taskIds.length ? "success" : "warning", `Paused ${paused} of ${taskIds.length} task(s).`);
  await loadTasks();
  refreshAll();
});

flux.commands.register("refresh-board", async () => {
  await loadTasks();
  refreshAll();
  flux.ui.toast("info", `Loaded ${tasks.length} task(s).`);
});

// ---------------------------------------------------------------------------
// Download lifecycle events (must be listed in manifest main.events)
// ---------------------------------------------------------------------------
flux.events.on("onDone", async (ev) => {
  if (flux.settings.notifyOnDone) flux.ui.toast("success", `Finished: ${ev.filePath}`);
  await loadTasks(); // open views are re-rendered automatically after the handler returns
});

flux.events.on("onError", async (ev) => {
  flux.ui.toast("error", `Download failed: ${ev.message}`);
  await loadTasks();
});
```

## 下一步

- [视图参考](/docs/zh/plugins/views/) —— 全部节点种类、属性、限制与事件。
- [构建桌面原生界面](/docs/zh/plugins/desktop-ui-guide/) —— 为桌面端提供完全原生的窗口与控件，与你的可移植视图并存。
- [TypeScript 项目](/docs/zh/plugins/typescript-project/) —— 同一个插件，带类型、JSX 与构建步骤。
- [调试与排错](/docs/zh/plugins/debugging/) —— 日志、重新加载行为与错误信息。
- [Manifest 参考](/docs/zh/plugins/manifest/)与[权限与审计](/docs/zh/plugins/permissions/)。
- [`feature-demo` 示例](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo)在一个插件里展示了全部节点种类。
