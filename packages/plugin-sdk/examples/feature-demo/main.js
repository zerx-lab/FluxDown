// 功能演示插件的常驻宿主脚本（manifest `main`，background:true，daemon 内常驻）。
// 纯 classic script，无需构建；`flux` 由宿主注入。
const { h } = flux.ui;

const PIXEL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
const SAMPLE_URL = "https://proof.ovh.net/files/1Mb.dat";
const RESOLVER_URL = "https://fluxdown-demo.invalid/file";

// ---------------------------------------------------------------------------
// 宿主级共享状态（跨视图会话）
// ---------------------------------------------------------------------------
const shared = {
  ticks: 0,
  events: [], // 最近 20 条下载事件
  commands: [], // 最近 10 次命令调用
  rpcCalls: 0,
};
const openViews = new Set(); // 已打开会话的 ctx，用于定时器 / 事件后刷新

function pushLimited(list, item, limit) {
  list.unshift(item);
  if (list.length > limit) list.length = limit;
}
function now() {
  return Date.now();
}
function refreshAll() {
  for (const ctx of openViews) ctx.refresh();
}

// 后台定时器：没有打开的视图时会被钳制到 ≥60 s，有视图时 ≥1 s。
let tickTimer = null;
function restartTicker() {
  if (tickTimer !== null) flux.timers.clear(tickTimer);
  const seconds = Number(flux.settings.tickSeconds) || 2;
  tickTimer = flux.timers.setInterval(() => {
    shared.ticks += 1;
    refreshAll();
  }, seconds * 1000);
}
restartTicker();

// ---------------------------------------------------------------------------
// 下载事件（main.events）
// ---------------------------------------------------------------------------
for (const name of ["onStart", "onDone", "onError", "onCancel", "onMetaProbed"]) {
  flux.events.on(name, (ev) => {
    pushLimited(shared.events, { key: `${now()}-${name}`, at: now(), name, ev }, 20);
    if (name === "onDone" && flux.settings.toastOnDone) {
      flux.ui.toast("success", `演示插件：任务完成 ${ev.taskId}`);
    }
    if (name === "onError") flux.ui.toast("error", `演示插件：任务失败 ${ev.message || ""}`);
  });
}

// ---------------------------------------------------------------------------
// 命令（命令面板 / 任务右键）
// ---------------------------------------------------------------------------
flux.commands.register("hello", async ({ taskIds }) => {
  pushLimited(shared.commands, { at: now(), id: "hello", taskIds }, 10);
  flux.ui.toast("info", `${flux.settings.greeting}！选中了 ${taskIds.length} 个任务`);
  refreshAll();
});

flux.commands.register("toggle-selected", async ({ taskIds }) => {
  pushLimited(shared.commands, { at: now(), id: "toggle-selected", taskIds }, 10);
  if (taskIds.length === 0) {
    flux.ui.toast("warning", "没有选中任务");
    return;
  }
  for (const id of taskIds) {
    const task = await flux.tasks.get(id);
    // status 1 = 下载中
    if (task.status === 1) await flux.tasks.pause(id);
    else await flux.tasks.resume(id);
  }
  flux.ui.toast("success", `已切换 ${taskIds.length} 个任务`);
  refreshAll();
});

flux.commands.register("inspect", async ({ taskIds }) => {
  pushLimited(shared.commands, { at: now(), id: "inspect", taskIds }, 10);
  for (const id of taskIds) {
    const t = await flux.tasks.get(id);
    flux.ui.toast("info", `${t.fileName || t.url}：${t.downloadedBytes}/${t.totalBytes} B，状态 ${t.status}`);
  }
  refreshAll();
});

// ---------------------------------------------------------------------------
// RPC（桌面脚本经 fluxdown.invoke 调用）
// ---------------------------------------------------------------------------
flux.rpc.handle("stats", async () => {
  shared.rpcCalls += 1;
  const tasks = await flux.tasks.list();
  refreshAll();
  return {
    ticks: shared.ticks,
    tasks: tasks.length,
    rpcCalls: shared.rpcCalls,
    events: shared.events.length,
    greeting: flux.settings.greeting,
  };
});

flux.rpc.handle("echo", (args) => ({ youSent: args, at: now() }));

