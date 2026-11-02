---
title: Plugin SDK & CLI
description: Reference for fluxdown-plugin-sdk — the create/build/validate/pack/dev CLI, the project config, runtime helpers, JSX runtime, shipped types and the manifest validator API.
section: plugins
order: 13
---

`fluxdown-plugin-sdk` is the toolchain for writing FluxDown plugins in TypeScript. It gives you typed `flux.*` APIs, a JSX runtime for [portable views](/docs/en/plugins/views/), the same manifest validator the engine and the plugin index use, and a CLI that bundles, validates and packs a `.fxplug`.

This page is a reference. For a step-by-step walkthrough from an empty folder to a packed plugin, read [TypeScript project](/docs/en/plugins/typescript-project/). You do not need the SDK at all — a plain folder of `.js` files works too (see [Your first plugin](/docs/en/plugins/your-first-plugin/)).

| | |
|---|---|
| npm package | `fluxdown-plugin-sdk` |
| CLI binary | `fluxdown-plugin` |
| Requires | Node.js 20 or newer (`engines.node` is `>=20`); `bun` works as a package manager too |
| Licence | AGPL-3.0-or-later |

`npx fluxdown-plugin-sdk create …` runs the CLI of the package without installing it. Inside a generated project the CLI is on the `npm run` scripts path as `fluxdown-plugin`.

## CLI

All commands share one calling convention: `fluxdown-plugin <command> [dir] [--flag value]…`. `dir` defaults to the current directory. Every `--flag` takes exactly one value (`--sdk` without a value fails with `--sdk needs a value`). Running with no command, `help` or `--help` prints the usage; an unknown command prints it and exits with code 1. Any failure prints its message and exits with code 1.

