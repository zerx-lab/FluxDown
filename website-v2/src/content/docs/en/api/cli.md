---
title: Command-Line Client
description: The fluxdown CLI — an aria2c-style download client that drives the management API from your shell or scripts.
section: api
order: 2
---

FluxDown ships a command-line client, the `fluxdown` binary, modelled on `aria2c`. It works in two modes:

- **Remote mode (default)**: a thin, typed HTTP client over the [management API](/docs/en/api/overview/). Most commands talk to a running FluxDown desktop app or [headless server](/docs/en/headless-server/setup/) over `http://127.0.0.1:17800` (or wherever you point it), so it manages exactly the same tasks and queues you'd see in the app. In this role it's a scriptable remote control, the same part `aria2c`'s RPC client mode plays.
- **Standalone mode (`add --local`)**: fully independent of any running instance — it **embeds the download engine** inside the CLI process and downloads directly. This path never contacts a server and needs no token, ideal for headless environments or one-off scripted downloads. See [`add`](#add) and [Standalone mode](#standalone-mode---local) below.

The [`config`](#config) subcommand is also purely local file I/O and never contacts a server.

## Getting the binary

The CLI is a workspace crate (`native/cli`). Build it from source:

```bash
cargo build -p fluxdown_cli --release
# binary at target/release/fluxdown (fluxdown.exe on Windows)
```

Or run it straight through Cargo during development:

```bash
cargo run -p fluxdown_cli -- ping
```

## Configuration

Every command accepts these global options; the two that matter most also read environment variables, so you set them once:

| Option | Environment variable | Default | Meaning |
|---|---|---|---|
| `--url <URL>` | `FLUXDOWN_URL` | `http://127.0.0.1:17800` | Base address of the FluxDown instance. |
| `--token <TOKEN>` | `FLUXDOWN_TOKEN` | (none) | Management API token. Required for every command except `ping`. |
| `--timeout <SECS>` | — | `30` | Per-request timeout, in seconds. |
| `--json` | — | off | Emit machine-readable JSON instead of formatted text. |

Each of `url`, `token`, and `timeout` is resolved with the precedence **explicit flag > environment variable > persisted config file > built-in default**. The persisted layer is written by [`config set`](#config) (like `go env -w`), so you can save the token once instead of exporting it every time.

The token comes from your running instance: on the desktop app it's under **Settings → Local API service**; on the headless server it's the key you set yourself the first time you open the Web UI, and you can view or change it later under **Settings → Security & Access**. See [Authentication](/docs/en/api/overview/#authentication).

```bash
# Option A: export environment variables (per shell session)
export FLUXDOWN_TOKEN="fxd_your_token_here"
export FLUXDOWN_URL="http://127.0.0.1:17800"   # optional; this is the default
fluxdown list

# Option B: persist once, no exports needed afterwards
fluxdown config set token fxd_your_token_here
fluxdown list
```

The CLI always connects directly to the given address and never routes through a system proxy, so a local instance behind an `HTTP_PROXY` environment variable still works.

## Commands

| Command | Aliases | What it does |
|---|---|---|
| `ping` | — | Probe whether the instance is alive. No token needed. |
| `info` | — | Show the instance's app name and version. |
| `add <URL...>` | `get` | Create one or more download tasks. |
| `list` | `ls` | List tasks, optionally filtered by status. |
| `status <ID>` | `stat` | Show one task's full detail. |
| `pause <ID>` | — | Pause a task. |
| `resume <ID>` | — | Resume a paused task. |
| `rm <ID>` | — | Delete a task (optionally its file too). |
| `pause-all` | — | Pause every task. |
| `resume-all` | — | Resume every paused task. |
| `queue` | — | List named queues and their config. |
| `watch [ID]` | — | Poll and redraw progress until tasks reach a terminal state. |
| `config <SUB>` | — | Read/write persisted CLI config (`set`/`unset`/`get`/`list`/`path`). No server needed. |

### add

```bash
# Single URL, auto everything
fluxdown add https://example.com/file.zip

# Several URLs at once
fluxdown add https://example.com/a.zip https://example.com/b.zip

# Read URLs from a file (one per line; blank lines and lines starting with # are ignored)
fluxdown add -i urls.txt

# Read URLs from stdin
cat urls.txt | fluxdown add -i -
```

`add` options:

| Option | Meaning |
|---|---|
| `-i, --input-file <FILE>` | Read URLs from a file, one per line (`-` for stdin). Combines with URLs given on the command line. |
| `-d, --dir <DIR>` | Save directory (empty = the instance's default). |
| `-o, --out <NAME>` | Output file name. Only applied when adding a single URL. Path separators and `..` are stripped so the name always stays inside the save directory. |
| `-s, --segments <N>` | Segment/thread count (`0` = auto, decided by file size). |
| `--proxy <URL>` | Per-task proxy URL. |
| `-U, --user-agent <UA>` | User-Agent string. |
| `--referrer <URL>` | Referrer header. |
| `--cookies <STR>` | Cookie string. |
| `--queue <ID>` | Named queue id (empty = default queue). |
| `--checksum <SPEC>` | Checksum to verify, format `algo=hexhash` (e.g. `sha256=abc123...`). |
| `--local` | Standalone mode: don't contact a server, download directly via an embedded engine in this process (see [Standalone mode](#standalone-mode---local)). Only `add` supports this. |

On success it prints the new task id(s), one per line (`added <id>`), or a JSON array of ids with `--json`. When several URLs are given, each is attempted independently: a failure on one URL prints `failed to add <url>: ...` to stderr but does not abort the others or discard already-created tasks. The command exits `0` only if every URL succeeded; otherwise it reports the first failure's exit code after listing whatever was created.

#### Standalone mode (`--local`)

With `--local`, `add` no longer contacts any running instance. Instead it **embeds the download engine** (the same engine the desktop app/server use) inside the CLI process and downloads directly. This is the only way to run the CLI **fully offline** — no running app or server, and no token required.

```bash
# Fully offline download, independent of any running FluxDown instance
fluxdown add https://example.com/file.zip --local
```

Behavior:

- **One-shot, blocking**: the process creates the task(s), blocks until every download reaches a terminal state (completed/failed), then exits with a code reflecting the result.
- **Shared database**: it uses the same SQLite database under the shared data directory as the desktop app/server, so tasks added with `--local` are visible in the app too (provided both resolve to the same data directory — i.e. installed mode; portable mode may keep them separate).
- **Save-directory precedence**: `-d/--dir` > the default save directory configured in the shared database > the current working directory.
- **Unattended selection**: HLS picks the highest bitrate, BitTorrent/magnet downloads all files (no interactive prompts).
- **Ctrl-C semantics**: interrupting pauses unfinished tasks and exits with code `7`; you can resume later via the app/server.
- Only `add` supports `--local`; every other command always uses remote mode.

### list

```bash
fluxdown list
fluxdown list --status downloading
fluxdown --json list          # scriptable output
```

`--status` accepts a name or a numeric code: `pending`/`0`, `downloading`/`1`, `paused`/`2`, `completed`/`3`, `error`/`4`, `preparing`/`5`. The plain-text output is a table of id, status, progress, size, and name; `--json` emits the raw `TaskDto` array (camelCase fields — see the [API overview](/docs/en/api/overview/)).

### status, pause, resume, rm

```bash
fluxdown status 42a14870-9276-4ea2-84ea-eb75ae497766
fluxdown pause  42a14870-9276-4ea2-84ea-eb75ae497766
fluxdown resume 42a14870-9276-4ea2-84ea-eb75ae497766

# delete the task record only
fluxdown rm 42a14870-9276-4ea2-84ea-eb75ae497766
# delete the record AND the file on disk
fluxdown rm 42a14870-9276-4ea2-84ea-eb75ae497766 --delete-files
```

### pause-all, resume-all, queue

```bash
fluxdown pause-all
fluxdown resume-all
fluxdown queue           # list named queues
```

### watch

`watch` polls the instance and redraws a progress table (clearing the screen each tick) until every watched task reaches a terminal state (completed / error), then exits.

```bash
fluxdown watch                 # all active tasks
fluxdown watch <ID>            # one task
fluxdown watch --interval 2    # refresh every 2 seconds (default 1)
```

### config

`config` reads and writes local configuration so you don't have to export environment variables every time (think `go env -w`). It never contacts a server. Valid keys are `url`, `token`, `timeout`, and `log_level`.

```bash
fluxdown config set token fxd_your_token_here   # persist the token
fluxdown config set url http://192.168.1.10:17800
fluxdown config set timeout 60

fluxdown config get token        # print one value
fluxdown config list             # show every key (unset keys shown as "(unset)")
fluxdown --json config list      # machine-readable
fluxdown config unset token      # clear one value
fluxdown config path             # print the config file location
```

The file is `cli.toml` under the platform config directory (`%APPDATA%\zerx\fluxdown\config\` on Windows, `$XDG_CONFIG_HOME/fluxdown/` on Linux, `~/Library/Application Support/dev.zerx.fluxdown/` on macOS). The stored `token` is plain text; on Unix the file is created with `0600` permissions (owner-only). A value set here is overridden by an explicit `--flag` or the matching environment variable on any given invocation.

`log_level` is stored separately in the engine data directory's `logging.conf`. If that file is malformed, `config list` and `config get` without a key show `log_level` as `(invalid)` in both text and JSON, while still listing the other keys. `config get log_level` reports the error; `config set log_level auto` repairs the file and restores the default.

## Exit codes

The CLI mirrors `aria2c`'s exit-status convention, so scripts can branch on the failure category:

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | Unknown error. |
| `2` | Request timed out (or, from clap, no subcommand given). |
| `3` | Not found (a 404 — e.g. the task id doesn't exist). |
| `5` | Network error (couldn't connect — is FluxDown running?). |
| `7` | Interrupted with downloads still unfinished (Ctrl-C in `--local` mode). |
| `24` | Authentication failed (missing or invalid token). |
| `32` | Bad request (a 400, or invalid input such as an unknown status filter). |

## Size suffixes

Wherever the CLI displays sizes it uses 1024-based units (`KiB`, `MiB`, `GiB`). Any size you type accepts the `K`/`M`/`G`/`T` suffix (case-insensitive, optional trailing `B`), also 1024-based: `10M` is `10 × 1024 × 1024` bytes.

## Relationship to aria2 and the API

The CLI has two roles: remote mode drives the same management API, and standalone mode (`add --local`) embeds the engine to download offline. If you already have aria2-targeting tooling, the [aria2-compatible RPC endpoint](/docs/en/api/overview/#curl-examples) may be a better fit; for AI clients, use [MCP](/docs/en/api/overview/#mcp-model-context-protocol). Remote mode and those entry points operate on the same tasks and queues. Metalink, XML-RPC, and aria2's session save/restore are intentionally not implemented — FluxDown's SQLite store already persists everything across restarts.