// 页面脚本（`page/hello.js`）经 `fluxdown.invoke("pageHello")` 调用；必须显式 `page: true` 才对网页暴露。
// ctx.caller 为 "page"，ctx.pageUrl 是调用方页面地址（来自网页环境，按不可信输入处理）。
flux.rpc.handle(
  "pageHello",
  (args, ctx) => ({ greeting: "hello from feature-demo", echo: args, caller: ctx && ctx.caller, at: now() }),
  { page: true },
);

// ---------------------------------------------------------------------------
// 视图：演示面板（活动栏）
// ---------------------------------------------------------------------------
// TaskDto.status: 0=pending 1=downloading 2=paused 3=completed 4=error 5=preparing.
function statusBadge(status) {
  const map = {
    0: ["排队", "muted"],
    1: ["下载中", "accent"],
    2: ["已暂停", "warning"],
    3: ["已完成", "success"],
    4: ["失败", "danger"],
    5: ["准备中", "accent"],
  };
  const [text, tone] = map[status] || [`状态 ${status}`, "neutral"];
  return { text, tone };
}

async function loadTasks(state) {
  try {
    const tasks = await flux.tasks.list();
    state.tasks = tasks.slice(0, 200);
    state.taskError = "";
  } catch (e) {
    state.taskError = String(e && e.message ? e.message : e);
  }
}

function controlsTab(state) {
  return h(
    "stack",
    { gap: "md" },
    h(
      "card",
      { title: "按钮与通知" },
      h(
        "stack",
        { direction: "row", gap: "sm", wrap: true },
        h("button", { key: "inc", text: `计数 +1（${state.count}）`, variant: "primary" }),
        h("button", { key: "reset", text: "归零", variant: "outline" }),
        h("button", { key: "ghost", text: "Ghost", variant: "ghost" }),
        h("button", { key: "danger", text: "Danger", variant: "danger" }),
        h("button", { key: "disabled", text: "禁用", disabled: true }),
        h("button", { key: "busy", text: "加载中", loading: true }),
      ),
      h(
        "stack",
        { direction: "row", gap: "sm", wrap: true },
        h("button", { key: "toast-info", text: "info 通知" }),
        h("button", { key: "toast-success", text: "success 通知" }),
        h("button", { key: "toast-warning", text: "warning 通知" }),
        h("button", { key: "toast-error", text: "error 通知" }),
      ),
    ),
    h(
      "card",
      { title: "表单控件（输入有 300 ms 去抖，聚焦时不被回写覆盖）" },
      h("input", { key: "name", value: state.name, placeholder: "输入文字，回车触发 submit" }),
      h("input", { key: "secret", value: state.secret, placeholder: "密码框", kind: "password" }),
      h("input", { key: "num", value: state.num, placeholder: "数字框", kind: "number" }),
      h("textarea", { key: "notes", value: state.notes, placeholder: "多行文本", rows: 3 }),
      h("switch", { key: "sw", label: "开关", checked: state.sw }),
      h("checkbox", { key: "cb", label: "复选框", checked: state.cb }),
      h("select", {
        key: "sel",
        value: state.sel,
        placeholder: "选择一项",
        options: [
          { value: "a", label: "选项 A" },
          { value: "b", label: "选项 B" },
          { value: "c", label: "选项 C" },
        ],
      }),
      h("kv", {
        items: [
          { key: "输入", value: state.name || "（空）" },
          { key: "最近 submit", value: state.submitted || "（无）" },
          { key: "数字", value: state.num || "（空）" },
          { key: "多行字数", value: String(state.notes.length) },
          { key: "开关 / 复选", value: `${state.sw} / ${state.cb}` },
          { key: "下拉", value: state.sel || "（未选）" },
        ],
      }),
    ),
  );
}

