---
title: 原生桌面界面
description: 用 gpui-shell ES 模块为插件构建原生桌面窗口与嵌入式面板：manifest、View 类、fluxdown 模块、主题、信任模型与排错。
section: plugins
order: 8
sourceHash: "2b27d0ee563e"
---

**桌面脚本**是 FluxDown 桌面端（GPUI）在自身进程内运行的 ES 模块，用来绘制完全原生的界面：布局、控件和样式都由你自己决定，而不是使用[可移植视图](/docs/zh/plugins/ui-guide/)那套固定的节点类型。本指南会从零构建两个插件——一个独立**窗口**和一个**嵌入式面板**——然后讲清桌面脚本能做什么、不能做什么。

桌面脚本是进阶能力：它以**完全信任**运行，并且只存在于桌面端，不会在 daemon、Web 端或移动端运行。如果你的界面能用可移植节点表达，可移植视图更简单、更安全，而且各端通用。

## 选桌面还是可移植？

| | 可移植视图 | 桌面视图 |
|---|---|---|
| 写法 | `main` 脚本返回数据节点 `{ k, p, c }` | 一个 ES 模块，用 `View` 子类绘制 gpui 元素 |
| 运行位置 | daemon（QuickJS） | 桌面端进程（gpui-shell，QuickJS） |
| 客户端 | 桌面端、Web 端 | 仅桌面端 |
| 外观 | 固定的节点集合，由客户端套用主题 | gpui 能画的一切，样式由你的代码决定 |
| 位置 | `activity`、`settings`、`taskDetail`、`auth` | 五种全部，包括 `window` |
| 权限 | 只需 `main` 所需的权限 | `desktop`（风险等级**完全信任**） |
| 文件、进程、剪贴板、网络 | 只能经 `flux.*` API | 直接访问（网络仅限列出的主机） |
| 没打开窗口时是否运行 | 是（在 daemon 里） | 否 |

