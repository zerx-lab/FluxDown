---
title: API Overview
description: The FluxDown HTTP API — route groups, authentication, and how it relates to the headless server.
section: api
order: 1
---

FluxDown exposes an HTTP API for extensions, userscripts, aria2 clients, and automation. The aria2 `/jsonrpc` endpoint and the official clients' `/rpc` endpoint are different protocols.

- **Desktop** listens on `127.0.0.1:17800` by default. GPUI's local-service settings can enable LAN access (`local_server_lan_enabled`), binding `0.0.0.0` on the next start; loopback is a default, not a hardcoded restriction. Management API and MCP default off; takeover and aria2 default on.
- **The [headless server](/docs/en/headless-server/setup/)** is `fluxdown-agent --server` with a sibling `fluxdownd`. Only `FLUXDOWN_BIND` determines its listener (default `[::]:17800`, IPv4 + IPv6). Compatibility groups default on for a fresh installation, but **an unset access key does not mean anonymous access**. The Web UI manages downloads over the agent's `/rpc`.

## Route groups

| Group | Endpoints | Switch | Authentication |
|---|---|---|---|
| Health | `GET /ping` | master switch | none |
| Script takeover | `POST /download`, `POST /download/batch` | `local_server_takeover_enabled` (default on) | nonempty `X-FluxDown-Client`; token required when configured |
| aria2 RPC | `POST /jsonrpc`, `GET /jsonrpc` (WebSocket) | `local_server_jsonrpc_enabled` (default on) | per-call token when configured, except method-list calls |
| Management API | `/api/v1/*` (tasks, queues, etc.) | `local_server_api_enabled` (desktop off; fresh headless on) | **required** token |
| MCP | `POST /mcp` | `local_server_mcp_enabled` (desktop off; fresh headless on) | **required** token, shared with management |

Before headless setup is complete, `/download`, `/download/batch`, and `/jsonrpc` (POST and WS upgrade) return **HTTP 403**, with `setup required: set the access key first`; method-list calls cannot bypass this gate. Management API and MCP also reject an empty key. The Web page, health check, and setup endpoints remain reachable (setup may return 503 until startup is ready). Initialize the key or preset a valid `FLUXDOWN_TOKEN` before using these compatibility endpoints; their individual switches still apply afterward.

When management is enabled, `GET /api/v1/openapi.json` provides an unauthenticated interface description. The legacy `fluxdown-server` extension REST routes (`/api/v1/config`, queue CRUD, stats, fs/list, components, webhooks, logs, `/api/v1/ws`, and `/api/v1/token/regenerate`) are no longer offered by the new host: use the Web UI or `/rpc`. Unauthenticated `GET /api/v1/setup/status` and `POST /api/v1/setup` bootstrap the first key; the latter only accepts requests while no key is set.