function displayTab(state) {
  const fraction = (shared.ticks % 10) / 10;
  return h(
    "stack",
    { gap: "md" },
    h("heading", { text: "一级标题", level: 1 }),
    h("heading", { text: "二级标题", level: 2 }),
    h("heading", { text: "三级标题", level: 3 }),
    h(
      "stack",
      { direction: "row", gap: "sm", wrap: true, align: "center" },
      ...["neutral", "accent", "success", "warning", "danger", "info", "muted"].map((tone) =>
        h("badge", { key: `b-${tone}`, text: tone, tone }),
      ),
    ),
    h(
      "stack",
      { gap: "xs" },
      h("text", { text: "xs 文本", size: "xs" }),
      h("text", { text: "lg 半粗 accent", size: "lg", weight: "semibold", tone: "accent" }),
      h("text", { text: "等宽 mono 0123456789", mono: true }),
      h("text", { text: "很长很长的文本会被截断。".repeat(20), truncate: true }),
    ),
    h("divider"),
    h("progress", { value: fraction, label: `定时器驱动进度 ${Math.round(fraction * 100)}%` }),
    h("progress", { value: null, label: "不确定进度" }),
    h("spinner", { label: "加载指示" }),
    h("callout", { tone: "info", title: "信息", text: "callout info" }),
    h("callout", { tone: "warning", title: "警告", text: "callout warning" }),
    h("callout", { tone: "danger", text: "callout danger（无标题）" }),
    h("callout", { tone: "success", text: "callout success" }),
    h("markdown", {
      text: "这是一段 **markdown**。\n\n第二段落。\n\n```js\nconsole.log('代码块');\n```",
    }),
    h("code", { text: "const answer = 42;" }),
    h("link", { text: "打开 FluxDown 官网", href: "https://fluxdown.com" }),
    h("image", { src: PIXEL, alt: "1×1 像素图", width: 48, height: 48 }),
    h("empty", { title: "空状态", description: "empty 节点示例" }),
    h(
      "section",
      { title: "section 分区", description: "带描述的分区" },
      h("scroll", { maxHeight: 80 }, ...Array.from({ length: 12 }, (_, i) => h("text", { key: `s${i}`, text: `滚动行 ${i + 1}` }))),
    ),
  );
}

function tasksTab(state) {
  const rows = state.tasks.map((t) => ({
    key: t.taskId,
    name: t.fileName || t.url,
    size: t.totalBytes,
    done: t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : 0,
    pct: t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : 0,
    status: statusBadge(t.status),
    // createdAt 是 Unix 秒（字符串）；`time` 列要求毫秒。
    created: Number(t.createdAt) * 1000 || 0,
  }));
  return h(
    "stack",
    { gap: "md" },
    state.taskError ? h("callout", { tone: "danger", title: "任务读取失败", text: state.taskError }) : null,
    h(
      "stack",
      { direction: "row", gap: "sm", wrap: true },
      h("button", { key: "reload", text: "刷新任务", variant: "outline" }),
      h("button", { key: "create", text: "新建示例任务（1 MB）", variant: "primary" }),
      h("button", { key: "create-resolver", text: "新建 resolver 改写任务" }),
      h("button", { key: "create-burst", text: "连续新建 21 次（测限流）", variant: "danger" }),
    ),
    state.lastAction ? h("text", { text: state.lastAction, tone: "muted", size: "xs" }) : null,
    h("table", {
      key: "tasks",
      columns: [
        { key: "name", title: "名称" },
        { key: "size", title: "大小", kind: "bytes", width: 90 },
        { key: "pct", title: "百分比", kind: "percent", width: 80 },
        { key: "done", title: "进度", kind: "progress", width: 120 },
        { key: "status", title: "状态", kind: "badge", width: 90 },
        { key: "created", title: "创建时间", kind: "time", width: 160 },
      ],
      rows,
      rowActions: [
        { id: "pause", label: "暂停" },
        { id: "resume", label: "继续", tone: "success" },
        { id: "remove", label: "删除", tone: "danger" },
      ],
      emptyText: "暂无任务",
    }),
    state.selectedTask ? h("text", { text: `表格选中：${state.selectedTask}`, size: "xs" }) : null,
  );
}

function eventsTab() {
  return h(
    "stack",
    { gap: "md" },
    h("text", { text: "下载任务触发的事件会出现在这里（开始 / 完成 / 失败 / 取消 / 元信息）。", tone: "muted" }),
    shared.events.length === 0
      ? h("empty", { title: "还没有事件", description: "新建一个任务试试" })
      : h("list", {
          key: "events",
          items: shared.events.map((e) => ({
            key: e.key,
            title: `${e.name} · ${e.ev.taskId || ""}`,
            description: new Date(e.at).toLocaleTimeString() + " " + (e.ev.url || ""),
            badge: e.name,
            tone: e.name === "onError" ? "danger" : e.name === "onDone" ? "success" : "info",
          })),
        }),
    h("heading", { text: "最近命令", level: 3 }),
    h("kv", {
      items: shared.commands.length
        ? shared.commands.map((c) => ({ key: new Date(c.at).toLocaleTimeString(), value: `${c.id} [${c.taskIds.join(", ")}]` }))
        : [{ key: "—", value: "从命令面板（⌘K）或任务右键执行演示命令" }],
    }),
  );
}

