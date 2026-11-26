---
title: 认证指南
description: 让用户在插件里登录站点：authenticate 入口、二维码与账号密码登录，以及在 resolver 中复用登录凭据。
section: plugins
order: 5
sourceHash: "34ecd8c7845d"
---

很多站点只对已登录用户提供文件。**认证插件**负责教会 FluxDown 如何登录这样的站点：用户在插件所在行里登录一次，FluxDown 保存得到的凭据，之后自动附加到该插件的后续请求上，你的 resolver 完全不必接触密码，也不必反复登录。

本文讲解相关概念、`authenticate` 入口的五个 action、一个完整的二维码登录、账号密码与 Cookie 两种变体，以及 resolver 如何使用已保存的凭据。`flux.auth` 和 `flux.fetch` 的字段级参考见 [API 参考](/docs/zh/plugins/api-reference/)，manifest 字段见 [Manifest 参考](/docs/zh/plugins/manifest/)。

## 基本概念

- **凭据库。** 凭据保存在 FluxDown 自己的存储里，而不是插件的 `flux.storage`。持久化和过期判断由宿主统一实现，插件不必各写一套。凭据归保存它的插件所有，其他插件无法读取或使用；插件被卸载时凭据也会一并删除。
- **站点（site）。** 一份凭据保存在一个*站点*之下：`scheme://host[:port]`，例如 `https://www.example.com`。scheme 是键的一部分——`http` 与 `https` 是两个不同的站点，为 `https` 保存的凭据绝不会附加到明文 `http` 请求上。裸的 `example.com` 会被规范化为 `https://example.com`；要登记明文站点，必须自己写成 `http://example.com`。
- **`authRef`。** 指向一份凭据的稳定引用：`<插件 identity>::<规范化后的站点>`，例如 `acme@demo::https://www.example.com`。`flux.auth.save` 会把它返回给你，通常不需要手工拼。
- **按站点的默认引用。** 如果你不传 `authRef`，FluxDown 会根据插件 identity 和每个请求的 URL 推导出默认引用并自动查找凭据。这就是已保存的登录态被自动复用的方式。
- **档案（profile）。** 你保存的内容：Cookie、Bearer token、Basic 认证的用户名和密码，以及/或者固定的请求头，外加可选的过期时间。字段见 [`flux.auth`](/docs/zh/plugins/api-reference/)。

## 声明插件

认证插件既要有 `auth` 入口，**也要**声明 `auth` 权限。声明了 `auth.entry` 却没有该权限的 manifest 会被拒绝。

```json
{
  "auth": { "entry": "auth.js", "timeoutMs": 30000 },
  "permissions": ["auth"]
}
```

| 字段 | 含义 |
|---|---|
| `auth.entry` | 定义全局函数 `authenticate` 的脚本文件。 |
| `auth.timeoutMs` | 可选。单次调用的墙钟上限。默认 **30 秒**，上限也是 30 秒；与 resolver 的 `timeoutMs` 相互独立；不能为 `0`。 |
| `permissions: ["auth"]` | 解锁 `flux.auth`、`resolve` 里的 `ctx.authRef`，以及 `flux.fetch` 的自动凭据注入。没有该权限的插件拿不到任何凭据，显式传 `authRef` 会被 `插件未声明 auth 权限，不能使用认证凭据` 拒绝。 |

`auth` 权限属于*较高*风险：安装插件时会请用户授权。见[权限](/docs/zh/plugins/permissions/)。

## 用户如何登录

声明了 `auth.entry` 的插件，会在 设置 → 扩展 → 插件 里它那一行出现「登录」按钮（桌面端和 Web 端都有）。点开后是一个带两个可选输入框的对话框：

- **站点**——一个 URL，作为 `ctx.site` 传给你。如果你的插件只对应固定站点，可以留空。
- **登录信息**——自由文本，作为 `ctx.input` 传给你（账号、验证码等）。它是普通文本框，不做掩码。

对话框这样驱动你的插件：