| Command | What it does |
|---|---|
| `fluxdown-plugin create <dir> [--identity a@b] [--name "Name"] [--sdk <path>]` | Scaffolds a project, see [`create`](#create). |
| `fluxdown-plugin build [dir]` | Bundles every entry into `dist/`, copies the manifest and assets, then validates, see [`build`](#build). |
| `fluxdown-plugin validate [dir]` | Validates an existing `dist/` with the engine rules; prints the risk level and network hosts. Does **not** build. |
| `fluxdown-plugin pack [dir]` | Runs `build`, then zips `dist/` into `<identity>-<version>.fxplug` in the project root. |
| `fluxdown-plugin dev [dir]` | Watches sources and rebuilds `dist/`. |

### `create`

`fluxdown-plugin create <dir>` fails with `<absolute path> is not empty` when the target directory exists and contains anything; a missing directory is created. Missing `<dir>` fails with `create needs a target directory`.

| Option | Default | Effect |
|---|---|---|
| `--identity` | `me@` + a slug of the directory name | Becomes `identity` in `manifest.json`. The slug lowercases the name and replaces every run of characters outside `a-z0-9_-` with `-`, trimming leading and trailing `-`; an empty result becomes `plugin`. The option value is not validated — a bad identity is caught by `build`. |
| `--name` | the directory name | Becomes `name` in `manifest.json`. The generated view is titled with the first 40 characters of it (the title limit). |
| `--sdk <path>` | the published version | The dependency is written as `file:<absolute path>` (a local SDK checkout) instead of `^<sdk version>`. Meant for SDK development. |

The generated files:

| File | Content |
|---|---|
| `manifest.json` | `version` `0.1.0`; description `A FluxDown plugin built with fluxdown-plugin-sdk.`; `main: { "entry": "main.js" }`; one `activity` view `dashboard` titled with the plugin name; one palette command `hello` titled `Say hello`. No `permissions`. |
| `fluxdown-plugin.config.json` | `{ "entries": { "main.js": "src/main.tsx" }, "desktop": {}, "assets": [] }` |
| `package.json` | `name` = slug of the directory, `private: true`, `version` `0.1.0`, `type: "module"`; dependency `fluxdown-plugin-sdk`; dev dependency `typescript` `^5.7.0`; scripts below |
| `tsconfig.json` | see [tsconfig](#tsconfig-and-envdts) |
| `.gitignore` | `node_modules/`, `dist/`, `*.fxplug` |
| `src/env.d.ts` | `/// <reference types="fluxdown-plugin-sdk" />` |
| `src/main.tsx` | a JSX counter view `dashboard` plus the `hello` command handler |

`package.json` scripts: `build` = `fluxdown-plugin build`, `validate` = `fluxdown-plugin validate`, `pack` = `fluxdown-plugin pack`, `dev` = `fluxdown-plugin dev`, `typecheck` = `tsc --noEmit`.

The project has **no resolver, hook or settings** to begin with — those are added by editing `manifest.json`, the config and `src/`, as shown in [TypeScript project](/docs/en/plugins/typescript-project/). A project grown that way looks like:

```
my-plugin/
  manifest.json                    your manifest (the source of truth, copied to dist/)
  fluxdown-plugin.config.json      what to bundle
  package.json
  tsconfig.json
  src/
    env.d.ts                       brings the global `flux` into scope
    main.tsx                       resident main script (views, commands)
    resolve.ts                     resolver entry        ← added by you
    hooks.ts                       hooks entry           ← added by you
  dist/                            build output (git-ignored): what gets installed and zipped
```

### `build`

1. Reads `manifest.json` (must exist and be a JSON object) and `fluxdown-plugin.config.json`.
2. **Deletes `dist/`.**
3. Bundles every `entries` and `desktop` item with esbuild (one build per entry; the first error aborts with the formatted esbuild message).
4. Copies `manifest.json`, every `assets` file, and every file named by the manifest that is not built (see [config](#fluxdown-pluginconfigjson)).
5. Validates `dist/` in strict mode (see [limits](#limits-and-validation)). On failure it prints `validation failed:` followed by one `  - <error>` line per problem and exits 1; on success it prints `built dist`.

Because step 2 removes the directory, a dev plugin registered from `dist/` loses its files for the duration of a rebuild. Press **Reload** after a build to re-read everything (see [Debugging](/docs/en/plugins/debugging/#the-dev-loop)).

### `validate`

Fails with `dist/ not found; run \`fluxdown-plugin build\` first` when there is no `dist/`. Otherwise it applies the checks in [limits and validation](#limits-and-validation) to `dist/` and prints, on success, `valid (risk: <standard|elevated|full>)`, with `, network hosts: a, b` appended when views declare `desktop.networkHosts`.

### `pack`

Runs `build`, then writes every file of `dist/` into a zip at `<project>/<identity>-<version>.fxplug` — for example `me@my-plugin-0.1.0.fxplug`. The manifest is at the archive root. The archive is deterministic: files are added in sorted order with a fixed modification time (2020-01-01 UTC) and deflate level 6, so the same sources produce the same bytes. It prints `packed <absolute path>`.

### `dev`

Deletes `dist/` once at start, then keeps esbuild in watch mode for every entry. After each successful rebuild (debounced by 100 ms) it re-copies `manifest.json`, assets and referenced files and validates again, printing `rebuilt, valid` or the `validation failed:` list; a bundling error prints the esbuild message and leaves the last good output in place. Edits to `manifest.json`, `assets` and referenced files trigger the same step. The config file is read once when `dev` starts — restart it after changing `fluxdown-plugin.config.json`. Stop with Ctrl+C.

`dev` only rebuilds files; it does not tell FluxDown anything. Resolver, hooks, auth and subscription scripts of a dev plugin are re-read on every call, so rebuilding is enough for them. For the manifest or `main` press **Reload** on the plugin row.

## `fluxdown-plugin.config.json`

```json
{
  "entries": { "main.js": "src/main.tsx", "resolve.js": "src/resolve.ts" },
  "desktop": { "ui/dashboard.js": "src/desktop/dashboard.ts" },
  "assets": ["rules.json"]
}
```

All three keys are optional (a missing file behaves like all-empty, which builds nothing and only copies the manifest). Any other key fails with `fluxdown-plugin.config.json: unknown key '<key>'`; a non-object file fails with `fluxdown-plugin.config.json: must be a JSON object`. `entries` and `desktop` must be objects of `output path → source path`; every output path must be a safe relative path (no `..`, absolute path, drive letter or empty segment); `assets` must be an array of safe relative paths.

| Key | Meaning |
|---|---|
| `entries` | Classic scripts, `output path in dist → source file`. Bundled with esbuild as a single **IIFE**, `bundle: true`, platform `browser`, target ES2022, with the automatic JSX transform and `jsxImportSource` set to `fluxdown-plugin-sdk/ui`. No `import` / `export` survives, because the engine evaluates the files as classic scripts. Use for `main`, resolvers, hooks, `auth` and subscriptions. The output path must equal the manifest `entry`. |
| `desktop` | Desktop UI scripts (`contributes.views[].desktop.entry`), bundled as **ES modules**. The host modules `gpui-kit`, `gpui`, `gpui-base`, `gpui-component`, `gpui-shell`, `gpui-fps`, `fluxdown`, the Node-style built-ins the desktop runtime provides (`buffer`, `console`, `crypto`, `fs/promises`, `net`, `os`, `path`, `process`, `url`, `websocket`, `zlib`) and `node:*` specifiers stay as bare imports. See the [Desktop UI guide](/docs/en/plugins/desktop-ui-guide/). |
| `assets` | Extra **files** copied verbatim into `dist/` at the same relative path (for example a sniff rule pack or an icon). |

Besides `assets`, `build` automatically copies these manifest-named files from the project root when they are not produced by a build entry: the resolver, hooks, auth, subscription, `main` and `desktop` entries, `icon` and `contributes.sniffRules`. This is what lets a project mix hand-written `.js` files with bundled ones. A named file that does not exist is not copied; `validate` then reports it as missing.

### tsconfig and `env.d.ts`

The generated `tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022"],
    "types": [],
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "jsx": "react-jsx",
    "jsxImportSource": "fluxdown-plugin-sdk/ui"
  },
  "include": ["src"]
}
```

`src/env.d.ts` (`/// <reference types="fluxdown-plugin-sdk" />`) brings the global `flux` and `console` into scope. Do not enable the DOM lib: the QuickJS host has no DOM, no `URL`, no `setTimeout` — the typecheck then catches code that would fail at runtime. `tsc` only type-checks (`noEmit`); esbuild produces the output.

## Limits and validation

`build`, `pack` and `validate` all run the same checks on `dist/`:

| Check | Failure message |
|---|---|
| `dist/manifest.json` exists | `dist/manifest.json missing` |
| Manifest passes `validateManifest(…, { strict: true })` | one line per problem, in Chinese like the engine's (see [Debugging](/docs/en/plugins/debugging/#install-and-validation-errors)) |
| At most **200 files** in `dist/` | `package has N files; the limit is 200` |
| At most **50 MB** (52,428,800 bytes) in total | `package is N bytes; the limit is 52428800` |
| A declared `contributes.sniffRules` file is a valid pack (≤ 256 KiB, `format` `fluxdown-sniff-pack`, ≤ 500 rules) | the `validateSniffPack` message |

The engine applies the same 200-entry and 50 MB (uncompressed) limits when it installs a zip. The plugin market additionally caps packages at 10 MB; see [Packaging & market](/docs/en/plugins/packaging/).

Strict mode (always on in the CLI) is stricter than the app in two ways, which the plugin index also enforces: `version` and `minAppVersion` must be a plain three-part number `MAJOR.MINOR.PATCH` (no `v` prefix, no pre-release or build suffix, no leading zeros), and every resolver, hooks, auth and subscription entry and the `icon` must exist in the package. (At install time the app only requires the `main` entry, `desktop` entries, view icons, the `sniffRules` file, page script files and the `contributes.archive` file.)

## Entry-point helpers

The engine reads fixed globals after evaluating a script: `resolve`, `onStart` / `onError` / `onDone` / `onMetaProbed` / `onCancel`, `authenticate`, `subscribe`, `intercept` and `archive`. The helpers in `fluxdown-plugin-sdk/runtime` assign your functions to those globals (with `Reflect.set(globalThis, …)`), so each entry file can be an ordinary module. Each helper returns the function(s) you passed.

| Helper | Installs | Manifest field that makes the engine call it |
|---|---|---|
| `defineResolver(fn: ResolveFn)` | `globalThis.resolve` | `resolvers[].entry` |
| `defineHooks(handlers: HookHandlers)` | every function-valued property of `handlers` as a global with the same name: `onStart`, `onError`, `onDone`, `onMetaProbed`, `onCancel` | `hooks.entry`; which events fire is set by `hooks.events` |
| `defineInterceptors(handlers: InterceptHandlers)` | `globalThis.intercept`, dispatching on `ctx.stage` to the same-named handler in `handlers` (`prepare`, `probed`, `refresh`, `selectVariant`, `selectPeriod`, `segments`, `liveSegments`, `finalize`, `failed`); returns `undefined` when there is no handler | `interceptors.entry` (needs the `intercept` permission; which stages fire is set by `interceptors.stages`) |
| `defineAuth(fn: AuthFn)` | `globalThis.authenticate` | `auth.entry` (needs the `auth` permission) |
| `defineSubscription(fn: SubscribeFn)` | `globalThis.subscribe` | `subscriptions[].entry` |
| `defineArchive(fn: ArchiveFn)` | `globalThis.archive`; `fn` takes `ctx: ArchiveContext` (`{ fileName }`) and returns `ArchiveResult` (`{ folderName }`), `null` or `undefined`, sync or async | `contributes.archive` (needs the `archive` permission); the script only has `flux.settings`, `flux.logger` and `console` |
| `defineMain(setup: (flux: MainFlux) => void)` | nothing — it calls `setup(flux)` **immediately and synchronously** with the `main`-script facade (`MainFlux`), so views, commands, rpc handlers, events and timers are registered while the script is evaluated | `main.entry` |

```ts
import { defineResolver, defineHooks } from "fluxdown-plugin-sdk/runtime";

defineResolver(async (ctx) => {
  const res = await flux.fetch({ url: ctx.url });
  return { url: JSON.parse(res.body).direct };
});

defineHooks({
  onDone: (ev) => flux.logger.info("finished", ev.filePath),
});
```

Notes:

- Only call helpers at the top level of the script, and nothing else there should touch `flux` (install-time checks evaluate resolver, hooks, auth and subscription scripts in a bare context). A plugin has at most one resolver and one subscription provider; calling a helper twice simply overwrites the global.
- `defineMain` is the typed way to register in the resident `main` script, as the generated template does. `ui`, `commands`, `rpc`, `events`, `timers` and `tasks` exist only in `main`, so the global `flux` type (shared by all entry scripts) does not declare them; `MainFlux` does (`tasks` is optional: it needs the `tasks` permission). Without `defineMain` you would have to cast the global to `MainFlux` yourself.
- Import runtime values from `fluxdown-plugin-sdk/runtime`, and only **types** from the package root (`import type { ViewEvent } from "fluxdown-plugin-sdk"`, which is erased at build time). The root module also re-exports the manifest validator and the runtime helpers; keeping value imports on the dedicated sub-paths keeps your bundles small and predictable.

## JSX

With `jsxImportSource` set as above, every lowercase tag is a [view node](/docs/en/plugins/views/#node-reference), fully typed from the same schema the daemon validates against:

```tsx
import { defineMain } from "fluxdown-plugin-sdk/runtime";

defineMain((flux) => {
  flux.ui.view("dashboard", {
    render: () => (
      <stack gap="md" padding="md">
        <heading text="Hello" />
        <button key="go" text="Go" variant="primary" />
      </stack>
    ),
    event: (_state, ev) => { if (ev.node === "root/go") flux.ui.toast("info", "Clicked"); },
  });
});
```

| Rule | Detail |
|---|---|
| Output | exactly the `{k, p, c}` shape `flux.ui.h` returns |
| `key` | becomes `p.key` (it is not a regular prop) and decides the node id |
| Dropped children | `null`, `false` and `undefined`; nested arrays are flattened |
| `undefined` props | omitted from `p` |
| **No string or number children** | The child type is `ViewNode`, so `<stack>hello</stack>` fails to typecheck, and at runtime a string child is rejected by the view validator with `a node must be an object {k, p, c}`. Put text in a node: `<text text="hello" />`. |
| Fragment | `<>…</>` splices its children into the parent. It cannot be the **root** of a view: `render` must return a real node. |
| Function components | `function Row(props) { return <stack>…</stack>; }` works: it receives the props (including `children` and `key`) and must return a node. |
| Also exported | `jsxs` (same as `jsx`) and `jsx-dev-runtime` for `"jsx": "react-jsxdev"` |

## Types shipped

- `flux` and its sub-APIs (`Flux`, `FluxFetchOptions`, …), the entry contexts and results (`ResolveContext`, `ResolveResult`, `AuthContext`, `AuthResult`, `SubscribeContext`, `ArchiveContext`, `ArchiveResult`, `HookHandlers`, …), `ViewEvent`, `ViewContext`, `ViewDefinition`, `FluxEvent`, and all view types and intrinsic elements — from `fluxdown-plugin-sdk`.
- `ThemeTokenPath` — every theme token path that `fluxdown.theme().tokens` resolves, generated from the theme registry.
- `fluxdown-plugin-sdk/types/desktop` — declarations for desktop UI scripts (`gpui-kit` and friends), generated from the desktop runtime.

## Package exports

| Import | Contents |
|---|---|
| `fluxdown-plugin-sdk` | Types (`flux`, view nodes, entry contexts, theme tokens), plus re-exports of the manifest API and runtime helpers — import **types only** from here in plugin code. Importing it brings the global `flux` / `console` declarations into scope. |
| `fluxdown-plugin-sdk/manifest` | Zero-dependency validator, see below. |
| `fluxdown-plugin-sdk/runtime` | `defineResolver`, `defineHooks`, `defineAuth`, `defineSubscription`, `defineMain`. |
| `fluxdown-plugin-sdk/ui/jsx-runtime`, `…/ui/jsx-dev-runtime` | JSX runtime; set `"jsxImportSource": "fluxdown-plugin-sdk/ui"`. |
| `fluxdown-plugin-sdk/types/desktop` | Types only: declarations for desktop UI scripts. |

## Validator API

The manifest validator is a zero-dependency module (the plugin index vendors the same file):

```ts
import { validateManifest, riskOf, networkHostsOf, validateSniffPack } from "fluxdown-plugin-sdk/manifest";

const manifest = JSON.parse(await readFile("manifest.json", "utf8"));
const files = new Set(["manifest.json", "main.js", "resolve.js"]); // forward-slash paths in the package

const { ok, errors } = validateManifest(manifest, files, { strict: true });
riskOf(manifest);          // "standard" | "elevated" | "full"
networkHostsOf(manifest);  // sorted union of every view's desktop.networkHosts
```

| Export | Signature / value |
|---|---|
| `validateManifest(manifest, files, options?)` | `manifest: unknown`, `files?: Set<string>` (package-relative paths with `/`; when omitted, file-existence checks are skipped), `options.strict?: boolean` (strict mode needs `files`; without it you get a single error asking for it, not a crash). Returns `{ ok: boolean; errors: string[] }` with **every** error found. Messages are in Chinese, mirroring the engine. |
| `validateSniffPack(content)` | `content: string \| Uint8Array`; returns `{ ok, errors }`. |
| `riskOf(manifest)` | `"full"` when `permissions` contains `desktop` or `pageScripts`; `"elevated"` for any of `ffmpeg`, `ytdlp`, `auth`, `tasks`, `browserRules`, `background`, `intercept`, `archive`; otherwise `"standard"`. |
| `networkHostsOf(manifest)` | sorted, de-duplicated union of `contributes.views[].desktop.networkHosts`. |
| `packageFilesOf(manifest)` | the files that must exist at install: `main.entry`, each `desktop.entry`, each view `icon`, `sniffRules`, every `pageScripts[].js` file and `contributes.archive`. |
| `isSafeRelativePath`, `isValidIdentity`, `isValidNetworkHost` | the predicates the validator uses. |
| `VALID_PERMISSIONS`, `VALID_EVENTS`, `VIEW_LOCATIONS` | the closed value lists. |
| `WINDOW_SIZE_MIN`/`MAX` (240 / 4096), `NETWORK_HOSTS_MAX` (64), `VIEWS_MAX` (16), `COMMANDS_MAX` (32), `TITLE_MAX_CHARS` (40), `SNIFF_PACK_MAX_BYTES` (256 KiB), `SNIFF_PACK_MAX_RULES` (500), `SNIFF_PACK_FORMAT` | limits used by the checks. |

The validator follows the same rules as the engine, checked against a shared set of test cases. One difference in wording: for an unknown manifest field the SDK reports `manifest: 未知字段 <key>`, while the engine, which rejects unknown fields while parsing the JSON, reports `JSON 解析失败: unknown field …`.