function bridgeTab(state) {
  return h(
    "stack",
    { gap: "md" },
    h(
      "stack",
      { direction: "row", gap: "sm", wrap: true },
      h("button", { key: "fetch", text: "flux.fetch httpbin" }),
      h("button", { key: "storage", text: "storage 读写" }),
      h("button", { key: "fs", text: "fs 读写" }),
      h("button", { key: "settings-update", text: "settings.update 改问候语" }),
      h("button", { key: "timeout", text: "setTimeout 3 秒后通知" }),
      h("button", { key: "throw", text: "render 抛错（测错误条）", variant: "danger" }),
    ),
    h("code", { text: state.bridgeOutput || "点击上方按钮，结果显示在这里" }),
    h("kv", {
      items: [
        { key: "flux.info", value: JSON.stringify(flux.info) },
        { key: "settings", value: JSON.stringify(flux.settings) },
        { key: "rpc 调用次数", value: String(shared.rpcCalls) },
      ],
    }),
  );
}

flux.ui.view("dashboard", {
  async init(ctx) {
    openViews.add(ctx);
    const state = {
      tab: "controls",
      count: 0,
      name: "",
      secret: "",
      num: "",
      notes: "",
      submitted: "",
      sw: false,
      cb: true,
      sel: "",
      tasks: [],
      taskError: "",
      selectedTask: "",
      lastAction: "",
      bridgeOutput: "",
      boom: false,
      session: ctx.session,
    };
    await loadTasks(state);
    return state;
  },
  render(state, ctx) {
    if (state.boom) {
      state.boom = false;
      throw new Error("演示：render 主动抛错，下一次渲染会自动恢复");
    }
    const body =
      state.tab === "controls"
        ? controlsTab(state)
        : state.tab === "display"
          ? displayTab(state)
          : state.tab === "tasks"
            ? tasksTab(state)
            : state.tab === "events"
              ? eventsTab()
              : bridgeTab(state);
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h(
        "stack",
        { direction: "row", justify: "between", align: "center" },
        h("heading", { text: `${flux.settings.greeting}，这里是演示面板`, level: 2 }),
        h("badge", { text: `tick ${shared.ticks}`, tone: "accent" }),
      ),
      h("text", { text: `会话 ${ctx.session} · 语言 ${ctx.locale}`, tone: "muted", size: "xs" }),
      h("tabs", {
        key: "tabs",
        value: state.tab,
        items: [
          { value: "controls", label: "控件" },
          { value: "display", label: "展示" },
          { value: "tasks", label: "任务" },
          { value: "events", label: "事件" },
          { value: "bridge", label: "宿主能力" },
        ],
      }),
      body,
    );
  },
  async event(state, ev, ctx) {
    const id = ev.node.split("/").pop();
    switch (`${ev.type}:${id}`) {
      case "change:tabs":
        state.tab = ev.value;
        if (state.tab === "tasks") await loadTasks(state);
        return;
      case "press:inc":
        state.count += 1;
        return;
      case "press:reset":
        state.count = 0;
        return;
      case "press:toast-info":
      case "press:toast-success":
      case "press:toast-warning":
      case "press:toast-error": {
        const level = id.slice("toast-".length);
        flux.ui.toast(level, `这是一条 ${level} 通知`);
        return;
      }
      case "change:name":
        state.name = ev.value;
        return;
      case "submit:name":
        state.submitted = ev.value;
        flux.ui.toast("info", `submit：${ev.value}`);
        return;
      case "change:secret":
        state.secret = ev.value;
        return;
      case "change:num":
        state.num = ev.value;
        return;
      case "change:notes":
        state.notes = ev.value;
        return;
      case "change:sw":
        state.sw = ev.value;
        return;
      case "change:cb":
        state.cb = ev.value;
        return;
      case "change:sel":
        state.sel = ev.value;
        return;
      case "press:reload":
        await loadTasks(state);
        state.lastAction = `已刷新：${state.tasks.length} 个任务`;
        return;
      case "press:create": {
        const r = await flux.tasks.create({ request: { url: SAMPLE_URL } });
        state.lastAction = `已新建：${JSON.stringify(r)}`;
        await loadTasks(state);
        return;
      }
      case "press:create-resolver": {
        const r = await flux.tasks.create({ request: { url: RESOLVER_URL } });
        state.lastAction = `resolver 任务：${JSON.stringify(r)}（应被改写为 ${SAMPLE_URL}）`;
        await loadTasks(state);
        return;
      }
      case "press:create-burst": {
        let ok = 0;
        let limited = "";
        for (let i = 0; i < 21; i++) {
          try {
            await flux.tasks.create({ request: { url: `${SAMPLE_URL}?burst=${i}`, startPaused: true } });
            ok += 1;
          } catch (e) {
            limited = String(e && e.message ? e.message : e);
            break;
          }
        }
        state.lastAction = `成功 ${ok} 次；第 ${ok + 1} 次：${limited || "未被限流"}（已暂停，可在下载页删除）`;
        await loadTasks(state);
        return;
      }
      case "rowAction:tasks": {
        if (ev.action === "pause") await flux.tasks.pause(ev.key);
        if (ev.action === "resume") await flux.tasks.resume(ev.key);
        if (ev.action === "remove") await flux.tasks.remove(ev.key, true);
        state.lastAction = `${ev.action} → ${ev.key}`;
        await loadTasks(state);
        return;
      }
      case "select:tasks":
        state.selectedTask = ev.key;
        return;
      case "select:events":
        flux.ui.toast("info", `选中事件 ${ev.key}`);
        return;
      case "press:fetch": {
        const r = await flux.fetch({ url: "https://httpbin.org/get?demo=1&secret=hidden-in-audit" });
        state.bridgeOutput = `HTTP ${r.status}\n${r.body.slice(0, 600)}`;
        return;
      }
      case "press:storage": {
        const prev = await flux.storage.get("counter");
        const next = String((Number(prev) || 0) + 1);
        await flux.storage.set("counter", next);
        state.bridgeOutput = `storage counter: ${prev} → ${next}`;
        return;
      }
      case "press:fs": {
        await flux.fs.writeFile("demo.txt", `写入于 ${new Date().toISOString()}`);
        const content = await flux.fs.readFile("demo.txt");
        const files = await flux.fs.list();
        state.bridgeOutput = `readFile: ${content}\nlist: ${JSON.stringify(files)}`;
        return;
      }
      case "press:settings-update": {
        const next = flux.settings.greeting === "你好" ? "Hello" : "你好";
        await flux.settings.update({ greeting: next });
        state.bridgeOutput = `greeting 已改为 ${next}（到设置页确认已持久化）`;
        return;
      }
      case "press:timeout":
        flux.timers.setTimeout(() => flux.ui.toast("success", "3 秒定时器触发"), 3000);
        state.bridgeOutput = "已设置 3 秒定时器";
        return;
      case "press:throw":
        state.boom = true;
        ctx.refresh();
        return;
      default:
        return;
    }
  },
  dispose(state) {
    forgetSession(state.session);
  },
});