需要独立窗口、自定义绘制、本地文件 / 进程，或节点集合里没有的控件时选桌面；其余情况选可移植。同一个视图也可以**两者都提供**：桌面端优先用桌面脚本，Web 端用可移植视图（见[与可移植视图并存](#与可移植视图并存)）。

## 工作原理

- manifest 为视图声明一个 `desktop` 入口。该文件是插件包的一部分，引擎只校验它的路径和是否存在，从不执行它。
- 用户打开视图时，桌面端把文件加载进**该插件独立的 gpui-shell 运行时**，并挂载你默认导出的类。
- 脚本通过 `fluxdown` 模块与 FluxDown 交互，通过 `invoke` ↔ `flux.rpc.handle` 与插件自己的 `main` 脚本交互。
- 因为脚本是桌面进程里的受信任代码，安装带 `desktop` 的插件时会向用户展示完全信任的授权对话框（见[权限与审计](/docs/zh/plugins/permissions/)）。

## 教程一：独立窗口

我们来做 **Release Checker**：一个带「检查」按钮的窗口，从 GitHub API 取最新的 FluxDown 发行版并显示出来，同时通过插件的 `main` 脚本记住上次看到的版本。它不需要可移植视图，Web 端也没有任何界面。

### 1. manifest

```json
{
  "identity": "acme@release-checker",
  "name": "Release Checker",
  "version": "1.0.0",
  "description": "Looks up the latest FluxDown release in a native window.",
  "permissions": ["desktop"],
  "main": { "entry": "main.js" },
  "contributes": {
    "views": [
      {
        "id": "checker",
        "title": "Release Checker",
        "location": "window",
        "desktop": {
          "entry": "ui/release.js",
          "networkHosts": ["api.github.com"],
          "width": 520,
          "height": 360
        }
      }
    ]
  }
}
```

- 任何 `desktop` 入口都要求声明 `permissions: ["desktop"]`。
- `location: "window"` 让视图成为独立窗口。`width` / `height`（逻辑像素，240–4096，默认 960 × 640）只允许用在 `window` 视图上。
- `networkHosts` 列出脚本可以访问的每个主机（见[网络访问](#网络访问)）。没有 `api.github.com`，下面的 `fetch` 会被拒绝。
- 只有桌面实现的视图并不需要 `main`；这里加上它是为了演示 `invoke`。没有 `portable: true` 的视图不要求 `main`。

### 2. `main` 脚本

`main.js` 是经典脚本：用 `flux.storage`（无需任何权限）保存上次看到的 tag，并暴露两个 RPC 处理函数：

```js
flux.rpc.handle("lastSeen", async () => ({ tag: await flux.storage.get("lastSeenTag") }));

flux.rpc.handle("remember", async (args) => {
  const tag = args && typeof args.tag === "string" ? args.tag : "";
  if (tag === "") return { ok: false, error: "tag is required" };
  await flux.storage.set("lastSeenTag", tag);
  return { ok: true };
});
```

处理函数可以返回任何可 JSON 序列化的值。请把失败**当作数据返回**（`{ ok: false, error }`）：处理函数一旦抛出异常，桌面一侧只会收到错误码，收不到你写的信息（见[调用 `main`](#调用-maininvoke)）。

### 3. 窗口脚本

`ui/release.js`：

```js
import { View } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { invoke, toast } from "fluxdown";

const LATEST = "https://api.github.com/repos/zerx-lab/FluxDown/releases/latest";

export default class ReleaseWindow extends View {
  init(_props, cx) {
    this.cx = cx; // AsyncContext：await 之后仍然可用
    this.release = null;
    this.lastSeen = null;
    this.status = "点击「检查」查看最新发行版。";
    this.busy = false;
    cx.spawn(async (cx) => {
      try {
        const saved = await invoke("lastSeen");
        this.lastSeen = saved && saved.tag ? saved.tag : null;
      } catch (e) {
        this.status = "插件的 main 脚本不可用。";
      }
      cx.notify();
    });
  }

  check() {
    this.busy = true;
    this.status = "检查中…";
    this.cx.spawn(async (cx) => {
      try {
        const response = await fetch(LATEST, { headers: { Accept: "application/vnd.github+json" } });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const body = await response.json();
        this.release = { tag: body.tag_name, name: body.name, url: body.html_url };
        this.status = "完成";
        await invoke("remember", { tag: body.tag_name });
        this.lastSeen = body.tag_name;
        toast("success", `最新版本：${body.tag_name}`);
      } catch (e) {
        this.status = `检查失败：${e && e.message ? e.message : e}`;
        toast("error", this.status);
      }
      this.busy = false;
      cx.notify();
    });
  }

  render(cx) {
    const colors = cx.theme().colors;
    const column = v_flex().size_full().gap(12).p(20).bg(colors.background);
    column.child(Label.new("FluxDown 发行版").text_size(18));
    column.child(Label.new(this.status).text_color(colors.muted_foreground));
    if (this.release) {
      column.child(Label.new(`${this.release.name} (${this.release.tag})`));
      if (this.lastSeen === this.release.tag) {
        column.child(Label.new("已记为上次查看的版本").text_color(colors.muted_foreground));
      }
    }
    return column.child(
      h_flex()
        .gap(8)
        .child(
          Button.new("check")
            .label("检查")
            .primary()
            .loading(this.busy)
            .on_click(() => this.check()),
        )
        .child(
          Button.new("open")
            .label("打开页面")
            .disabled(!this.release)
            .on_click((_event, cx) => {
              if (this.release) cx.open_url(this.release.url);
            }),
        ),
    );
  }
}
```

要点：

- 模块**默认导出一个继承 `View` 的类**（`View` 从 `gpui-kit` 导入）。这个类就是整个界面。
- `init(props, cx)` 在视图创建时执行一次。把状态放在 `this` 上，长期存在的东西（任务、定时器）也在这里创建。
- `render(cx)` 返回用链式调用构建的 gpui 元素（`v_flex().gap(12).child(...)`）。它在视图失效时执行，**不是**每帧都执行。改了状态之后调用 `cx.notify()` 请求重新渲染。
- `cx.spawn(async (cx) => { ... })` 用来跑异步工作。它收到的 `cx` 在 `await` 之后依然有效；普通事件处理函数里的 `cx` 活不过该处理函数，所以 `init` 把 `AsyncContext` 存进了 `this.cx`。
- 这里没有浏览器 DOM，没有全局 `setTimeout`，也没有 `require`。计时用 `cx.timer.after / every` 或 `cx.sleep(ms)`，I/O 用 `fetch` 和 `fs/promises`。
- 不要把元素存在实例上，请在 `render` 里现建。

### 4. 安装并打开

1. 把三个文件放进一个文件夹：`manifest.json`、`main.js`、`ui/release.js`。
2. 在 FluxDown 打开 **设置 → 扩展 → 插件**，选择**从目录安装（开发模式）**，选中该文件夹。
3. 批准授权对话框。它会显示**完全信任**风险等级、`desktop` 权限和主机 `api.github.com`。
4. 找到这个插件所在的行。因为它有带桌面入口的 `window` 视图，行上会出现一个**窗口图标按钮**，悬停提示为视图标题。点击它，窗口以 520 × 360 居中打开，标题为 `Release Checker: Release Checker`（`<插件名>: <视图标题>`）。

这个行内按钮是用户打开 `window` 视图的**唯一**入口：没有命令面板项，也没有活动栏入口。出现的条件是：插件已启用、加载无误，且视图的 location 为 `window` 并带有 `desktop` 入口。再次点击会聚焦已经打开的窗口。

## 教程二：嵌入式面板

嵌入式视图位于主窗口内部：可以是**活动栏页面**、**插件设置对话框里的一个标签页**，或**任务详情面板里的一个标签页**。我们来做 **Task Inspector**，一个任务详情标签页，用 `fluxdown.call` 读取当前任务。

### 1. manifest

```json
{
  "identity": "acme@task-inspector",
  "name": "Task Inspector",
  "version": "1.0.0",
  "description": "Adds a native Inspector tab to the task details panel.",
  "permissions": ["desktop"],
  "contributes": {
    "views": [
      {
        "id": "inspector",
        "title": "Inspector",
        "location": "taskDetail",
        "desktop": { "entry": "ui/inspector.js" }
      }
    ]
  }
}
```

没有 `main`，也没有 `portable`：这个标签页只存在于桌面端。（这里不能写 `width` 和 `height`——它们只属于 `window` 视图。）

### 2. 脚本

`ui/inspector.js`：

```js
import { View } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { taskId, call, toast } from "fluxdown";

const STATUS = ["等待中", "下载中", "已暂停", "已完成", "出错", "准备中"];

// `fluxdown.call` 失败时，以序列化成 JSON 文本的 RPC 错误详情 reject。
function describeError(error) {
  const text = error && error.message ? error.message : String(error);
  try {
    const detail = JSON.parse(text);
    return detail.field ? `${detail.code} (${detail.field})` : String(detail.code);
  } catch {
    return text;
  }
}

export default class Inspector extends View {
  init(_props, cx) {
    this.cx = cx;
    this.task = null;
    this.error = null;
    this.loading = false;
    this.load();
  }

  load() {
    const id = taskId();
    if (id === null) {
      this.error = "请从某个任务的详情里打开此标签页。";
      return;
    }
    this.loading = true;
    this.cx.spawn(async (cx) => {
      try {
        this.task = await call("daemon.task.get", { taskId: id });
        this.error = null;
      } catch (e) {
        this.error = describeError(e);
        toast("error", `无法加载任务：${this.error}`);
      }
      this.loading = false;
      cx.notify();
    });
  }

  row(cx, name, value) {
    return h_flex()
      .gap(12)
      .child(Label.new(name).w(96).text_color(cx.theme().colors.muted_foreground))
      .child(Label.new(String(value)));
  }

  render(cx) {
    const task = this.task;
    const body = v_flex().gap(8).p(16);
    if (this.error) {
      body.child(Label.new(this.error).text_color(cx.theme().colors.destructive));
    } else if (task) {
      const percent = task.totalBytes > 0 ? Math.round((task.downloadedBytes / task.totalBytes) * 100) : 0;
      body
        .child(this.row(cx, "文件", task.fileName))
        .child(this.row(cx, "状态", STATUS[task.status] ?? task.status))
        .child(this.row(cx, "进度", `${percent}%`))
        .child(this.row(cx, "链接", task.url));
    } else {
      body.child(Label.new("加载中…"));
    }
    return body.child(
      Button.new("reload")
        .label("刷新")
        .loading(this.loading)
        .on_click((_event, cx) => {
          this.load();
          cx.notify();
        }),
    );
  }
}
```

嵌入式视图的特别之处：

- `taskId()` 对 `taskDetail` 视图返回标签页所属任务的 id（字符串），其他位置返回 `null`。FluxDown 会为每个（标签页，任务）创建一个视图并缓存，所以只在 `init` 里读一次 id 是正确的。
- `call("daemon.task.get", { taskId })` 发往本机 agent。任务对象使用 camelCase 字段（`taskId`、`url`、`fileName`、`status`、`downloadedBytes`、`totalBytes` 等）；`status` 是数字：0 等待、1 下载中、2 已暂停、3 已完成、4 出错、5 准备中。
- **嵌入式视图没有浮层宿主。** `window.push_toast`、`window.open_dialog`、`window.open_sheet` 只存在于独立窗口。在面板里请用 `fluxdown.toast` 提示（并把状态画进视图本身）。
- 活动栏页面在第一次打开时创建，并在主窗口存续期间一直保留；任务详情标签页按任务创建，由详情面板缓存。

### 3. 安装

和教程一一样安装这个文件夹（`manifest.json` + `ui/inspector.js`），批准对话框，然后打开某个任务的详情：内置标签页之后会多出一个 **Inspector** 标签页。设置视图和活动栏页面做法相同，只需把 `location` 改成 `settings`（每个插件至多一个）或 `activity`。

## `fluxdown` 模块

在桌面脚本里按裸模块名导入：`import { identity, taskId, call, invoke, theme, locale, toast } from "fluxdown"`。每个视图拿到的是绑定到自己插件和任务的一份。

| 函数 | 返回 | 说明 |
|---|---|---|
| `identity()` | `string` | 插件 identity，例如 `acme@release-checker`。 |
| `taskId()` | `string \| null` | `taskDetail` 视图的任务 id；其他位置为 `null`。 |
| `call(method, params?)` | `Promise<unknown>` | 调用本机 agent 的 RPC 方法。只允许以 `daemon.` 或 `agent.` 开头的方法。 |
| `invoke(name, args?)` | `Promise<unknown>` | 调用你的 `main` 用 `flux.rpc.handle(name, …)` 注册的处理函数（经 `daemon.plugin.invoke`）。 |
| `theme()` | `{ revision, tokens }` | FluxDown 当前完整已解析的主题。主题每变化一次 `revision` 就递增，并且视图会自动重绘。 |
| `locale()` | `string` | 界面语言，例如 `zh-CN`、`en`。 |
| `toast(level, text)` | `void` | 在当前活动窗口弹出通知。`level` 为 `info`、`success`、`warning` 或 `error`。 |
| `close()` | `boolean` | 关闭承载该视图的容器（`auth` 视图的登录对话框、设置对话框、独立窗口）并返回 `true`；关闭会推迟到当前调用返回之后执行。嵌入页面的视图（活动栏页、任务详情标签页）没有容器：什么也不做并返回 `false`。 |
| `navigate(target)` | `void` | 让主窗口到前台并切换页面。`target` 为 `"downloads"`、`"rss"`、`"webhooks"` 或 `{ view: "<id>" }`（本插件的某个 `activity` 视图）。同步执行：形状非法会抛出 `fluxdown.navigate: …`，目标不是本插件已贡献的活动栏视图时同样抛错；合法目标则推迟执行。 |

### 错误语义

| 情形 | 你会看到 |
|---|---|
| `call` 的方法不在 `daemon.*` / `agent.*` 内 | **在调用处同步抛出**：``fluxdown.call: `system.shutdown` is not allowed; only daemon.* and agent.* methods``。这次尝试会在审计日志里记为 `denied`。 |
| `call` / `invoke` 的 RPC 失败 | Promise 以序列化成 JSON 文本的错误详情 **reject**，例如 `{"code":"invalidArgument","retryable":false,"field":"name"}`。字段有 `code`、`retryable`，以及可选的 `field`、`reason`、`revision`。用 `JSON.parse` 解析 `error.message`（或该值本身）。 |
| `toast` 传入其他 level | 抛出 ``fluxdown.toast: unknown level `x`; expected info, success, warning or error``。 |
| 没有可用主题时调用 `theme()` | 抛出 `fluxdown.theme: theme is not available`。 |
| 在脚本调用之外（例如游离的回调里）调用 `toast` | 抛出 `fluxdown.toast: no active application scope`。 |

像教程里那样，用 `try` / `catch` 包住 `await call(...)` 和 `await invoke(...)`：同一个 `catch` 既能接住同步抛出，也能接住 reject。

### 调用 `main`（`invoke`）

`invoke(name, args)` ↔ `flux.rpc.handle(name, handler)` 是桌面脚本与 daemon 里常驻 `main` 脚本之间的桥：

- `args` 和处理函数的返回值必须可 JSON 序列化。处理函数返回 `undefined` 时结果是 `null`。
- 处理函数在 `main` 的虚拟机里执行，所以 `main` 能用的一切它都能用：`flux.tasks`、`flux.storage`、`flux.fs`、`flux.fetch`、`flux.ffmpeg` 等，取决于你声明的权限。没有窗口也要继续工作的逻辑，以及不该由桌面脚本自己做的事，都适合放在这里。
- `invoke` 需要有 `main` 入口。如果插件没有 `main`、已被禁用，或该名称没有注册，Promise 会以 `invalidArgument` 错误 reject（`rpc not registered: <name>` 这类信息不会转发）。处理函数抛出异常时，桌面一侧同样只能拿到错误码。界面需要提示信息时，请**把失败当作数据返回**——`{ ok: false, error: "…" }`。
- `invoke` 与 `main` 里的其他调用（视图会话、命令、定时器）串行执行，并受同样的预算限制（每次调用 2 秒 CPU、30 秒墙钟）。

## 样式与主题

- `cx.theme()` 返回基础主题。语义颜色是 `cx.theme().colors` 的 snake_case 字段：`background`、`foreground`、`surface`、`primary`、`primary_foreground`、`secondary`、`muted`、`muted_foreground`、`accent`、`destructive`、`border`、`input`、`ring` 等，可直接传给 `.bg(...)`、`.text_color(...)` 这类样式方法。
- `fluxdown.theme().tokens` 是按路径索引的完整 FluxDown 令牌表，例如 `"colors.primary"`、`"colors.success"`、`"colors.warning"`、`"colors.info"`、`"colors.statusDownloading"`、`"radius.md"`、`"spacing.lg"`、`"typography.sm.size"`、`"density.control"`。颜色是 `#rrggbbaa` 字符串，可以直接交给颜色样式方法；尺寸和圆角是数字。SDK 以 `ThemeTokenPath` 导出路径联合类型，以 `ThemeTokenValue` 导出取值类型。
- 用户切换主题时，宿主会递增 `theme().revision` 并重绘所有存活的脚本视图，所以在 `render` 里读取令牌就够了。
- FluxDown 始终保持自己的主题生效：脚本如果改写全局主题对象，FluxDown 会把自己的主题重新装回去，因此插件无法永久改变应用外观。
- 请用 FluxDown 的令牌而不是写死颜色，这样界面才会跟随浅色 / 深色和自定义主题。

## 桌面视图出现在哪里

| | 嵌入式（`activity`、`settings`、`taskDetail`、`auth`） | 窗口（`window`） |
|---|---|---|
| 容器 | 主窗口或设置对话框里的页面 / 标签页 | 独立、可调整大小、可最小化的窗口，带 FluxDown 标题栏 |
| 用户如何到达 | 活动栏条目 / 插件设置按钮 / 任务详情标签页 | 插件行上的窗口图标按钮（设置 → 扩展 → 插件） |
| `taskId()` | 对应的任务（仅 taskDetail） | `null` |
| `fluxdown.close()` | `auth`、`settings` 视图关闭所在对话框并返回 `true`；`activity` / `taskDetail` 返回 `false` | 关闭该窗口，返回 `true` |
| 浮层 API `window.push_toast`、`window.open_dialog`、`window.open_sheet` | **不可用**——请用 `fluxdown.toast` | 可用 |
| `width` / `height` | manifest 会拒绝 | 初始尺寸，240–4096（默认 960 × 640）；每个方向的最小尺寸取声明值与 240 中较小者 |

窗口：每个视图一个窗口（再次点击会聚焦它），标题为 `<插件名>: <视图标题>`，居中打开。窗口脚本里打开对话框的写法：

```js
import { div } from "gpui-kit";
// 在窗口脚本的事件处理函数里：
window.open_dialog(() => div().child("按 Esc 关闭"));
```

## 网络访问

脚本的网络访问被限制在 `desktop.networkHosts` 列出的主机内：

- 条目是小写 DNS 主机名（RFC 1123 标签，最长 253 个字符）或 IP 字面量；不能带 scheme、端口、路径或通配符。每个视图至多 64 项，不能重复。
- 全局 `fetch`、`net`、`websocket` 模块只能访问这些完全匹配的主机；每次重定向的目标也会检查。未列出的主机会被拒绝——调用会 reject。
- `fetch` 超时为 30 秒，每个响应最多缓冲 8 MiB。
- 主机列表会显示在授权对话框里。更新新增主机后，会就新的集合再次征求用户同意。
- `fluxdown.call` 不受影响：它访问的是本机 agent，限制的是只能调用 `daemon.*` / `agent.*`。

## 权限与信任

桌面脚本在桌面端进程内、以用户的权限运行：可以读写用户能访问的任何文件、运行任何程序（通过 `process` 模块）、使用剪贴板和 `localStorage`，并访问声明过的网络主机。它不能调用 `process.exit`（从不授予）。这些原生操作都不会被逐条审计；审计日志只记录脚本被加载（`desktop.load`，附带它运行时所拥有的授权，例如 `fs:all process:all network:api.github.com clipboard:rw`）、被卸载（`desktop.unload`），以及它发起的每次 `fluxdown.call` / `fluxdown.invoke`。详见[权限与审计](/docs/zh/plugins/permissions/)。

写桌面脚本时，请当作会有人审阅它：`networkHosts` 尽量少、不碰不需要的文件，并优先把特权操作交给 `main`——`main` 的能力只限于你声明的权限。

## 与可移植视图并存

一个视图可以同时有两种实现：

```json
{
  "id": "inspector",
  "title": "Inspector",
  "location": "taskDetail",
  "portable": true,
  "desktop": { "entry": "ui/inspector.js" }
}
```

`portable: true` 表示 `main` 里也注册了 `flux.ui.view("inspector", …)`，因此必须声明 `main`。规则如下：

| 客户端 / 情形 | 显示什么 |
|---|---|
| 桌面端，脚本文件在本机可读 | 桌面脚本 |
| 桌面端，daemon 在另一台机器上（包路径在本机不可读） | `portable: true` 时显示可移植视图；否则显示提示「该视图仅在桌面端可用」 |
| Web 端 | `portable: true` 时显示可移植视图；否则什么都不显示（入口被隐藏） |
| 移动端 | 什么都不显示（不渲染插件视图） |

反方向没有回退：如果选中了桌面脚本，但它加载失败或挂载时抛出异常，视图显示的是错误面板（「插件界面加载失败」及原因），而不是可移植视图。`window` 视图必须有 `desktop` 入口；只有可移植实现的 `window` 视图永远不会显示。

## 运行时、存储与数据目录

- **每个插件一个运行时。** 桌面端在加载某插件的第一个桌面视图时为它创建独立的 gpui-shell 运行时，最后一个视图关闭时丢弃。不要指望模块级变量在这之后还在；需要持久的状态请放进 `localStorage`、文件或 `main`。
- **数据目录。** 每个插件在桌面端数据目录下有自己的目录 `plugin-ui/<编码后的 identity>/`：字母、数字和 `-` 原样保留，其余字节都写成小写十六进制的 `_xx`（`acme@release-checker` → `acme_40release-checker`）。`localStorage` 持久化到该目录下的 `store.json`。它与 daemon 端供 `flux.fs` 使用的工作区是分开的。
- **模块。** 只能按裸模块名导入内置模块：`gpui-kit`（别名 `gpui`）、`gpui-base`、`gpui-component`、`gpui-shell`、`gpui-fps`、`fluxdown`、`buffer`、`console`、`crypto`、`fs/promises`、`net`、`os`、`path`、`process`、`url`、`websocket`、`zlib`，另有全局的 `fetch`、`TextEncoder`、`TextDecoder`、`localStorage` 和 `window`。运行时没有 npm 解析：请把第三方代码打包进入口文件（SDK 会做这件事）。你自己的文件之间的相对导入按导入方文件所在位置解析。
- **没有后台执行。** 脚本只在视图挂载期间运行。没有窗口也要发生的事情，放进 `main`。

## 用 SDK 构建

可以像上面那样手写脚本，也可以用 [`fluxdown-plugin-sdk`](/docs/zh/plugins/sdk/) 写 TypeScript。在 `fluxdown-plugin.config.json` 的 `desktop` 下列出桌面入口：键是 `dist/` 里的输出路径，值是源文件：

```json
{
  "entries": { "main.js": "src/main.ts" },
  "desktop": { "ui/release.js": "src/desktop/release.ts" }
}
```

`desktop` 入口会被打包成 **ES 模块**（`entries` 是 IIFE），宿主模块保留为裸导入。要在编辑器里获得类型，添加 `src/env.d.ts`：

```ts
/// <reference types="fluxdown-plugin-sdk" />
/// <reference types="fluxdown-plugin-sdk/types/desktop" />
```

`fluxdown-plugin-sdk/types/desktop` 声明了上面所有模块（`gpui-kit`、`gpui-base`、`gpui-component`、`fluxdown` 等）和全局对象。带类型的 `ui/release.ts` 大致如下：

```ts
import { View } from "gpui-kit";
import type { AsyncContext, Context } from "gpui-kit";
import { v_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { invoke, toast, theme } from "fluxdown";

export default class ReleaseWindow extends View {
  private cx!: AsyncContext;
  private tag: string | null = null;

  init(_props: unknown, cx: AsyncContext): void {
    this.cx = cx;
  }

  render(cx: Context) {
    const colors = cx.theme().colors;
    return v_flex()
      .gap(8)
      .p(16)
      .bg(colors.background)
      .child(Label.new(`主题 revision ${theme().revision}；上次 tag：${this.tag ?? "-"}`))
      .child(
        Button.new("ping").label("询问 main").on_click(() => {
          this.cx.spawn(async (cx) => {
            const reply = (await invoke("lastSeen")) as { tag: string | null };
            this.tag = reply.tag;
            toast("info", "main 已回复");
            cx.notify();
          });
        }),
      );
  }
}
```

然后：

```bash
npm run build     # 把 main.js 和 ui/release.js 打包进 dist/ 并校验
npm run pack      # 生成 acme@release-checker-1.0.0.fxplug
```

校验器会检查每个 `desktop.entry` 都存在于包中、扩展名为 `.js` 或 `.mjs`、已声明 `desktop` 权限，以及上面的 `networkHosts` / 尺寸规则。发布流程见[打包与发布](/docs/zh/plugins/packaging/)。

## 开发循环

1. 用**从目录安装**把项目（或它的 `dist/`）作为开发模式插件安装。
2. 运行 `npm run dev`，每次保存都会重建 `dist/`。
3. FluxDown **不会**监视桌面脚本。脚本在视图创建时读取，所以要运行新代码，请关闭并重新打开 `window` 视图。已创建的嵌入式页面或标签页会继续使用它已加载的代码；请关闭并重新打开主窗口（或重启桌面端）来加载新脚本。
4. 修改 `manifest.json`（视图、`networkHosts`、权限、`main`）之后，在插件行点**重新加载**（它会重读 manifest 和全部脚本）。新增权限或网络主机的改动需要重新批准授权对话框。
5. 修改 `main.js` 同样需要**重新加载**：daemon 会丢弃旧的 `main` 宿主（打开的可移植会话会结束，可以重新打开），之后的 `invoke` 就会走到新代码。

想确认实际运行了什么，可以看 **设置 → 扩展 → 插件 → 审计日志**：`desktop.load` 记录显示授权内容和加载结果，`desktop.call` / `desktop.invoke` 记录显示每次桥接调用，包括被 `denied` 的。

## 排错

| 现象 | 可能原因与处理 |
|---|---|
| 插件行上没有窗口图标 | 插件已禁用、仍在等待授权或加载失败；视图的 `location` 不是 `window`；或视图没有 `desktop` 入口。修正 manifest 后点**重新加载**。 |
| 面板或窗口显示「插件界面加载失败」 | 标题下面的文字就是原因：``plugin `<identity>` interface failed to load: …``。常见原因：脚本有语法错误；缺少默认导出，或默认导出没有继承 `View`；导入了不是内置模块的东西（没有打包进来的 npm 包或 `node:` 说明符）；包里缺少该文件。同样的信息也会写进桌面端日志（应用菜单里的「打开日志目录」）。 |
| 面板显示「该视图仅在桌面端可用」 | 桌面脚本文件在本机不可读（例如桌面端连接的是远端 daemon），且视图不是可移植的。请加上 `portable: true` 并在 `main` 里实现，或在 daemon 所在的机器上使用该插件。 |
| 连接远端 daemon 时，窗口打开后显示加载失败面板 | 原因相同：包目录在 daemon 所在的机器上。窗口没有可移植回退。 |
| 预期能通的主机 `fetch` 被 reject | 该主机（以及每个重定向目标）必须精确地、以小写、不带 scheme / 端口 / 通配符的形式列在 `desktop.networkHosts` 里。改完 manifest 后点**重新加载**并批准新增的主机。 |
| ``fluxdown.call: `x` is not allowed; only daemon.* and agent.* methods`` | 你调用了 `daemon.*` / `agent.*` 之外的方法。 |
| `invoke` 以 `{"code":"invalidArgument",…}` reject | 插件没有 `main`、`main` 没有注册该名称、插件已被禁用，或处理函数抛出了异常。需要携带信息时请把失败当作数据返回。 |
| 面板里调用 `window.open_dialog` / `window.push_toast` 失败 | 嵌入式视图没有浮层宿主，请用 `fluxdown.toast`。 |
| 修改脚本后看不到效果 | 脚本不会热重载；请重新打开窗口，嵌入式视图则重新打开主窗口（见[开发循环](#开发循环)）。 |
| 改了状态界面却不更新 | 在 `render` 之外修改状态后要调用 `cx.notify()`，包括 `cx.spawn` 任务的末尾。 |
| 定时器或 `require` 调用抛出桩（stub）错误 | 这里没有浏览器定时器和 `require`，错误信息会指出替代方案。计时用 `cx.timer.after(ms, fn)`、`cx.timer.every(ms, fn)` 或 `await cx.sleep(ms)`，模块用 `import`。 |
| manifest 被拒：`view 'x' 声明 desktop 时 permissions 必须包含 desktop` | 在 `permissions` 里加上 `"desktop"`。 |
| manifest 被拒：`view 'x' 的 width 仅允许用于 location 为 window 的 view` | 删掉 `width` / `height`，或把 `location` 改成 `window`。 |
| manifest 被拒：`view 'x' 有可移植实现，必须声明 main` | 添加 `main` 入口，或去掉 `portable: true`。 |
| manifest 被拒：`view 'x' 的 portable 仅允许与 desktop 同时声明` | 去掉 `portable`，或添加 `desktop` 入口。 |

## 下一步

- [视图参考](/docs/zh/plugins/views/)——可移植节点集、`main` 生命周期与客户端支持矩阵。
- [用可移植视图构建界面](/docs/zh/plugins/ui-guide/)——跨端的替代方案。
- [权限与审计](/docs/zh/plugins/permissions/)——授权、风险等级与审计内容。
- [功能演示插件](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo)里有窗口脚本（`ui/window.js`，含 `window.push_toast`、对话框和文件访问）和嵌入式面板（`ui/panel.js`，覆盖 `fluxdown` 的每个函数）。