1. **打开时**先发 `status`，看用户是否已经登录。
2. 点「开始登录」发 `begin`。
3. 如果回复是 `pending`，带有 `sessionId`，且 `challengeType` 是 `"qrcode"`，对话框会把挑战渲染成二维码图片，并**每 2 秒**自动发一次 `poll`。其他 `challengeType` 则把挑战以文本显示（带复制按钮），并提供一个「检查状态」按钮，点击时才发 `poll`。`data:image/…;base64,…` 形式的挑战会按图片显示。
4. 当回复是 `success` **并且** `authRef` 非空时，对话框显示已登录，并提供「注销」按钮，点击发 `logout`。
5. 无论以何种方式关闭对话框，只要有进行中的会话，都会发一次 `cancel`。

不用界面也能驱动同样的 action：通过 `/rpc` 调用 `daemon.plugin.auth`，或者对管理 API 发 `POST /api/v1/plugins/{identity}/auth`，请求体为 `{ "action", "site", "authRef", "sessionId", "input" }`。在无界面的服务器上登录、或者写脚本测试，就用这种方式。被禁用的插件无法登录，调用会以 `插件 <identity> 未启用` 失败。

## 自定义登录界面

默认情况下，「登录」按钮会打开上述固定对话框。插件可以通过声明一个带有 `location: "auth"` 的视图，用自定义界面替换整个对话框内容：

```json
{
  "permissions": ["auth"],
  "auth": { "entry": "auth.js" },
  "main": { "entry": "main.js" },
  "contributes": {
    "views": [{ "id": "login", "title": "Log in", "location": "auth" }]
  }
}
```

- 每个插件最多只能有一个 `auth` 视图，并且它需要顶层的 `auth` 声明（因此也需要 `auth` 权限）。该视图可以是可移植的（使用 `main` 和 `flux.ui.view` 渲染）或一个 [桌面脚本](/docs/zh/plugins/desktop-ui-guide/)；桌面应用更倾向于使用桌面脚本，而 Web UI 仅显示可移植视图。
- 当插件已启用、已加载且无待处理的同意时，对话框仅显示你的视图。标题仍为“登录 <plugin>”；站点/登录信息表单和固定按钮不会显示。否则，以及在移动应用中，将使用默认对话框。桌面应用在以下情况下也会回退到默认对话框：当视图仅包含桌面脚本且插件包不在本机（远程主机）上时。只要插件具有 `auth.entry`，按钮始终会显示。
- 从视图中驱动登录：调用你自己的 `flux.rpc.handle` 处理程序，或在 `main` 中运行流程，将结果存储于 `flux.auth.save`，然后调用 `ctx.close()` 来关闭对话框（桌面脚本：`fluxdown.close()`）。如果用户已登录，你可以直接从 `init` 调用 `ctx.close()`。
- 在此位置之前发布的应用会拒绝该清单，因此请将 `minAppVersion` 设置为下一个版本或更高版本。

```js
// main.js — QR login drawn by the plugin
const { h } = flux.ui;

flux.ui.view("login", {
  async init(ctx) {
    const res = await flux.fetch({ url: "https://passport.example.com/qr/generate" });
    const data = JSON.parse(res.body); // { key, url, image: "data:image/png;base64,…" }
    const state = { ctx, key: data.key, image: data.image, message: "Scan with the Example app", timer: 0 };
    state.timer = flux.timers.setInterval(async () => {
      const poll = JSON.parse((await flux.fetch({ url: "https://passport.example.com/qr/poll?key=" + encodeURIComponent(state.key) })).body);
      if (poll.state === "scanned") state.message = "Scanned, confirm on your phone";
      if (poll.state === "expired") state.message = "The QR code expired, reopen this dialog";
      if (poll.state === "confirmed") {
        flux.timers.clear(state.timer);
        await flux.auth.save({ site: "https://www.example.com", kind: "cookie", cookies: poll.cookies });
        state.ctx.close();
        return;
      }
      state.ctx.refresh();
    }, 2000);
    return state;
  },
  render: (state) =>
    h("stack", { gap: "md", padding: "md", align: "center" },
      h("image", { src: state.image, alt: "QR code", width: 200, height: 200 }),
      h("text", { text: state.message })),
  dispose(state) {
    flux.timers.clear(state.timer);
  },
});
```