// 会话关闭后从刷新集合移除（按 session 匹配）。
function forgetSession(session) {
  for (const ctx of openViews) if (ctx.session === session) openViews.delete(ctx);
}

// ---------------------------------------------------------------------------
// 视图：设置页（扩展 → 插件 → 设置 → 「插件设置」tab）
// ---------------------------------------------------------------------------
flux.ui.view("settings", {
  async init(ctx) {
    openViews.add(ctx);
    const state = {
      greeting: String(flux.settings.greeting),
      saved: "",
      archiveRules: null,
      archiveRulesError: "",
      session: ctx.session,
    };
    try {
      state.archiveRules = await flux.archiveRules.status();
    } catch (error) {
      state.archiveRulesError = String(error);
    }
    return state;
  },
  render(state) {
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h("callout", {
        tone: "info",
        title: "自定义设置视图",
        text: "这里由插件自己渲染；「基础设置」tab 是 manifest.settings 自动生成的表单。",
      }),
      h("text", {
        text: state.archiveRules
          ? `分类规则 v${state.archiveRules.sequence} · ${state.archiveRules.ruleCount} 条`
          : state.archiveRulesError || "分类规则状态不可用",
        tone: state.archiveRulesError ? "danger" : "muted",
      }),
      h(
        "stack",
        { direction: "row", gap: "sm" },
        h("button", { key: "rules-status", text: "读取分类规则状态", variant: "outline" }),
        h("button", { key: "rules-update", text: "手动更新分类规则", variant: "outline" }),
      ),
      h("input", { key: "greeting", value: state.greeting, placeholder: "问候语" }),
      h(
        "stack",
        { direction: "row", gap: "sm" },
        h("button", { key: "save", text: "保存问候语", variant: "primary" }),
        h("button", { key: "mode", text: `切换模式（当前 ${flux.settings.mode}）`, variant: "outline" }),
      ),
      state.saved ? h("text", { text: state.saved, tone: "success" }) : null,
      h("kv", {
        items: Object.entries(flux.settings).map(([key, value]) => ({ key, value: String(value) })),
      }),
    );
  },
  async event(state, ev) {
    const id = ev.node.split("/").pop();
    if (ev.type === "change" && id === "greeting") state.greeting = ev.value;
    if (ev.type === "press" && (id === "rules-status" || id === "rules-update")) {
      state.archiveRulesError = "";
      try {
        state.archiveRules =
          id === "rules-update"
            ? await flux.archiveRules.update()
            : await flux.archiveRules.status();
      } catch (error) {
        state.archiveRulesError = String(error);
      }
    }
    if (ev.type === "submit" && id === "greeting") state.greeting = ev.value;
    if (ev.type === "press" && id === "save") {
      await flux.settings.update({ greeting: state.greeting });
      state.saved = `已保存：${state.greeting}`;
    }
    if (ev.type === "press" && id === "mode") {
      await flux.settings.update({ mode: flux.settings.mode === "calm" ? "loud" : "calm" });
      state.saved = "模式已切换";
    }
  },
  dispose(state) {
    forgetSession(state.session);
  },
});