The BT anti-leech routes live under `/api/v1/bt/guard/{state,entries,events}` in the same management group; see [BT Anti-Leech](/docs/en/protocols/bt-guard/#rest-api).

## Authentication

Compatibility APIs and the Web UI share a user access key, persisted by the agent as `gateway_user_token`. Legacy `local_server_token` values are imported during migration. This is distinct from internal `agent.token` and `daemon.token` credentials.

| Group | Accepted forms |
|---|---|
| Script takeover | `X-FluxDown-Token`, plus a nonempty `X-FluxDown-Client` in every case. Only unexposed desktop configurations without a token allow anonymous use; headless rejects an empty key. |
| aria2 RPC | POST accepts `X-FluxDown-Token` or `params[0]="token:xxx"`; WS calls must carry the token in params. `system.listMethods` / `system.listNotifications` skip per-call token checks, not origin or first-setup gates. |
| Management / MCP | `Authorization: Bearer <token>` or `X-FluxDown-Token`; an empty key returns 403. |
| agent `/rpc` (WebSocket) | Bearer, or browser sub-protocols `fluxdown.rpc.v1, fluxdown.token.<base64url(token)>` (without padding). Browser connections carrying Origin must be same-origin with the service. |
| headless `/api/web/files/tasks/{id}`, `/api/web/exports/{id}` | Bearer or `?token=<token>`, for browser downloads that cannot set custom headers. |

On desktop, enabling LAN or CORS while takeover or aria2 is enabled **automatically generates and persists a random token if the key is empty**. Existing keys are retained; clearing a key while that exposure remains generates another. Startup and legacy migration enforce the same rule. Configure external tools with this token. Headless deliberately does not auto-fill the key, preserving its first-setup wizard.

### Cross-origin and Host validation

No `Access-Control-Allow-Origin` is returned by default. Takeover's custom headers trigger a CORS preflight; simple POSTs and WS upgrades do not, so the server also checks Origin. On `/jsonrpc` (POST/WS) and `/download*`, only extension origins (`chrome-extension://`, `moz-extension://`, `safari-web-extension://`) and requests same-origin with Host are accepted; other browser cross-origin requests return 403. Clients sending no Origin, such as CLI, aria2 clients, and `GM_xmlhttpRequest`, are unaffected by this origin gate but still need the required token.

For desktop loopback listeners, core API requests must also use Host `127.0.0.1`, `localhost`, or `[::1]` (an optional port is accepted), preventing DNS rebinding. LAN/headless listeners do not enforce a loopback Host. Enabling CORS does not remove this Host check.

**Allow cross-origin access from any website (CORS)** (`local_server_cors_allow_all`, default off) returns `Access-Control-Allow-Origin: *`, permits private-network preflights, echoes requested headers, and lifts the compatibility endpoints' Origin gate. Any page can then probe the service and, if it obtains the token, invoke compatibility endpoints; token checks remain. aria2 and management create tasks directly without confirmation, so a desktop takeover dialog is not a universal safeguard.

## Takeover versus direct task creation

`/download*` uses the external-download flow: desktop may ask for confirmation or create silently under do-not-disturb settings; headless has no confirmation window and creates directly after authorization. `aria2.addUri` / `aria2.addTorrent` and management `POST /api/v1/tasks` always create directly, without that confirmation flow. **Headless takeover and aria2 are unavailable before first setup**.

## Batch RPC and slow methods

aria2 `/jsonrpc` accepts a top-level JSON array, executing entries in order and returning corresponding responses; `system.multicall` accepts nested calls. Every authenticated child needs its own leading `token:xxx` param (POST can alternatively share a valid token header); a token only on the outer multicall envelope is insufficient. Nested multicall is forbidden.

agent/daemon `/rpc` accepts one request object per frame, not that top-level array. Send separate requests with distinct IDs and match responses by ID for concurrent calls. Both use `native/protocol/src/method.rs::SLOW_DAEMON_METHODS`: daemon runs slow calls with bounded concurrency, and agent gives them an independent slow lane, keeping installs and network probes from blocking pause/resume or settings writes. Ordinary daemon commands retain their lane's ordering.

For bulk local-task mutations, prefer one `daemon.task.pauseMany {taskIds}`, `daemon.task.resumeMany {taskIds}`, or `daemon.task.deleteMany {taskIds, deleteFiles}` call over many concurrent single-task requests. The daemon deduplicates IDs and ignores unknown ones; an empty list is a no-op. It emits a consolidated task snapshot after the batch rather than a per-task snapshot flood.
## Internal daemon ↔ agent authentication

This is not an external API login mechanism. After a credential-free WS upgrade, a new agent uses `system.auth.challenge` / `system.auth.prove` to verify the daemon and then prove possession of the shared long-term key, before `system.hello`. The long-term `daemon.token` is never transmitted. Blob, file, and export HTTP requests use a session Bearer derived independently by both parties and revoked on disconnect; no authenticated session means no fallback to the long-term token.

The daemon still accepts valid static Bearers from an older resident agent, on WS upgrade and HTTP, for the window where binaries have been replaced but that process is still running. A new agent **never** downgrades by sending its long-term token to an old daemon. This compatibility does not permit pairing-protocol downgrade.

## LAN pairing v2

Pairing uses a single-use six-digit code (120-second lifetime) and a SAS comparison on both devices, not sharing the user API key. The required sequence is `/api/v1/link/pair/hello` → `/api/v1/link/pair/reveal` → `/api/v1/link/pair/confirm`:

1. hello carries `protocolVersion: 2` and a SHA-256 **commitment** to the initiator's ephemeral key/nonce, not those values themselves. The responder returns its fresh ephemeral values first.
2. reveal discloses the committed values. After commitment verification and validation of the identity signature over the full transcript, each device computes and displays its six-digit SAS locally; the SAS is not transmitted as a field. A mismatched commitment or reveal later than 30 seconds invalidates the session. An old reveal cannot be replayed, and confirm cannot skip this step.
3. Users compare the SAS on both devices and approve before the responder permits registration. The code alone provides no protection against a man-in-the-middle; never skip SAS comparison.

Protocol versions must match exactly; a missing version is treated as 0 and rejected. **The old, commitment-free handshake is incompatible and never used as a downgrade**. hello/reveal/confirm use the code, commitment, session, and local approval rather than the management user-token gate. Upgrade both devices if pairing reports a version mismatch.

## curl examples

Create a task directly (management API):

```bash
curl -X POST http://<host>:17800/api/v1/tasks \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/file.zip","segments":8}'
# -> {"taskId":"..."}
```

List tasks:

```bash
curl http://<host>:17800/api/v1/tasks \
  -H "Authorization: Bearer <token>"
```

Add a download via the aria2-compatible RPC (works with existing aria2-targeting userscripts/clients):

```bash
curl -X POST http://<host>:17800/jsonrpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": "1",
    "method": "aria2.addUri",
    "params": ["token:<token>", ["https://example.com/file.zip"]]
  }'
```

`CreateTaskRequest` accepts `url` (required), and optional `fileName`, `saveDir`, `segments`, `cookies`, `referrer`, `proxyUrl`, `userAgent`, `queueId`, `checksum` (`algo=hexhash`), and `headers` — all camelCase in the JSON body. A supplied `fileName` is sanitized (path separators and `..` stripped) so the download always stays inside its save directory. The full schema is in the OpenAPI document below.

## MCP (Model Context Protocol)

FluxDown speaks [MCP](https://modelcontextprotocol.io) over HTTP, so AI clients (Claude Desktop, Cursor, Cline, and any MCP-capable agent) can drive downloads in natural language. It's a single endpoint, `POST /mcp`, protected by the same token as the management API.

MCP is JSON-RPC 2.0 over one HTTP endpoint (not REST) — every operation is one POST to `/mcp`, distinguished by the `method` in the body, using the stateless subset of the Streamable HTTP transport: requests get an `application/json` response, notifications get `202 Accepted`, and no session id is tracked. Authenticate with `Authorization: Bearer <token>` (or `X-FluxDown-Token`); the spec permits a static bearer token for internal deployments in place of OAuth 2.1.

### Tools

A client calls `tools/list` to discover these at runtime (each ships a full JSON Schema for its arguments), then `tools/call` to invoke one:

| Tool | What it does | Arguments |
|---|---|---|
| `download_add` | Create a download task (HTTP/HTTPS/FTP/magnet/BitTorrent). Returns the new task id. | `url` (required); optional `fileName`, `saveDir`, `segments`, `proxyUrl`, `cookies`, `referrer`, `userAgent`, `queueId`, `checksum` |
| `download_list` | List tasks, optionally filtered by status. | `status` (optional: `all`/`pending`/`downloading`/`paused`/`completed`/`error`/`preparing`) |
| `download_get` | Get one task's full detail by id. | `taskId` (required) |
| `download_pause` | Pause a task. | `taskId` (required) |
| `download_resume` | Resume a paused task. | `taskId` (required) |
| `download_pause_all` | Pause all active tasks (pending / downloading / preparing). | none |
| `download_resume_all` | Resume all paused tasks. | none |
| `download_remove` | Delete a task, optionally removing the file on disk. | `taskId` (required); optional `deleteFiles` (bool) |
| `queue_list` | List all named queues and their config. | none |

All nine map straight onto the management API's host capabilities, so an MCP client and a REST client see exactly the same tasks and queues.

### Connecting a client

Point your MCP client at the endpoint with a bearer token, e.g. in an `mcp.json`:

```json
{
  "mcpServers": {
    "fluxdown": {
      "url": "http://<host>:17800/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

Or exercise it directly with curl — initialize, then call a tool:

```bash
curl -X POST http://<host>:17800/mcp \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"download_add",
                 "arguments":{"url":"https://example.com/file.zip","segments":8}}}'
