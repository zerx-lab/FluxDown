---
title: Packaging & Plugin Market
description: Build a .fxplug, check it locally, understand install/update/uninstall, publish to the plugin index end to end, and manage plugins over REST/RPC.
section: plugins
order: 15
---

This page takes a finished plugin from source folder to a package users can install, and from there to the official plugin market. For writing the plugin itself start with [Your first plugin](/docs/en/plugins/your-first-plugin/) or [TypeScript project](/docs/en/plugins/typescript-project/); every manifest rule is in the [Manifest reference](/docs/en/plugins/manifest/).

## The `.fxplug` format

A `.fxplug` file is a **zip of the plugin folder** with a different extension. Both layouts work:

```
my-plugin.fxplug          my-plugin.fxplug
├── manifest.json         └── my-plugin/          ← single wrapper folder
└── resolver.js               ├── manifest.json      is stripped automatically
                               └── resolver.js
```

`manifest.json` must be at the zip root, **or** the zip must contain exactly one top-level folder, with nothing else beside it, that holds `manifest.json`. Anything else is rejected with `zip 内未找到根 manifest.json` (“root manifest.json not found in the zip”).

> **macOS pitfall.** Finder's *Compress* command adds a `__MACOSX/` folder (and often `.DS_Store` files) next to your folder. A zip with `my-plugin/` **and** `__MACOSX/` at the top level has two top-level folders, so it is not treated as a single wrapper and fails with the error above. A stray file beside the folder fails the same way. Create the archive from the command line or with the SDK instead.

### Build the package

**Recommended: the SDK.** In a project created with `fluxdown-plugin create`:

```sh
npm run pack        # or: bun run pack
```

`pack` bundles the entries into `dist/`, validates the result with the strict rules, and writes `<identity>-<version>.fxplug` (for example `acme@demo-1.0.0.fxplug`) with the manifest at the zip root. See the [SDK reference](/docs/en/plugins/sdk/) and the end-to-end [TypeScript project tutorial](/docs/en/plugins/typescript-project/).

**Manually**, for a hand-written plugin folder. Zip the folder's *contents*, not the folder itself:

```sh
cd my-plugin
zip -r -X -D ../acme-demo-1.0.0.fxplug . -x "*.DS_Store" -x "__MACOSX/*"
```

- `.` puts `manifest.json` at the zip root.
- `-D` omits directory entries, `-X` omits extra file attributes, and the `-x` patterns keep macOS metadata out.
- Include only what the manifest references (entries, icon, `rules.json`, assets); do not zip `node_modules`, `.git` or source maps you do not need.

On Windows, select the files **inside** the folder (not the folder), then *Send to → Compressed (zipped) folder*, and rename the result to `.fxplug`.

### Limits enforced at install

Installers apply these whatever the source of the zip:

