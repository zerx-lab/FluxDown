---
title: Server Setup
description: Build and run the headless FluxDown server (fluxdown-agent --server + fluxdownd), configure it with environment variables, and expose it safely.
section: headless-server
order: 1
---

The headless server is `fluxdown-agent --server` plus its sibling `fluxdownd` download daemon: no desktop UI, no tray, no file associations. It exposes the same Rust engine (HTTP/HTTPS, FTP, BitTorrent, HLS, DASH) through a Web UI baked into the `fluxdown-agent` executable and a JSON-RPC endpoint (`/rpc`, the same protocol the desktop client uses), so you can run it on a NAS, a home server, or a VPS and manage downloads remotely from a browser. Releases ship **two binaries in one folder** — `fluxdown-agent` (with the Web UI embedded) and `fluxdownd`. Keep them in the same directory: the agent starts `fluxdownd` as a child process, and stops it again on `SIGTERM`/`SIGINT`.

For most deployments the prebuilt Docker image is the easiest path — see [Docker & NAS](/docs/en/headless-server/docker/). On Windows, use the prebuilt zip — no compiling needed; see the next section. The rest of this page covers building and running from the workspace source with Cargo, plus configuration that applies to every method.

## Windows: run the prebuilt package

Windows needs neither a build nor an installer. Steps:

1. Download `FluxDown-Server-<version>-windows-x64.zip` from the [download page](/download/) or [GitHub Releases](https://github.com/zerx-lab/FluxDown/releases) (`windows-arm64` on ARM devices). The archive holds one `fluxdown-server-<version>-<platform>\` folder containing `fluxdown-agent.exe` and `fluxdownd.exe`. Move those two files into a fixed folder **without a version number**, such as `D:\FluxDown\server\`, and **keep both in the same directory** (a fixed path means the launcher script and firewall rule below survive upgrades).
2. In PowerShell, go to that folder, set environment variables, then start the agent (see [Environment variables](#environment-variables); all optional):

   ```powershell
   cd D:\FluxDown\server
   $env:FLUXDOWN_DATA_DIR = "D:\FluxDown\data"      # data dir: pick a stable, backed-up path
   $env:FLUXDOWN_SAVE_DIR = "D:\Downloads"          # default save dir, applied on first start
   .\fluxdown-agent.exe --server
   ```

3. Open `http://localhost:17800/` in a browser and set the access key in the Initialize FluxDown Server wizard (see [First run](#first-run-set-the-access-key-in-the-web-ui)).

Differences from Linux:

- The Windows release `fluxdown-agent.exe` is a GUI-subsystem program: **no console window and no console log output**; PowerShell returns immediately and the server keeps running in the background. Check that `http://localhost:17800/` opens in a browser, or look for the processes in Task Manager / `Get-Process fluxdown-agent, fluxdownd`.
- The default bind is `[::]:17800` (all interfaces). To reach it from other devices on the LAN, allow it through Windows Firewall (elevated PowerShell; `-Profile Private` keeps it closed on public networks):

  ```powershell
  New-NetFirewallRule -DisplayName "FluxDown Server" -Direction Inbound -Program "D:\FluxDown\server\fluxdown-agent.exe" -Action Allow -Profile Private
  ```

  For local-only use, set `$env:FLUXDOWN_BIND = "127.0.0.1:17800"`. Finish access-key setup before exposing it, see [Exposing it safely](#exposing-it-safely-reverse-proxy--tls) below. If the FluxDown desktop app is installed on the same machine, give the server a different port (for example `$env:FLUXDOWN_BIND = "[::]:17900"`) so the two don't collide on 17800.
- Without `FLUXDOWN_DATA_DIR` the location **depends on the account running it** and is split in two: engine data in `%LOCALAPPDATA%\FluxDown\`, agent state (including the access key and device identity) in `%APPDATA%\zerx\FluxDown\data\agent`. Always set `FLUXDOWN_DATA_DIR` explicitly (engine data goes in its root, agent state in `<root>\agent`), so backups and migrations get everything.
- Windows has no `SIGTERM`, and this program has no console to press Ctrl+C in, so **stopping means force-ending the processes**. Pause your tasks in the Web UI first, then run this in PowerShell (the path filter leaves a desktop FluxDown install alone; use an elevated shell if the server runs under another account):

  ```powershell
  Get-Process fluxdown-agent, fluxdownd -ErrorAction SilentlyContinue |
    Where-Object Path -like 'D:\FluxDown\server\*' | Stop-Process
  ```

### Start at boot / keep it running

FluxDown Server is **not a native Windows service** (there is no service entry point for `sc create`). To start it automatically, use the built-in Task Scheduler.

> **Do not run it as `SYSTEM` or as an administrator.** The access key is effectively full control of the host: the Web UI can point the save directory anywhere and the server has no save-directory sandbox, so anyone holding the key (or exploiting a bug) could write into `System32` or an all-users startup folder and run it with the server's privileges. Use a dedicated standard (non-admin) local account, the same least-privilege idea as the dedicated `fluxdown` user in the Linux systemd example.

**1. Create the dedicated account and grant folder access** (elevated PowerShell, one time):

```powershell
$cred = Get-Credential -UserName fluxdown -Message "Set a password for the dedicated FluxDown account"
New-LocalUser -Name fluxdown -Password $cred.Password -PasswordNeverExpires -UserMayNotChangePassword
Add-LocalGroupMember -Group Users -Member fluxdown

New-Item -ItemType Directory -Force D:\FluxDown\data, D:\Downloads | Out-Null
icacls D:\FluxDown\data /grant "fluxdown:(OI)(CI)M"    # data dir: modify
icacls D:\Downloads     /grant "fluxdown:(OI)(CI)M"    # save dir: modify
```

The account needs write access to the data directory and to every download folder, and only read & execute on `D:\FluxDown\server\` (see Upgrading below). If the task later fails to start with a logon error, grant the account **Log on as a batch job** in `secpol.msc` → Local Policies → User Rights Assignment.

**2. Create a launcher script** at `D:\FluxDown\start-server.cmd` (environment variables go here, since a scheduled task cannot carry them itself). The loop restarts the server 10 seconds after it exits, which is the crash self-healing:

```bat
@echo off
set FLUXDOWN_DATA_DIR=D:\FluxDown\data
set FLUXDOWN_SAVE_DIR=D:\Downloads
cd /d D:\FluxDown\server
:loop
start "" /wait fluxdown-agent.exe --server
ping -n 11 127.0.0.1 >nul
goto loop
```

`start /wait` keeps the script alive until the server exits, so Task Scheduler sees the task as still running; `ping -n 11` is a 10-second pause that, unlike `timeout`, also works without a console.

**3. Register the task** (elevated PowerShell; starts at boot, no login needed, runs as the dedicated account with standard rights, no run-time limit):

```powershell
$action   = New-ScheduledTaskAction -Execute "D:\FluxDown\start-server.cmd"
$trigger  = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable
Register-ScheduledTask -TaskName "FluxDown Server" -Action $action -Trigger $trigger -Settings $settings `
  -User "$env:COMPUTERNAME\fluxdown" -Password $cred.GetNetworkCredential().Password -RunLevel Limited

Start-ScheduledTask -TaskName "FluxDown Server"   # start now, no reboot needed
```

Task Scheduler's own `-RestartCount` / `-RestartInterval` only retry when the task fails to **launch**; once the action has started, any later exit counts as a finished run, so they don't recover a crashed server. That is why the loop lives in the script.

Notes:

- **Download folder on a network share:** a mapped drive letter (such as `Z:\`) only exists inside an interactive session and is invisible to a boot-time task. Use the UNC path (`\\nas\share\downloads`) as the save directory instead, and make sure the share grants access to the `fluxdown` account (for example a same-name account with the same password on the NAS). Do not switch to `-LogonType S4U` for this: Microsoft documents that S4U logons have no access to the network or to encrypted files. If you only want it to run while you are signed in, a `-AtLogOn` trigger with your own account (`-LogonType Interactive`) is the alternative.
- **Stop or remove:** run `Stop-ScheduledTask -TaskName "FluxDown Server"` first (otherwise the loop restarts the server), then end any leftover processes with the `Get-Process … | Stop-Process` command above (ending the task does not necessarily take the background processes with it). `Unregister-ScheduledTask -TaskName "FluxDown Server" -Confirm:$false` deletes the task.
- **Upgrading:** the Windows server binary supports in-app update (Web UI → Settings → About), which needs the account to have write access to `D:\FluxDown\server\`. Since the update restarts the server by itself, it would race with the loop script, so with this setup we recommend a manual upgrade: `Stop-ScheduledTask`, end the processes, overwrite `fluxdown-agent.exe` and `fluxdownd.exe` in `D:\FluxDown\server\` with the new ones from the archive, then `Start-ScheduledTask`. The data directory is untouched.
- If you need a native Windows service you can use a third-party service wrapper, but that is not an officially supported setup.

## Build and run

The agent lives in `native/agent` (package `fluxdown_agent`, binary `fluxdown-agent`) and the daemon in `native/daemon` (package `fluxdown_daemon`, binary `fluxdownd`). Build **both** into the same target directory, then start the agent in server mode. From the repository root:

```bash
# Production build (Web UI embedded in the agent via the web-ui feature)
cargo build --release -p fluxdown_agent --features web-ui
cargo build --release -p fluxdown_daemon
# binaries in target/release/: fluxdown-agent and fluxdownd (.exe on Windows)

# Run (default bind [::]:17800, IPv4 + IPv6)
./target/release/fluxdown-agent --server
```

The agent spawns its sibling `fluxdownd`, which opens its own SQLite (or PostgreSQL) database and runs the download engine, while the agent serves the Web UI from bytes embedded in its executable — no separate database server, static file directory, or reverse proxy is required to get started.

## Building the web front end

Only needed when you build the server yourself — official release binaries and Docker images already contain the UI.

The Web UI is a separate SPA in `web/` (React 19 + TanStack, built with [Bun](https://bun.sh)). Its build output is embedded into `fluxdown-agent` **at compile time**, so it must exist *before* you build the agent:

```bash
cd web
bun install
bun run build      # outputs to web/dist

cd ..
cargo build --release -p fluxdown_agent --features web-ui   # embeds web/dist
```

Rebuild the agent after every front-end change — the running binary keeps serving the bytes it was compiled with (or set `FLUXDOWN_WEBROOT` to serve a directory live). Two compile-time knobs:

- `FLUXDOWN_EMBED_WEBROOT` — embed a different directory instead of `web/dist` (used by CI, which builds the SPA in a separate job).
- If the directory is missing or empty, the build still succeeds with a warning; the server then answers browser requests with a `503` page explaining how to fix it, while `/rpc` and the HTTP API keep working normally.

<!-- TODO(screenshot): browser showing the first-run “Initialize FluxDown Server” wizard -->

## Environment variables

All configuration is read once at startup from environment variables. There is no config file.

| Variable | Default | Description |
|---|---|---|
| `FLUXDOWN_BIND` | `[::]:17800` | TCP address the HTTP/WebSocket server listens on. `[::]` accepts both IPv4 and IPv6 (needed for IPv6 DDNS and fnOS FN Connect) and falls back to `0.0.0.0` when the system has no IPv6; an explicit `0.0.0.0:<port>` listens on IPv4 only. |
| `FLUXDOWN_DATA_DIR` | Platform auto-detected (see below) | Root data directory. The daemon/engine data (database, logs) live directly in it; the agent's own state lives in `<root>/agent`. |
| `FLUXDOWN_SAVE_DIR` | unset — platform download directory | Initial default save directory, applied only on first start (seeded, by the daemon, when no `default_save_dir` is stored yet). A directory later chosen in Settings always wins. The Synology package uses it to point at the shared folder picked in the install wizard. |
| `FLUXDOWN_DATABASE_URL` | unset — uses a SQLite file inside the data dir | Explicit connection string: `sqlite:/path/to/file.db` or `postgres://user:pass@host/db`. |
| `FLUXDOWN_WEBROOT` | unset — serves the embedded Web UI | Optional override: serve the SPA from this directory instead of the embedded copy (custom front end, or a hot-swapped `bun run build` output). There is **no** implicit `./web` lookup next to the executable. |
| `FLUXDOWN_TOKEN` | unset — first-run Web setup wizard | Optional pre-set management access key (this is the gateway user token the Web UI and API authenticate with). Applied only when no key is stored yet (value is trimmed; must satisfy the key rules below, otherwise ignored with a warning). Use for unattended docker-compose / k8s / CI deploys that skip the wizard. See `FLUXDOWN_TOKEN_FORCE` below to override an existing key instead. |
| `FLUXDOWN_TOKEN_FORCE` | unset — `FLUXDOWN_TOKEN` only seeds an empty key | Truthy value (`1`/`true`/`yes`/`on`) makes `FLUXDOWN_TOKEN` override the stored key on every boot, instead of only when no key is set yet. Use when an orchestrator (Kubernetes Secret, docker-compose env) is the single source of truth for the key and Web UI key changes should not stick across restarts. |
| `FLUXDOWN_ALLOW_LOCAL_PLATFORM` | unset (off) | Allows every authenticated WebSocket client to invoke “open file” / “show in folder” on the server OS. Direct browser connections to literal `localhost` or a loopback address get this capability automatically; public reverse-proxy origins do not. **Leave this unset on remotely accessible servers**: enabling it lets remote users trigger the OS opener for downloaded files. |
| `FLUXDOWN_DEMO` | unset (off) | Truthy value (`1`/`true`/`yes`/`on`) turns on demo mode: only a built-in, generated 64 MiB file can be downloaded. Useful for public demos. |
| `FLUXDOWN_DEMO_URL` | unset (off) | Overrides demo mode's allowed URL with a specific one instead of the built-in generated file. |
| `FLUXDOWN_LANG` | unset (falls back to browser language) | Fallback language reported by `/ping` (`en`/`zh`) for the Web UI's first load. Users who pick a language in their browser always keep their own choice. |
| `FLUXDOWN_MDNS` | on | Device-link mDNS advertisement switch. Set to a falsy value to stop announcing this server on the LAN. |
| `FLUXDOWN_LINK_NAME` | `FluxDown Server` | Device name shown to other FluxDown clients when link discovery is on. |
| `FLUXDOWN_LINK_ADVERTISE_ADDR` | unset (auto-detect) | Overrides the reachable address(es) this server reports to other FluxDown devices when pairing and exchanging device info. Comma-separated `host[:port]` or `http(s)://` URLs (a missing port defaults to the actual listen port). Set it when running in Docker / behind NAT / on a multi-NIC host, where the auto-detected address is an internal one other devices cannot reach — for example `192.168.1.5:8080`. |
| `FLUXDOWN_ANALYTICS` | on | Anonymous usage statistics switch (only active in builds that carry an app key; official builds do). Set to a falsy value to disable. |
| `FLUXDOWN_LOG_LEVEL` | unset — `info` | Default `tracing` log level (`error`/`warn`/`info`/`debug`/`trace`, case-insensitive) applied when `RUST_LOG` is not set. `RUST_LOG` always wins when present — use `FLUXDOWN_LOG_LEVEL` for a simple one-value override, `RUST_LOG` for per-module directives. Read once at startup; changing it requires a restart. |

When `FLUXDOWN_DATA_DIR` is not set, the data directory is auto-detected the same way the desktop app does:

| Platform | Directory |
|---|---|
| Windows (portable build) | next to the executable |
| Windows (installed) | `%LOCALAPPDATA%\FluxDown\` |
| Linux | `$XDG_DATA_HOME/fluxdown/` |
| macOS | `~/Library/Application Support/fluxdown/` |

For a headless deployment you almost always want to set `FLUXDOWN_DATA_DIR` explicitly to a stable, backed-up path instead of relying on auto-detection.

```bash
FLUXDOWN_BIND=0.0.0.0:8080 \
FLUXDOWN_DATA_DIR=/srv/fluxdown/data \
./fluxdown-agent --server
```

## First run: set the access key in the Web UI

Fresh installations enable the compatibility API groups (takeover, aria2, management, MCP), with CORS off by default; only `FLUXDOWN_BIND` determines the listener. While no access key is set, `/download`, `/download/batch`, and `/jsonrpc` (POST/WS upgrade) return HTTP 403 (`setup required: set the access key first`); management and MCP also reject an empty key. aria2 and script takeover are therefore unavailable before first setup. The Web page and setup endpoints remain reachable; `GET /api/v1/setup/status` reports `setupRequired` (or 503 until startup is ready). Headless does not apply desktop LAN/CORS automatic token generation.

Open `http://<server-ip>:17800/`. The login page becomes an **Initialize FluxDown Server** wizard (not a normal sign-in form): enter an access key, confirm it, optionally click the button to random-generate one, optionally check “Remember this device”, then save. You are signed into the main UI immediately — no server restart.

Key rules (enforced the same way in the Web UI and the API):

- ASCII printable characters only (no spaces, no non-ASCII)
- Length 8–128
- Must contain both letters and digits

Once saved, the key is stored in the agent's state (`<data dir>/agent`) and survives restarts as long as the data directory persists. Use it to:

- Sign in to the Web UI (see [Web UI](/docs/en/headless-server/web-ui/)).
- Authenticate management API calls with `Authorization: Bearer <token>` (see [API Overview](/docs/en/api/overview/)).

This flow (instead of “generate a token and print it once to stderr”) exists because NAS users (Synology, QNAP, Unraid, and similar) often cannot see container or package stderr — a one-shot printed secret effectively locked them out.

### Unattended deploys

To skip the wizard (docker-compose, Kubernetes, CI), preset the key with `FLUXDOWN_TOKEN`. It is adopted only when the database still has no key:

```bash
FLUXDOWN_TOKEN='replace-with-strong-key-2026' ./fluxdown-agent --server
```

To instead force the environment variable to always win — even after someone changes the key from the Web UI — also set `FLUXDOWN_TOKEN_FORCE=1`:

```bash
FLUXDOWN_TOKEN='replace-with-strong-key-2026' FLUXDOWN_TOKEN_FORCE=1 ./fluxdown-agent --server
```

### Security note

The setup window is first-come, first-served: whoever reaches the wizard first sets the key. Finish initialization (or preset `FLUXDOWN_TOKEN`) before exposing the server on an untrusted network.

### Resetting the access key

If you lose the key or suspect it leaked, change it in the Web UI (**Settings → Security & Access**) while signed in. If you can no longer sign in, restart the server once with `FLUXDOWN_TOKEN=<new key>` and `FLUXDOWN_TOKEN_FORCE=1`: the environment value overwrites the stored key on start-up (it must satisfy the key rules above, otherwise it is ignored with a warning). Remove `FLUXDOWN_TOKEN_FORCE` afterwards if you want Web UI changes to stick.

The new key takes effect immediately; the old one is invalidated at the same moment.

## Database: SQLite and PostgreSQL

By default the server opens a SQLite database file inside the data directory — no setup needed. For multi-instance or higher-throughput deployments, point it at PostgreSQL instead:

```bash
FLUXDOWN_DATABASE_URL=postgres://fluxdown:password@localhost/fluxdown \
./fluxdown-agent --server
```

The connection string's scheme (`sqlite:` vs `postgres:`) selects the backend; both share the same schema and migrations. Credentials in `FLUXDOWN_DATABASE_URL` are masked in the server's own log output, but treat the environment variable itself like any other secret (avoid putting it in shell history or committing it to a process manager's config in plaintext where avoidable).

## Exposing it safely (reverse proxy & TLS)

`FLUXDOWN_BIND` defaults to `[::]:17800`, listening on every interface over both IPv4 and IPv6. Desktop defaults to loopback but can explicitly enable LAN; headless does not use that desktop LAN switch. **The deployment owner is responsible for the network boundary**:

- The management access key is the only thing standing between the internet and full remote control of your server (create/delete downloads, stream any completed file back). Treat it like a root password: don't share it, don't log it, rotate it if it may have leaked.
- If the server is reachable beyond a trusted LAN, put it behind a reverse proxy (nginx, Caddy, Traefik) terminating TLS, and only expose HTTPS. The Web UI sends the key in a WebSocket sub-protocol header and, for file downloads, a query string; on plain HTTP that is visible to anyone on the network path.
- The WebSocket endpoint (`/rpc`, used by the Web UI) needs `Upgrade`/`Connection` headers forwarded by the proxy. A minimal nginx snippet:

  ```nginx
  location / {
      proxy_pass http://127.0.0.1:17800;
      proxy_http_version 1.1;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection "upgrade";
      proxy_set_header Host $http_host;
  }
  ```

  The `/rpc` handshake checks the browser's `Origin`: the `Host` (or `X-Forwarded-Host`) the proxy forwards must be the hostname the browser used, not a LAN IP or `127.0.0.1`; otherwise the handshake is rejected (403) and the Web UI only shows "Cannot reach the FluxDown service." `$http_host` keeps the browser's host and port as-is; on Synology DSM add the WebSocket entries under "Custom Header", and in Nginx Proxy Manager enable "Websockets Support".

- Prefer binding to a private interface (`FLUXDOWN_BIND=127.0.0.1:17800` and letting the reverse proxy sit in front, or a VPN/Tailscale address) over exposing the port directly to the public internet, even with TLS.

## Running as a systemd service

A minimal unit file for a Linux deployment (adjust paths and user):

```ini
[Unit]
Description=FluxDown headless download server (agent + daemon)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fluxdown
Group=fluxdown
WorkingDirectory=/opt/fluxdown
Environment=FLUXDOWN_BIND=[::]:17800
Environment=FLUXDOWN_DATA_DIR=/var/lib/fluxdown
ExecStart=/opt/fluxdown/fluxdown-agent --server
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

Extract the release archive into `/opt/fluxdown` so that `fluxdown-agent` and `fluxdownd` sit side by side (the agent carries the Web UI inside it — nothing else to install; `systemctl stop` sends `SIGTERM`, and the agent shuts `fluxdownd` down with it), create the `fluxdown` system user and `/var/lib/fluxdown`, then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now fluxdown-server   # the unit name is whatever you saved the file as
sudo journalctl -u fluxdown-server -f
```

Then open `http://<host>:17800/` in a browser and complete the Initialize FluxDown Server wizard (or preset `FLUXDOWN_TOKEN` in the unit file for unattended setup).

## Upgrading from the legacy `fluxdown-server`

Earlier releases shipped a single `fluxdown-server` binary. Server releases (the `fluxdown-server` Docker image and the `FluxDown-Server-*` archives/NAS packages keep their names; they are now attached to the regular `vX.Y.Z` GitHub release, older versions live under `server-v*`) now ship `fluxdown-agent` + `fluxdownd` instead.

- **Same data, same settings**: keep the same data volume / `FLUXDOWN_DATA_DIR`; `FLUXDOWN_BIND`, `FLUXDOWN_DATA_DIR`, `FLUXDOWN_SAVE_DIR`, `FLUXDOWN_DATABASE_URL`, `FLUXDOWN_TOKEN`, `FLUXDOWN_TOKEN_FORCE`, `FLUXDOWN_WEBROOT`, `FLUXDOWN_LANG`, `FLUXDOWN_DEMO*` keep their names and meaning. Your existing access key carries over.
- **Change the start command**: `fluxdown-server` → `fluxdown-agent --server` (both binaries in one directory). The Docker image, Synology / QNAP / OpenWrt packages already do this for you — just upgrade in place.
- **Removed extension REST endpoints** (they only existed on the old server): `/api/v1/config`, queue create/update/delete and start/stop/schedule/order, `/api/v1/stats`, `/api/v1/fs/list`, components, webhooks, logs, and `/api/v1/token/regenerate`. Manage these from the built-in Web UI (which now talks JSON-RPC over `/rpc`) or call the JSON-RPC protocol directly. The core API — `/ping`, `/download`, `/jsonrpc` (aria2), `/mcp`, `/api/v1/info`, tasks, queue listing — is kept.
- `FLUXDOWN_SERVER_VERSION` is gone; the reported version is the agent's crate version.

## Next steps

- [Web UI](/docs/en/headless-server/web-ui/) — sign in and manage downloads from a browser.
- [API Overview](/docs/en/api/overview/) — automate the server from scripts or other tools.