```

## The fluxdown:// URL protocol

Alongside the HTTP API, FluxDown registers a custom URL protocol that any web page, script, or third-party app can use to hand a download off — no local HTTP call required:

```text
fluxdown://download?url=<percent-encoded URL>&filename=<optional name>
```

- `url` — required. The address to download, percent-encoded (`http`/`https`/`ftp` direct links or a `magnet:` link). A `fluxdown://` URL with a missing or empty `url` parameter is silently ignored.
- `filename` — optional. A suggested file name, pre-filled for the user to keep or change. Useful when the real name only exists in a `Content-Disposition` header the receiving app will never see.

Who answers it depends on the platform:

- **Desktop (Windows, macOS, Linux)** — the app registers the protocol handler (Windows registry on every startup; a `CFBundleURLTypes` declaration on macOS; an `x-scheme-handler` entry in the `.desktop` file on Linux). Opening a `fluxdown://` URL launches the app (or forwards to the already-running instance) and routes the request into the same external-download flow as browser-extension requests: a quick-download confirmation by default, silent task creation if the user enabled no-prompt downloads. On Android and in restricted desktop environments, the browser extension itself can deliver through this protocol — see [the fluxdown:// protocol mode](/docs/en/browser-extension/usage/).
- **Android** — the app declares a VIEW intent-filter for the scheme. Opening the URL wakes the app and shows the new-download sheet with `url` and `filename` pre-filled; the user confirms before anything downloads. Successive protocol URLs arriving while the sheet is open are merged into it as additional lines (this is how the browser extension delivers batch downloads on Android).

A plain HTML link is enough to integrate:

```html
<a href="fluxdown://download?url=https%3A%2F%2Fexample.com%2Ffile.zip&filename=file.zip">
  Download with FluxDown
</a>
```

Note the protocol carries no cookies, headers, or credentials — the receiving app fetches the URL from scratch. For authenticated downloads, use the script-takeover or management endpoints above, which accept `cookies` and `headers` in the request body.

## Interactive documentation

- [`/api-docs`](/api-docs) on this site renders the full OpenAPI 3.1 spec (generated from the actual route handlers) with a try-it-out UI, for the routes common to both hosts.
- A running server also serves its own live spec at `/api/v1/openapi.json` (raw JSON) — always in sync with the exact build you're running.
