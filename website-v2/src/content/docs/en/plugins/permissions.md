---
title: Permissions, Consent & Audit
description: What each plugin permission grants, how risk levels and consent work (UI and headless), how the market checks packages, what is audited, and what full trust means.
section: plugins
order: 14
---

Plugins are not sandboxed to the point of uselessness: a plugin may ask for powerful capabilities. FluxDown's rule is that every capability is **declared** in the manifest, **shown to you and approved** before it works, and **recorded** in an audit log.

This page covers the user-facing model and the rules a plugin author needs to know. The manifest fields themselves are in the [Manifest reference](/docs/en/plugins/manifest/#permissions); the APIs behind each permission are in the [API reference](/docs/en/plugins/api-reference/).

## Permissions

| Permission | Grants |
|---|---|
| *(none)* | The base sandbox: `flux.fetch`, `flux.storage`, scratch files, logging, view / command contributions. |
| `ffmpeg` | `flux.ffmpeg` / `flux.ffprobe` on finished files. |
| `ytdlp` | `flux.ytdlp`. |
| `auth` | Platform login flows and stored credentials used by `flux.fetch` / `flux.auth`. |
| `tasks` | `flux.tasks.*`: list, create, pause, resume and delete download tasks. |
| `browserRules` | Contribute rules to the browser extension (`contributes.sniffRules`). |
| `background` | Keep the `main` script running with the daemon. |
| `intercept` | Interceptors: read and change the URL, request headers, cookies and body of matching tasks, control their connection count, skip the probe, swap credentials mid-download, rewrite or filter streaming segments, choose the quality or DASH Period, process the file before completion, and ask for a retry after a task fails. See the [Interceptors guide](/docs/en/plugins/interceptors-guide/). |
| `desktop` | Run native desktop scripts with **full trust** (see below). |
| `pageScripts` | Run the plugin's scripts on the websites it lists in `contributes.pageScripts`, through the FluxDown browser extension, with **Full** risk (see [Page scripts](#page-scripts-in-the-browser)). |
| `archive` | Let `contributes.archive` choose safe directories from the known filename and optional source metadata. New `directories` are relative to the authorized default directory; legacy `folderName` remains a category child. `contributes.archiveRules` supplies publisher-signed data, not executable code; no URLs, paths or credentials are exposed. An `archiveRules.updateUrl` hostname is included in network consent. The callback can read settings and write plugin logs, but cannot access files, network, tasks or processes. Risk: **Elevated**. |

Subscription providers, hooks, resolvers and portable views need no permission of their own.

## Risk levels

The risk level is computed from the permissions — it is not self-declared.

| Level | When | Shown as |
|---|---|---|
| **Standard** | No permissions. | — |
| **Elevated** | Any of `ffmpeg`, `ytdlp`, `auth`, `tasks`, `browserRules`, `background`, `intercept`, `archive`. | Warning badge |
| **Full** | `desktop` or `pageScripts`. | Destructive badge |

The badge appears on installed plugins and on market listings.

## Consent

A plugin runs only with the capabilities you have approved. The approval is stored per plugin as the exact set of permissions and network hosts you saw, together with the version.

- **Install or update.** When a plugin is installed (market, `.fxplug`, directory or dev install) or updated, and its permissions or its requested network hosts are not covered by what you approved before, it is installed but stays **disabled** with the reason *consent required* until you approve. In the desktop app a consent dialog shows the risk badge, each permission with what it allows, and the list of network hosts, and the plugin's row gets a **Grant** button.
- **Full trust.** For a **Full** plugin the dialog adds an explicit warning, and the confirm button is enabled only after you tick *I understand*.
- **Market installs.** The confirmation shown before a market install is the same dialog; confirming it there grants the same set, so you are not asked twice. The dialog records the version it showed; if the market's latest version has changed in the meantime, the install is refused so you can review the new version's permissions first.
- **Updates ask again only for what is new.** A new version that adds a permission or a network host needs your approval; one that does not changes nothing.
- **Upgrading FluxDown.** Plugins installed before consent existed keep working: the permissions they had at that point are recorded as approved (marked as a migration in the audit log). Anything new they request later needs approval, and plugins you install afterwards are never pre-approved.
- **Dev installs are not exempt.** A dev-mode plugin that declares permissions also waits for approval before it runs.

Approving is exact: the approved set must match what the plugin currently requests, no more and no fewer.

### Consent without a UI (headless and scripts)

On a headless server, or in any setup without the desktop dialog, approve through the service RPC. Call `daemon.plugin.consent` on the agent/daemon `/rpc` endpoint (see the [API overview](/docs/en/api/overview/)) with the plugin's identity and the **complete** lists it currently requests:

```json
{
  "identity": "acme@demo",
  "permissions": ["background", "tasks"],
  "networkHosts": []
}
```

- `permissions` is the plugin's whole `permissions` array; `networkHosts` is the union of `main.imageHosts`, every view's `desktop.networkHosts`, and any declared `archiveRules.updateUrl` hostname. Order and duplicates do not matter; hosts are compared exactly (case-sensitive).
- If either list differs from what the manifest requests, the call fails with `consent does not match requested capabilities`. Read the plugin's current request from `daemon.plugin.list` (`consentPending`, `consentPendingHosts`) first.
- Until consent is complete, enabling the plugin (`daemon.plugin.setEnabled` with `true`, or `PUT /api/v1/plugins/{identity}/enabled`) fails with `consent required`.

After a successful call the plugin loads and enables normally. The grant is written to the audit log as `consent.granted`.

## How the market checks a package

The plugin index entry carries the permissions and network hosts that the market UI shows you before installing. FluxDown re-checks the downloaded package against that entry before installing it:

- the package manifest's `identity` and `version` must **equal** the index entry's;
- every permission and every network host in the package manifest must be **contained in** the index entry's lists — a package that asks for more than the entry advertised is refused. (Contained, not equal: that is the market's anti-escalation check. The consent check above is stricter and requires an exact match.)