| Check | Limit |
|---|---|
| Package size | ≤ 10 MiB when downloaded from the market or the index; the REST upload endpoint accepts a request body of at most 4 MB |
| Zip entries | ≤ 200 |
| Total uncompressed size | ≤ 50 MB (measured on the bytes actually extracted, not the sizes declared in the zip) |
| Entry paths | Must stay inside the target folder — `..` and absolute paths are rejected (zip-slip protection) |
| Manifest | Parsed and fully validated (see [Manifest reference](/docs/en/plugins/manifest/#validation)) |
| Files | `main.entry`, every `desktop.entry` and the `sniffRules` file must exist |
| Scripts | Resolver, hooks, auth and subscription entries are compile-checked; a syntax error aborts the install |
| Settings | Every `pattern` must compile as a JavaScript RegExp |
| Load | The plugin must actually load afterwards — this includes the `minAppVersion` gate |

The plugin is extracted to `<data dir>/plugins/<identity>/`. A directory install (dev tooling) does not strip a wrapper folder: the path you give must contain `manifest.json` directly.

## Check locally before you publish

Run these in order; each catches problems the next one would only report after a release.

1. **Validate the manifest and package.** With the SDK: `fluxdown-plugin validate` (also run by `pack`). It applies the engine rules plus the market's stricter ones: strict `MAJOR.MINOR.PATCH` version and `minAppVersion`, every resolver / hooks / auth / subscription entry and the icon present in the package, the 200-entry and 50 MB limits, and the sniff-rule pack content check.
2. **Install it for real.** Load the folder as a dev plugin, then install the packed `.fxplug` through *Settings → Extensions → Plugins* (or `POST /api/v1/plugins/install`, see below) on a clean profile to prove the package, not only the folder, works. Walk through consent and run the plugin's features once. If something fails, see [Debugging](/docs/en/plugins/debugging/).
3. **Dry-run the index admission** (optional but recommended). The index repository ships the same admission code the bot runs. Clone it and use Node 20 or later, no dependencies:

   ```sh
   git clone https://github.com/zerx-lab/fluxdown-plugin-index
   cd fluxdown-plugin-index
   # add your entry to registry.json first (see "Publishing to the plugin index"), then:
   GITHUB_TOKEN=<token> node scripts/check-registry.mjs --head registry.json
   ```

   `check-registry.mjs` downloads your repository's **Latest Release** and runs the full admission against it, so a published GitHub Release must already exist. `node scripts/validate.mjs` and `node --test scripts/index.test.mjs scripts/stage.test.mjs` check the index repository itself (shards, hashes, sequence) and are mainly for people running their own index.

## Install, update and uninstall

**Installing** the same identity again replaces the previous version in place. What happens to your data:

| | On update | On uninstall |
|---|---|---|
| Install folder | Replaced; a failed update restores the old version | Deleted (a dev plugin's source folder is never deleted — only its registration) |
| Settings, `flux.storage` values, enabled/disabled state | **Kept** | Deleted (everything stored under the plugin's `plugin.<identity>.` configuration prefix) |
| Consent | Kept; only newly requested permissions or hosts need approval | Deleted with the rest |
| Circuit breaker | Reset — a plugin the breaker had disabled is re-enabled by installing a new version | — |
| Manually disabled | Stays disabled | — |
| Market revocation | Cleared when the installed version differs from the revoked one | — |
| Stored credentials | Kept | Deleted |
| Task bindings | Kept | Tasks that were bound to the plugin are switched to "ignore the plugin and use the original link" so they can still resume |
| Scratch files (`flux.fs`) and remembered site defaults | Kept | Deleted |

An install is all-or-nothing: if any check fails, nothing changes on disk. For rules about when a new version asks for consent again, see [Permissions](/docs/en/plugins/permissions/#consent).

## The plugin market

FluxDown's market is **a data format, not a service** — an index the app reads directly, with no backend and no account. The pieces:

- **An index** — a Git-versioned JSON catalog of plugins, versions and download mirrors. The default index lives at [`zerx-lab/fluxdown-plugin-index`](https://github.com/zerx-lab/fluxdown-plugin-index) and is fetched from two default sources (the raw GitHub file and a jsDelivr mirror; the first that works wins). Anyone can fork it and run their own, and users can point the app at other sources through the `market_index_sources` setting (comma-separated URLs).
- **Author-owned repositories** — each plugin lives in its author's own GitHub repository. The index only records which repository publishes which plugin identity; the repository's **Latest Release** is ingested automatically.
- **Content addressing** — every published version records `contentHash = sha256(<the .fxplug file>)`. After downloading from any mirror, FluxDown recomputes the hash and rejects a mismatch. A compromised mirror cannot swap the payload, and a published version's bytes never change.
- **Manifest match** — before installing, FluxDown checks that the package's `identity` and `version` equal the index entry and that its permissions and network hosts are contained in the entry's, so a package can never take more than the market showed you (see [Permissions](/docs/en/plugins/permissions/#how-the-market-checks-a-package)).
- **Rollback protection** — each index carries a monotonically increasing `sequence`; the app remembers the highest value seen per index and refuses an older one.

v1 has no author-level cryptographic signatures — integrity rests on content addressing plus TLS plus Git history. The index schema reserves signature fields so they can be added later without breaking existing clients.

## Publishing to the plugin index

The whole path, from a finished package to a listing in the market. You register the plugin **once**; after that every version is a GitHub Release.

### Before you start

- A **public** GitHub repository that holds the plugin. **One repository hosts one plugin.**
- A manifest whose `identity` is the permanent ID you want (`author@name`). It can never change afterwards.
- A package that passes `fluxdown-plugin validate` and installs on a clean profile.

### 1. Publish a GitHub Release

1. Set `version` in `manifest.json` to a strict `MAJOR.MINOR.PATCH` (digits only, no leading zeros, no `v` prefix, no suffix). The first version can be anything, for example `1.0.0`; every later one must be **strictly higher than every version already in the index**.
2. Create a GitHub Release — not a draft, not a pre-release — and attach **exactly one** `.fxplug` file. Its tag is yours to choose, but it must be the repository's *Latest* release (the one GitHub marks "Latest").
3. Recommended: enable GitHub's [immutable releases](https://github.blog/changelog/2025-10-28-immutable-releases-are-now-generally-available) so published assets cannot be swapped afterwards.

### 2. Register once with a pull request

1. Fork [`zerx-lab/fluxdown-plugin-index`](https://github.com/zerx-lab/fluxdown-plugin-index).
2. Edit only [`registry.json`](https://github.com/zerx-lab/fluxdown-plugin-index/blob/main/registry.json) and append one entry to the `plugins` array:

   ```json
   { "id": "author@name", "repo": "owner/repo", "tags": ["resolver", "video"] }
   ```

   | Field | Rules |
   |---|---|
   | `id` | Must equal the manifest `identity`. |
   | `repo` | `owner/repo` of your public repository. One repository publishes one plugin. |
   | `tags` | Optional; at most 10 lowercase tags, shown in the market. |

3. Open the pull request. The **registry-check** bot downloads your Latest Release, runs the full admission and comments with the result, the permissions you declare and the URL patterns your plugin intercepts.
4. If the check fails, fix the release (publish a new one, or replace the asset on a release that is not yet ingested), then push any commit to the PR to run the check again — or run the `registry-check` workflow manually and enter the PR number.
5. A maintainer reviews and merges the PR. **The first registration is reviewed by a person**; this is the only manual step. The Stage job then publishes your plugin within minutes.

### 3. What the admission checks

The PR check and the hourly Stage job apply the same rules:

| Check | Rule |
|---|---|
| Release | Latest Release of the registered repository, exactly one `.fxplug` asset, served from that repository's own `releases/download` URL |
| Bytes | ≤ 10 MiB; sha256 equals GitHub's asset digest when GitHub provides one |
| Zip | ≤ 200 entries, ≤ 50 MiB unpacked; no `..`, absolute or backslash paths, no symlinks, no encrypted entries, no ZIP64, no duplicate entries; only *stored* or *deflate* compression; CRC verified; `manifest.json` at the root or inside one wrapper folder |
| Manifest | The same schema as the client; additionally every `entry` and `icon` file must exist in the package |
| Setting localization (market only) | New publications and updates require complete, non-blank `localizations.en` and `localizations.zh` for every setting title and option label, plus any custom description/helper label |
| Identity | Manifest `identity` equals the registered `id` |
| Version | Strict `MAJOR.MINOR.PATCH`, strictly greater than every published version; byte-identical bytes only add the release as another mirror of that version |
| Immutability | If a release asset that was already ingested changes its digest, it is reported; the published version keeps its original bytes |

This is a publication policy, **not a client installation or loading requirement**. Existing market versions and byte-identical mirror additions keep working without translations. The market recognizes historical packages by the trusted published identity/version/hash, not an author-supplied legacy flag. The rule is enforced both during automatic Stage and against new shards in mirror CI; historical re-admission does not apply it. Plugins with no manifest settings have no setting labels to translate; custom script views still need to implement their own language-aware rendering.


### 4. Publish updates

Publish a new GitHub Release with a higher `version`. No pull request is needed. The Stage job runs **every hour** (and whenever `registry.json` changes) and ingests the repository's *Latest* release at that moment: hash it, unpack it, validate it, add a shard and republish `index.json`. Only the Latest Release is looked at, so do not rely on an intermediate release being picked up if you publish several within the hour.

If a release cannot be ingested (wrong identity, version not increased, malformed manifest, missing entry file…), an issue labelled `stage-fail` is opened in the index repository. It is closed automatically once a later run succeeds, so fix the problem and publish a corrected release — never reuse a version number.

Updates are fully automatic and not reviewed by hand. Permissions are always shown to users before they install, and a version that adds permissions asks users to consent again.

### Maintainer operations

These are performed through pull requests or by index maintainers:

- **Yank a version.** A maintainer sets the version's `yanked` to `deprecated`, `vulnerable` or `malicious` in its shard and regenerates the index. Yanked versions are skipped by "install latest"; `vulnerable` and `malicious` additionally disable already-installed copies. A disabled copy stays disabled until the user installs a version different from the revoked one, so publish a fixed release with a higher version.
- **Delist a plugin.** Remove its entry from `registry.json`. Stage stops ingesting it and closes its `stage-fail` issue; versions that must disappear from installs are yanked separately.
- **Change maintainer.** Open a pull request that re-points `repo` for an existing `id` to the new repository. The bot flags it and the previous maintainer must confirm in the PR. The new repository's version must still be higher than the published ones.

## Release checklist

1. `identity` is final, `version` is strict three-part and higher than the last release.
2. `minAppVersion` is raised if you use a field, event or permission older versions do not know.
3. `permissions` lists exactly what the code uses, and nothing more; every `networkHosts` entry is needed.
4. `fluxdown-plugin validate` passes; the package is under 10 MiB, 200 entries and 50 MB unpacked.
5. The `.fxplug` has `manifest.json` at its root (no `__MACOSX/`, no stray files).
6. You installed the package on a clean profile, approved consent, and exercised each feature.
7. The GitHub Release is not a draft or pre-release, is marked Latest and has exactly one `.fxplug` asset.
8. After merging (first time) or publishing (updates), check the market within the hour, and watch for a `stage-fail` issue.

## Management endpoints

Besides the settings page, plugins can be managed programmatically. The REST routes belong to the management API group and require the management token — see the [API overview](/docs/en/api/overview/). The same operations exist as RPC methods on the agent/daemon `/rpc` endpoint.

| REST | RPC method | Purpose |
|---|---|---|
| `GET /api/v1/plugins` | `daemon.plugin.list` | List installed plugins with load status and consent state. |
| `POST /api/v1/plugins/install` (body: the `.fxplug` bytes, request body ≤ 4 MB) | `daemon.plugin.install` | Install from a package. |
| `POST /api/v1/plugins/install-dev` with `{ "dirPath": "<absolute path on the daemon host>" }` | `daemon.plugin.installDev` | Register a folder as a dev plugin (not copied). |
| — | `daemon.plugin.reloadDev` | Re-read a dev plugin's manifest and sources and re-run the install checks; a failure keeps the registration. |
| `PUT /api/v1/plugins/{identity}/enabled` with `{ "enabled": true }` | `daemon.plugin.setEnabled` | Enable or disable; enabling fails with `consent required` while approvals are pending. |
| `PUT /api/v1/plugins/{identity}/settings` | `daemon.plugin.updateSettings` | Update setting values. |
| `POST /api/v1/plugins/{identity}/auth` | `daemon.plugin.auth` | Drive the login flow (`begin` / `poll` / `cancel` / `logout` / `status`). |
| `DELETE /api/v1/plugins/{identity}` | `daemon.plugin.uninstall` | Uninstall. |
| `GET /api/v1/market` | `daemon.plugin.marketList` | The merged market index. |
| `POST /api/v1/market/install` with `{ "pluginId": "author@name" }` | `daemon.plugin.marketInstall` | Install the latest non-yanked version; the RPC also accepts `version` to pin the version the user confirmed (the install is refused if the latest version differs). |
| — | `daemon.plugin.consent` | Grant the plugin's requested permissions and hosts (see [Permissions](/docs/en/plugins/permissions/#consent-without-a-ui-headless-and-scripts)). |
| — | `daemon.plugin.audit.list`, `.clear`, `.record` | Read, clear and report audit rows. |
| — | `agent.plugin.installFile` | The agent reads a local package file and installs it through the daemon. |