// ---------------------------------------------------------------------------
// 视图：任务详情 tab（ctx.taskId 为当前任务）
// ---------------------------------------------------------------------------
flux.ui.view("task-info", {
  async init(ctx) {
    openViews.add(ctx);
    const state = { task: null, error: "", session: ctx.session };
    await reloadTask(state, ctx);
    return state;
  },
  async render(state, ctx) {
    // 每次刷新（定时器驱动）重新拉取，展示实时进度。
    await reloadTask(state, ctx);
    if (state.error) return h("callout", { tone: "danger", title: "读取失败", text: state.error });
    const t = state.task;
    if (!t) return h("empty", { title: "没有任务", description: `taskId = ${ctx.taskId}` });
    const fraction = t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : null;
    return h(
      "stack",
      { gap: "sm", padding: "md" },
      h("heading", { text: t.fileName || "(未命名)", level: 3 }),
      h("badge", statusBadge(t.status)),
      h("progress", { value: fraction, label: fraction === null ? "大小未知" : `${Math.round(fraction * 100)}%` }),
      h("kv", {
        items: [
          { key: "taskId", value: t.taskId },
          { key: "URL", value: t.url },
          { key: "保存目录", value: t.saveDir },
          { key: "已下载", value: `${t.downloadedBytes} / ${t.totalBytes} B` },
          { key: "错误", value: t.errorMessage || "—" },
        ],
      }),
      h(
        "stack",
        { direction: "row", gap: "sm" },
        h("button", { key: "pause", text: "暂停" }),
        h("button", { key: "resume", text: "继续", variant: "primary" }),
      ),
    );
  },
  async event(state, ev, ctx) {
    const id = ev.node.split("/").pop();
    if (ev.type !== "press" || !ctx.taskId) return;
    if (id === "pause") await flux.tasks.pause(ctx.taskId);
    if (id === "resume") await flux.tasks.resume(ctx.taskId);
  },
  dispose(state) {
    forgetSession(state.session);
  },
});

async function reloadTask(state, ctx) {
  if (!ctx.taskId) return;
  try {
    state.task = await flux.tasks.get(ctx.taskId);
    state.error = "";
  } catch (e) {
    state.error = String(e && e.message ? e.message : e);
  }
}