The index itself is checked automatically on every release. **The first registration of a plugin is reviewed by a maintainer; later releases of an already registered plugin are not reviewed by hand** — they pass or fail the automatic admission checks. Permissions are always shown to you before you install. See [Packaging & Plugin Market](/docs/en/plugins/packaging/#publishing-to-the-plugin-index).

## Full trust: desktop scripts

A view's `desktop` script runs inside the desktop app process, as an ES module under the GPUI Shell. It is **not sandboxed** with respect to your machine: it can read and write files, start processes and use the clipboard with your user's privileges.

Its network access is the exception. `networkHosts` is an **enforced allow-list**: the script can connect only to the exact hostnames listed (no wildcards), and the list is shown in the consent dialog. Hosts not listed are refused. File system, process and clipboard access are not restricted, and `process.exit` is never granted.

Install a plugin with `desktop` only if you trust its author the way you would trust any program you download and run. For the author's side — how to write these scripts and what the module offers — see the [Desktop UI guide](/docs/en/plugins/desktop-ui-guide/).

**Native file, process and network operations performed by desktop scripts are not logged one by one.** The audit log records that a desktop script was loaded, unloaded, and every `fluxdown.call` / `fluxdown.invoke` it made — the coarse boundary where it touched FluxDown — but nothing about what it did on its own with the operating system.

Desktop scripts do not run in the daemon, on Web or on mobile.

## Page scripts in the browser

A plugin with `pageScripts` makes the FluxDown browser extension run the plugin's JavaScript on the websites its manifest lists. Such a script can read and change everything the page shows and everything the signed-in user can do on that site, so the permission is **Full** risk and the consent dialog (desktop and Web) lists each script's title and the sites it matches.

Two confirmations are needed, once per plugin:

1. **In FluxDown**: approve the permission in the consent dialog described above (a Full plugin needs *I understand* ticked).
2. **In the browser extension**: the first time you enable a plugin's page scripts in the extension's options page, a dialog states the risk and asks you to confirm.

After that, new scripts, new code and a changed match list from the same plugin take effect without further prompts. Disabling or uninstalling the plugin, or leaving a newly requested permission unapproved, removes its scripts from the browser. The script code is always fetched from the FluxDown desktop app; the extension keeps no copy of its own. See [Page scripts](/docs/en/plugins/page-scripts-guide/) for the bridge, the limits and the safety notes.

## Failure circuit breaker

A plugin whose calls time out or exceed their memory limit three times in a row is disabled automatically (the audit log records `breaker`). Installing a new version of the plugin clears the tripped state. See [Debugging](/docs/en/plugins/debugging/) for how to tell a tripped breaker from other disabled states.

## Audit log

Open **Settings → Extensions → Plugins → Audit log**, or **Audit record** in a plugin's details for that plugin only. Each row has a time, an origin (`daemon` or `desktop`), an action, a target, an outcome (`ok`, `denied`, `error`) and detail. You can filter by plugin, load older rows, copy the log as JSON, and clear it (with confirmation).

The same data is available over RPC: `daemon.plugin.audit.list` (paged, optional identity filter) and `daemon.plugin.audit.clear`.

| Action | When |
|---|---|
| `install`, `update`, `uninstall`, `enable`, `disable` | Lifecycle changes. |
| `consent.granted`, `consent.migrated` | You approved capabilities; or the upgrade recorded existing ones. |
| `revoked` | The market revoked the plugin. |
| `breaker` | The plugin was disabled by the failure circuit breaker. |
| `fetch` | `flux.fetch`; the target is `METHOD scheme://host/path` — the query string and credentials are removed — and the detail is the status. |
| `auth.save`, `auth.remove` | Credential changes. |
| `fs.write`, `fs.remove` | Writes to the plugin's scratch workspace. |
| `ffmpeg.run`, `ffprobe.run`, `ytdlp.run` | Tool runs; the target is the argument list (shortened). |
| `task.retry`, `task.artifact` | Task requests from hooks. |
| `intercept.<stage>` | An interceptor call for that stage (`prepare`, `segments`, `finalize`, …); the target is the task id, the detail is empty or the error text. |
| `tasks.create`, `tasks.control` | `flux.tasks` calls from `main`. Task creation is rate limited; a refused call is recorded with outcome `denied`. |
| `settings.update` | `flux.settings.update`. |
| `desktop.load`, `desktop.unload` | A desktop script started / stopped; the detail lists the grants it ran with. |
| `desktop.call`, `desktop.invoke` | RPCs made from a desktop script. |
| `audit.clear` | The log was cleared. |

Details:

- A row's target is cut to 512 bytes and its detail to 2048 bytes.
- `desktop.*` rows are reported by the desktop client through `daemon.plugin.audit.record`; the origin is always forced to `desktop`, at most 100 rows per call.
- Reads, `flux.storage` and logging are not recorded. Rows older than 90 days are removed, and the log keeps at most the latest 20,000 rows. Nothing is written while plugins are idle.