该示例假定登录服务会直接返回二维码图像和已确认的 cookies；请根据你的站点调整请求（从 `Set-Cookie` 读取的 cookies 如 [完整示例](#完整示例二维码登录) 中所示）。如果没有 `auth` 视图，一切都不会改变：默认的表单对话框和 `authenticate` 操作的行为与之前完全相同。

## `authenticate` 入口

`authenticate(ctx)` 是一个处理全部五个 action 的全局函数。**每次调用都在全新的脚本上下文里运行**，所以 `begin` 和 `poll` 之间不会保留任何东西，除非你把它存进 `flux.storage`（以你发出的 `sessionId` 为线索）。

`ctx`：

| 字段 | 含义 |
|---|---|
| `action` | `begin`、`poll`、`cancel`、`logout` 或 `status`。 |
| `site` | 用户填写的站点，或 `""`。 |
| `authRef` | 凭据引用。如果调用方没传，但给了有效的 `site`，宿主会自动填成 `<identity>::<规范化站点>`。对 `poll`、`cancel`、`logout`，对话框会把你上一次回复里带的 `authRef` 原样传回来。 |
| `sessionId` | `begin` 时你返回的会话标识（`poll` 和 `cancel` 时会传回来）。 |
| `input` | 「登录信息」文本。 |

要返回一个**对象**（不是 JSON 字符串，序列化由引擎完成）。什么都不返回是错误。

| 字段 | 含义 |
|---|---|
| `status` | 必填：`pending`、`success` 或 `error`。其他值会以 `authenticate.status 必须是 pending/success/error` 失败。 |
| `sessionId` | 你的会话标识。只有当 `status` 是 `pending` **并且** `sessionId` 非空时，对话框才认为登录*正在进行*。 |
| `challenge` | 给用户看的内容：二维码内容、URL、`data:image/…;base64,…` 图片或说明文字。 |
| `challengeType` | `"qrcode"` 会让对话框把挑战渲染成二维码并自动轮询；其他取值按文本显示。 |
| `message` | 状态文本。只有 `status` 为 `error` 时才用错误样式显示。 |
| `authRef` | `flux.auth.save` 返回的凭据引用。如果你返回 `success` 但没带它，宿主会把请求里的 `authRef` 填回去。 |

处于 `pending` 时，如果 `poll` 的回复省略了 `challenge`/`challengeType`，界面会保留上一次的挑战。其他回复会清掉你省略的字段；显式给空字符串则移除挑战内容。二维码挑战如果是普通文本（包括登录 URL），会在本地渲染成二维码图片，这个 URL 绝不会被当作图片去请求。

### 各 action 该做什么

| Action | 你收到的 | 你应该做的 | 回复 |
|---|---|---|---|
| `begin` | `site`、`input`，可能还有 `authRef` | 启动流程，创建会话并记下来。一步到位的登录（账号密码、粘贴 Cookie）就在这里完成。 | 交互式流程返回 `pending` + `sessionId` + `challenge`；一步登录返回 `success` + `authRef`；无法启动返回 `error` + `message`。 |
| `poll` | `sessionId`、`authRef`、`input` | 向平台询问用户是否已完成。成功时调用 `flux.auth.save`。 | 等待中返回 `pending`（保留 `sessionId`）；完成返回 `success` + `authRef`；过期或被拒返回 `error`。 |
| `cancel` | `sessionId` | 丢弃会话（清理你的存储）。尽力而为；对话框会忽略回复。 | 任何合法状态均可；返回 `error` 并写上「已取消」比较自然。 |
| `logout` | `authRef`（或 `site`） | 可以通知平台结束会话。**你不必自己删除凭据。** | `success`。 |
| `status` | 可能有 `site`、`authRef` | 在没有副作用的前提下报告当前状态。 | 见下。 |

### `status` 的三种结果

`status` 是对话框（以及任何 API 客户端）了解用户是否已登录的途径。对话框只有在收到 `success` **且 `authRef` 非空**时才认为已登录，所以请只返回下面三种之一：

| 情形 | 回复 |
|---|---|
| 已登录且凭据可用 | `{ status: "success", authRef, message }` |
| 未登录 | `{ status: "pending", message: "未登录" }`——不带 `sessionId`，这样对话框只会提供「开始登录」，并以中性样式显示这条消息。 |
| 凭据存在但已过期或被拒绝 | `{ status: "error", message: "登录已过期，请重新登录" }` |

## 完整示例：二维码登录

一个虚构的站点 `www.example.com`，它的登录服务 `passport.example.com` 提供三个接口（这是二维码登录常见的形态）：

- `GET /qr/generate` → `{ "key": "…", "url": "https://…" }`——要放进二维码的文本。
- `GET /qr/poll?key=…` → `{ "state": "waiting" | "scanned" | "confirmed" | "expired" }`。`confirmed` 时响应带有 `Set-Cookie`。
- `GET https://www.example.com/api/me` → `{ "name": "…" }`；Cookie 缺失或无效时返回 HTTP 401。

`manifest.json`

```json
{
  "identity": "acme@example-login",
  "name": "Example Site Login",
  "version": "1.0.0",
  "description": "Signs in to www.example.com with a QR code and resolves its video pages.",
  "resolvers": [
    {
      "match": { "urls": ["*://www.example.com/video/*"] },
      "entry": "resolve.js"
    }
  ],
  "auth": { "entry": "auth.js" },
  "permissions": ["auth"]
}
```

`auth.js`

```js
const PASSPORT = "https://passport.example.com";
const SITE = "https://www.example.com";
const THIRTY_DAYS = 30 * 24 * 3600;

// Several Set-Cookie headers arrive as ONE string, joined with "\n". Keep only
// the "name=value" part of each line to build a request Cookie header.
function cookieHeader(setCookie) {
  return String(setCookie || "")
    .split("\n")
    .map((line) => line.split(";")[0].trim())
    .filter(Boolean)
    .join("; ");
}

function savedAuthRef(ctx) {
  // Canonical form: "<plugin identity>::<normalized site>".
  return ctx.authRef || flux.info.identity + "::" + SITE;
}

async function loadSession() {
  const raw = await flux.storage.get("session");
  return raw ? JSON.parse(raw) : null;
}

async function begin() {
  const res = await flux.fetch({ url: PASSPORT + "/qr/generate" });
  if (res.status !== 200) {
    return { status: "error", message: "QR service answered HTTP " + res.status };
  }
  const data = JSON.parse(res.body);
  // Each call runs in a fresh context: persist what poll will need.
  const sessionId = String(Date.now());
  await flux.storage.set("session", JSON.stringify({ sessionId, key: data.key }));
  return {
    status: "pending",
    sessionId,
    challenge: data.url,
    challengeType: "qrcode",
    message: "Scan the code with the Example app",
  };
}

async function poll(ctx) {
  const session = await loadSession();
  if (!session || session.sessionId !== ctx.sessionId) {
    return { status: "error", message: "The login session is gone, start again" };
  }

  const res = await flux.fetch({
    url: PASSPORT + "/qr/poll?key=" + encodeURIComponent(session.key),
  });
  const data = JSON.parse(res.body);

  if (data.state === "waiting") {
    // No challenge fields: the dialog keeps showing the QR code.
    return { status: "pending", sessionId: ctx.sessionId };
  }
  if (data.state === "scanned") {
    return { status: "pending", sessionId: ctx.sessionId, message: "Scanned, confirm on your phone" };
  }
  if (data.state !== "confirmed") {
    await flux.storage.set("session", "");
    return { status: "error", message: "The QR code expired" };
  }

  const cookies = cookieHeader(res.headers["set-cookie"]);
  const me = await flux.fetch({ url: SITE + "/api/me", headers: { Cookie: cookies } });
  const name = me.status === 200 ? JSON.parse(me.body).name : "";

  const authRef = await flux.auth.save({
    site: SITE,
    kind: "cookie",
    account: name,
    cookies,
    expiresAt: Math.floor(Date.now() / 1000) + THIRTY_DAYS,
  });
  await flux.storage.set("session", "");
  return { status: "success", authRef, message: name ? "Logged in as " + name : "Logged in" };
}

async function cancel() {
  await flux.storage.set("session", "");
  return { status: "error", message: "Login cancelled" };
}

async function logout() {
  // Optional courtesy call. The host deletes the credential afterwards anyway.
  try {
    await flux.fetch({ method: "POST", url: SITE + "/api/logout" });
  } catch (e) {
    flux.logger.warn("remote logout failed", String(e && e.message));
  }
  return { status: "success", message: "Logged out" };
}

async function status(ctx) {
  const profile = await flux.auth.get(savedAuthRef(ctx));
  if (!profile) return { status: "pending", message: "Not logged in" };

  // The request goes to the credential's own site, so the saved cookies are
  // attached automatically. An expired credential is skipped, which yields 401.
  const me = await flux.fetch({ url: SITE + "/api/me" });
  if (me.status !== 200) {
    return { status: "error", message: "Session expired, please log in again" };
  }
  return {
    status: "success",
    authRef: profile.authRef,
    message: "Logged in as " + JSON.parse(me.body).name,
  };
}

async function authenticate(ctx) {
  switch (ctx.action) {
    case "begin":
      return begin();
    case "poll":
      return poll(ctx);
    case "cancel":
      return cancel();
    case "logout":
      return logout();
    case "status":
      return status(ctx);
    default:
      return { status: "error", message: "Unsupported action " + ctx.action };
  }
}
```

`resolve.js`

```js
async function resolve(ctx) {
  let res;
  try {
    // ctx.authRef is the default credential for this URL's site ("" when not
    // logged in or when the permission is missing). Passing it makes a missing
    // login a hard error instead of an anonymous request.
    res = await flux.fetch({
      url: "https://www.example.com/api/play?u=" + encodeURIComponent(ctx.url),
      authRef: ctx.authRef || undefined,
    });
  } catch (e) {
    // The message carries a prefix such as "插件运行时错误: ", so use includes().
    if (String(e && e.message).includes("authentication_required")) {
      throw new Error("Please log in first: Settings → Extensions → Plugins → Log in");
    }
    throw e;
  }
  if (res.status === 401) throw new Error("Login expired, log in again");
  if (res.status !== 200) throw new Error("API answered HTTP " + res.status);

  const data = JSON.parse(res.body);
  return { url: data.url, fileName: data.fileName, ephemeral: true };
}
```

值得照搬的几点：

- **会话状态放进 storage。** `sessionId` 把一次 `poll` 与创建它的 `begin` 对应起来，过期的对话框无法完成别人的会话。只用一个 storage 键，而不是每个会话一个键，才不会撞上 100 个键的上限。
- **从登录响应里取 Cookie。** `flux.fetch` 会把重复的响应头用 `"\n"` 连接，并把头名转成小写，因此要读 `res.headers["set-cookie"]` 并 `split("\n")`。
- **返回 `flux.auth.save` 给你的 `authRef`**；对话框要靠它来显示已登录状态。
- **`logout` 很简单。** 宿主会在你的脚本返回之后（哪怕脚本抛了异常）删除它发来的那个 `authRef` 对应的凭据。正常的注销流程里你不需要调用 `flux.auth.remove`。

## 不需要挑战的变体

没有什么可扫的时候，直接在 `begin` 里完成，并马上返回 `success`。

### 账号和密码

用户在「登录信息」里输入 `name:password`：

```js
async function begin(ctx) {
  const i = ctx.input.indexOf(":");
  if (i <= 0) return { status: "error", message: "Enter the login as username:password" };
  const user = ctx.input.slice(0, i);
  const pass = ctx.input.slice(i + 1);

  const res = await flux.fetch({
    method: "POST",
    url: SITE + "/api/login",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: "username=" + encodeURIComponent(user) + "&password=" + encodeURIComponent(pass),
  });
  if (res.status !== 200) return { status: "error", message: "Login rejected (HTTP " + res.status + ")" };

  const authRef = await flux.auth.save({
    site: SITE,
    kind: "cookie",
    account: user,
    cookies: cookieHeader(res.headers["set-cookie"]),
  });
  return { status: "success", authRef, message: "Logged in as " + user };
}
```

只要可能，请保存站点返回的*会话 Cookie 或 token*，而不是密码。对于确实使用 HTTP Basic 的站点，保存 `{ site, kind: "basic", username, password }` 即可：FluxDown 会替你发送 `Authorization: Basic …`。

### 粘贴 Cookie 或 token

用户把浏览器里的 Cookie 字符串（或 API token）复制到「登录信息」里：

```js
async function begin(ctx) {
  const cookies = ctx.input.trim();
  if (!cookies) return { status: "error", message: "Paste your cookie string" };

  const me = await flux.fetch({ url: SITE + "/api/me", headers: { Cookie: cookies } });
  if (me.status !== 200) return { status: "error", message: "The cookie was rejected (HTTP " + me.status + ")" };

  const authRef = await flux.auth.save({ site: SITE, kind: "cookie", cookies });
  return { status: "success", authRef };
}
```

对于 bearer token，保存 `{ site, kind: "bearer", accessToken }`，FluxDown 会加上 `Authorization: Bearer …`。如果只是一个永不变化、不需要登录对话框的 token，用 `password` 控件的[设置项](/docs/zh/plugins/manifest/)比认证插件更简单。

## 保存凭据

`flux.auth.save(profile)` 返回 `authRef` 字符串。

| 字段 | 含义 |
|---|---|
| `site` | **必填。** 完整 URL 或 `host[:port]`，按上文规则规范化。无法使用的值以 `认证档案的 site 必须是有效的 URL 或 host` 失败；缺失则报 `认证档案必须提供 site`。 |
| `authRef` | 不要写。如果写了，必须等于 `<identity>::<规范化站点>`，否则报 `authRef 必须等于当前插件和规范化站点组成的引用`。 |
| `kind` | `cookie`、`bearer`、`basic`、`headers` 或 `session`。只有 `basic` 会改变行为（启用基于 `username`/`password` 的 Basic 认证），其余只是标签。 |
| `cookies` | 注入为 `Cookie` 请求头（请求已有 `Cookie` 时不覆盖）。 |
| `accessToken` | 注入为 `Authorization: Bearer …`（请求已有 `Authorization` 时不覆盖）。 |
| `username`、`password` | 配合 `kind: "basic"` 使用。 |
| `headers` | 额外的请求头；优先于上面这些字段生成的同名头。 |
| `expiresAt` | Unix **秒**，必须是整数。过了这个时间凭据就视为已过期。对于有效期由服务器决定的 Cookie，可以不写。 |
| `account`、`refreshToken`、`refreshAt`、`metadata` | 由你自用；只存储、不解释（`metadata` 是字符串到字符串的映射）。 |

超过 64 KB 的档案会被拒绝（`认证档案超过 65536 字节上限`）。`flux.auth.get(authRef)` 返回已保存的档案或 `null`；`flux.auth.remove(authRef)` 以 `true` 完成。两者对属于其他插件的 `authRef` 都会拒绝（`认证凭据不属于当前插件` / `authRef 必须属于当前插件`）。

## 在请求中使用凭据

有了 `auth` 权限，`flux.fetch` 会为每个请求查找凭据：

- **不带 `authRef`（隐式）。** 默认引用由你的 identity 和请求的站点推导。如果存在有效凭据就附加；没有或已过期时，请求不带凭据照常发出，不会报错。
- **显式 `authRef`**（通常是 `ctx.authRef`）。凭据不存在时以 `authentication_required: 未找到认证凭据 <authRef>` 拒绝；已过期则是 `authentication_required: 认证凭据已过期 <authRef>`。抛出的错误可能带有 `插件运行时错误: ` 之类的前缀，所以请用 `includes("authentication_required")` 判断，不要用 `startsWith`。

几条容易踩坑的规则：

1. **站点必须一致。** 凭据只会附加到 scheme、host、端口都与凭据 `site` 相同的请求上。把显式 `authRef` 用在别的站点上会被拒绝：`认证凭据不属于当前请求站点`。如果你的 resolver 要访问多个域名（`www.example.com`、`api.example.com`），请为每个域名各保存一份凭据。
2. **`ctx.authRef` 跟随任务 URL。** 在 `resolve` 里，`ctx.authRef` 是*任务 URL 所在站点*的默认引用，而不是你调用的 API 域名的。只在访问同一站点的请求里使用它；或者干脆不写 `authRef`，让宿主按每个请求推导。
3. **不跟随跨源重定向。** 附加了凭据的请求，如果被重定向到不同的 scheme、host 或端口，**不会**被跟随：你拿到的是 3xx 响应本身（含它的 `location` 头）。如果你接受不带凭据，就自己再请求；或者修正 URL（例如把 `http` 改成 `https`）。
4. **显式头优先。** 请求上已有的 `Cookie` 或 `Authorization` 头绝不会被凭据里的 Cookie 或 token 覆盖。
5. **钩子和订阅源**也可以用同样的方式使用凭据：声明了该权限后，`flux.auth` 与自动注入在钩子和 `subscribe` 里同样可用，但那里没有 `ctx.authRef`——请依赖隐式默认引用。

## 宿主替你做的事

- **注销一定会清理。** `logout` 调用返回之后——哪怕抛了异常——宿主都会删除所请求 `authRef` 对应的凭据。如果插件被**禁用**，注销甚至不会运行你的脚本，宿主直接删除凭据，这样用户总能从一个有问题的插件里登出。
- **不计入熔断。** `authenticate` 里的超时和崩溃，不会累加用于自动禁用 resolver 的计数。
- **独立的预算和池。** 单次调用最长 `auth.timeoutMs`（默认和上限都是 30 秒），与 resolver 无关。认证调用使用一个专用的小池（同时两个），登录轮询因此不会挤占下载任务；如果池满超过 3 秒，调用会以 `插件运行时过载（并发已满）` 失败。
- **必填设置项会校验。** `required` 设置项没有值时，调用以 `缺少必填设置项: 插件 <identity> 需先配置「<title>」` 失败。
- **参数校验。** `action` 必须是那五个之一（`auth action 必须是 begin/poll/cancel/logout/status`）；既没有 `authRef` 也没有 `site` 的 `logout` 会以 `logout 需要 authRef 或 site` 失败。

## 错误

| 消息 | 原因 |
|---|---|
| `authenticate 返回值非法: …` | 你什么都没返回、返回了非对象，或者无法解析。请返回普通对象。 |
| `authenticate.status 必须是 pending/success/error` | `status` 缺失或拼写错误。 |
| `入口 authenticate 未定义` | 脚本里没有全局的 `authenticate`（用 `const` 声明的箭头函数不算——请用 `function` 声明）。 |
| `插件 <identity> 未声明 auth 权限` / `插件 <identity> 未提供 auth 入口` | manifest 里缺少权限或 `auth.entry`。 |
| `脚本执行超时` | 调用超过了 `auth.timeoutMs`。 |
| `authentication_required: …` | 带显式 `authRef` 的 `flux.fetch` 没有有效凭据。 |
| `认证凭据不属于当前插件` | 使用了其他插件的 `authRef`。 |
| 对话框显示「登录失败：…」 | 上面任一错误的包装；冒号后面是引擎消息。 |
| 对话框显示「插件返回的登录响应无效」 | 回复里没有字符串类型的 `status`。 |

宿主和引擎的消息是中文的；上面按你在对话框和日志里实际看到的原文列出。

## 测试建议

1. 用开发模式安装文件夹，并授予 `auth` 权限。`auth.js` 在每次调用时都会重新读取；修改 `manifest.json` 后点插件行上的「重新加载」。
2. 打开「登录」。多用 `flux.logger.info`，并查看插件日志（见[调试与排错](/docs/zh/plugins/debugging/)）。
3. 登录成功后，从匹配的站点添加一个下载，在日志里检查 resolver 发出的请求。
4. 想一次只测一个 action，可以直接调用管理接口：`curl -X POST -H "Authorization: Bearer <token>" -d '{"action":"status"}' http://127.0.0.1:17800/api/v1/plugins/acme@example-login/auth`（需要启用管理 API，见 [API 概览](/docs/zh/api/overview/)）。
5. 要测试过期，可以保存一份 `expiresAt` 只比现在晚几秒的档案。

接下来：用[钩子指南](/docs/zh/plugins/hooks-guide/)响应下载事件（钩子里同样可以用 `flux.auth`），用[写第一个插件](/docs/zh/plugins/your-first-plugin/)了解 resolver 的基础。
