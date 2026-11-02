#!/usr/bin/env node
// fluxdown-plugin: create / validate / build / pack / dev.
import * as esbuild from "esbuild";
import { zipSync, type Zippable } from "fflate";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  watch,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import {
  isSafeRelativePath,
  packageFilesOf,
  reviewOf,
  validatePackage,
} from "./manifest";

const SDK_NAME = "fluxdown-plugin-sdk";
const CONFIG_FILE = "fluxdown-plugin.config.json";

/** Modules the desktop (GPUI Shell) runtime provides; they stay as bare imports. */
const DESKTOP_EXTERNALS = [
  "gpui-kit", "gpui", "gpui-base", "gpui-component", "gpui-shell", "gpui-fps", "fluxdown",
  "buffer", "console", "crypto", "fs/promises", "net", "os", "path", "process", "url", "websocket", "zlib",
  "node:*",
];

interface PluginConfig {
  /** Classic scripts: `{ "main.js": "src/main.tsx" }` (output path in dist → source file). */
  entries: Record<string, string>;
  /** Desktop ESM scripts: `{ "ui/dashboard.js": "src/desktop/dashboard.ts" }`. */
  desktop: Record<string, string>;
  /** Extra files copied verbatim into dist. */
  assets: string[];
}

class CliError extends Error {}

const USAGE = `Usage: fluxdown-plugin <command> [options]

Commands:
  create <dir> [--identity author@name] [--name "Display Name"] [--sdk <path>]
                 Scaffold a plugin project (--sdk links a local SDK checkout).
  build [dir]    Bundle entries into dist/ and validate.
  validate [dir] Validate dist/ against the engine rules.
  pack [dir]     Build, then zip dist/ into <identity>-<version>.fxplug.
  dev [dir]      Watch sources and rebuild dist/ on change.
`;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function readJson(path: string): unknown {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (err) {
    throw new CliError(`${path}: ${(err as Error).message}`);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stringMap(value: unknown, where: string): Record<string, string> {
  if (value === undefined) return {};
  if (!isRecord(value) || !Object.values(value).every((v) => typeof v === "string")) {
    throw new CliError(`${CONFIG_FILE}: ${where} must be an object of string → string`);
  }
  for (const key of Object.keys(value)) {
    if (!isSafeRelativePath(key)) throw new CliError(`${CONFIG_FILE}: ${where} key '${key}' is not a safe relative path`);
  }
  return value as Record<string, string>;
}

function loadConfig(root: string): PluginConfig {
  const path = join(root, CONFIG_FILE);
  if (!existsSync(path)) return { entries: {}, desktop: {}, assets: [] };
  const raw = readJson(path);
  if (!isRecord(raw)) throw new CliError(`${CONFIG_FILE}: must be a JSON object`);
  for (const key of Object.keys(raw)) {
    if (!["entries", "desktop", "assets"].includes(key)) throw new CliError(`${CONFIG_FILE}: unknown key '${key}'`);
  }
  const assets = raw.assets ?? [];
  if (!Array.isArray(assets) || !assets.every((a) => typeof a === "string" && isSafeRelativePath(a))) {
    throw new CliError(`${CONFIG_FILE}: assets must be an array of safe relative paths`);
  }
  return {
    entries: stringMap(raw.entries, "entries"),
    desktop: stringMap(raw.desktop, "desktop"),
    assets: assets as string[],
  };
}

/** Recursively lists regular files under `dir` as forward-slash relative paths. */
function listFiles(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const full = join(current, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile()) out.push(relative(dir, full).split(sep).join("/"));
    }
  };
  walk(dir);
  return out.sort();
}

function manifestOf(root: string): Record<string, unknown> {
  const path = join(root, "manifest.json");
  if (!existsSync(path)) throw new CliError(`${path} not found (run inside a plugin project)`);
  const manifest = readJson(path);
  if (!isRecord(manifest)) throw new CliError("manifest.json must be a JSON object");
  return manifest;
}

/** Every package file a manifest names (entries, icon, sniff rules). */
function referencedFiles(manifest: Record<string, unknown>): string[] {
  const files = new Set(packageFilesOf(manifest));
  const entryOf = (value: unknown) => (isRecord(value) && typeof value.entry === "string" ? value.entry : null);
  for (const group of [manifest.resolvers, manifest.subscriptions]) {
    if (Array.isArray(group)) for (const item of group) { const e = entryOf(item); if (e) files.add(e); }
  }
  for (const single of [manifest.hooks, manifest.auth]) {
    const e = entryOf(single);
    if (e) files.add(e);
  }
  if (typeof manifest.icon === "string" && manifest.icon !== "") files.add(manifest.icon);
  return [...files].filter(isSafeRelativePath);
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

function baseOptions(root: string): esbuild.BuildOptions {
  return {
    absWorkingDir: root,
    bundle: true,
    platform: "browser",
    target: "es2022",
    jsx: "automatic",
    jsxImportSource: `${SDK_NAME}/ui`,
    logLevel: "silent",
    legalComments: "none",
  };
}

function buildOptionsList(root: string, config: PluginConfig, extraPlugins: esbuild.Plugin[]): esbuild.BuildOptions[] {
  const dist = join(root, "dist");
  const list: esbuild.BuildOptions[] = [];
  for (const [out, src] of Object.entries(config.entries)) {
    list.push({
      ...baseOptions(root),
      entryPoints: [src],
      outfile: join(dist, out),
      format: "iife",
      plugins: extraPlugins,
    });
  }
  for (const [out, src] of Object.entries(config.desktop)) {
    list.push({
      ...baseOptions(root),
      entryPoints: [src],
      outfile: join(dist, out),
      format: "esm",
      external: DESKTOP_EXTERNALS,
      plugins: extraPlugins,
    });
  }
  return list;
}

function formatBuildErrors(err: unknown): string {
  if (err !== null && typeof err === "object" && "errors" in err && Array.isArray(err.errors)) {
    const messages = esbuild.formatMessagesSync(err.errors, { kind: "error", color: false });
    return messages.join("\n");
  }
  return String(err);
}

/** Copies manifest + assets + unbuilt referenced files into dist/. */
function copyStatic(root: string, config: PluginConfig, manifest: Record<string, unknown>): void {
  const dist = join(root, "dist");
  mkdirSync(dist, { recursive: true });
  cpSync(join(root, "manifest.json"), join(dist, "manifest.json"));
  const built = new Set([...Object.keys(config.entries), ...Object.keys(config.desktop)]);
  const wanted = new Set([...config.assets, ...referencedFiles(manifest)]);
  for (const file of wanted) {
    if (built.has(file)) continue;
    const src = join(root, file);
    if (!existsSync(src)) continue; // reported by validate as a missing file
    mkdirSync(dirname(join(dist, file)), { recursive: true });
    cpSync(src, join(dist, file));
  }
}

async function build(root: string): Promise<void> {
  const manifest = manifestOf(root);
  const config = loadConfig(root);
  rmSync(join(root, "dist"), { recursive: true, force: true });
  for (const options of buildOptionsList(root, config, [])) {
    try {
      await esbuild.build(options);
    } catch (err) {
      throw new CliError(formatBuildErrors(err));
    }
  }
  copyStatic(root, config, manifest);
  const errors = validateDist(root);
  if (errors.length) throw new CliError(`validation failed:\n${errors.map((e) => `  - ${e}`).join("\n")}`);
  console.log(`built ${relative(process.cwd(), join(root, "dist")) || "dist"}`);
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

function validateDist(root: string): string[] {
  const dist = join(root, "dist");
  if (!existsSync(dist)) throw new CliError("dist/ not found; run `fluxdown-plugin build` first");
  const manifestPath = join(dist, "manifest.json");
  if (!existsSync(manifestPath)) return ["dist/manifest.json missing"];
  const manifest = readJson(manifestPath);
  const files = new Map(listFiles(dist).map((file) => [file, new Uint8Array(readFileSync(join(dist, file)))]));
  return validatePackage(manifest, files).errors;
}

function validate(root: string): void {
  const errors = validateDist(root);
  if (errors.length) throw new CliError(`validation failed:\n${errors.map((e) => `  - ${e}`).join("\n")}`);
  const review = reviewOf(readJson(join(root, "dist", "manifest.json")) as Record<string, unknown>);
  const hosts = review.networkHosts;
  console.log(`valid (risk: ${review.risk}${review.archiveEntry ? `, archive: ${review.archiveEntry}` : ""}${hosts.length ? `, network hosts: ${hosts.join(", ")}` : ""})`);
}

// ---------------------------------------------------------------------------
// pack
// ---------------------------------------------------------------------------

async function pack(root: string): Promise<string> {
  await build(root);
  const dist = join(root, "dist");
  const manifest = readJson(join(dist, "manifest.json")) as { identity: string; version: string };
  const zippable: Zippable = {};
  for (const file of listFiles(dist)) {
    // Fixed mtime keeps the archive reproducible.
    zippable[file] = [new Uint8Array(readFileSync(join(dist, file))), { level: 6, mtime: new Date(Date.UTC(2020, 0, 1)) }];
  }
  const out = join(root, `${manifest.identity}-${manifest.version}.fxplug`);
  writeFileSync(out, zipSync(zippable));
  console.log(`packed ${out}`);
  return out;
}

// ---------------------------------------------------------------------------
// dev
// ---------------------------------------------------------------------------

async function dev(root: string): Promise<void> {
  const manifest = manifestOf(root);
  const config = loadConfig(root);
  rmSync(join(root, "dist"), { recursive: true, force: true });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const finalize = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      try {
        const current = manifestOf(root);
        copyStatic(root, config, current);
        const errors = validateDist(root);
        console.log(errors.length ? `validation failed:\n${errors.map((e) => `  - ${e}`).join("\n")}` : "rebuilt, valid");
      } catch (err) {
        console.error(err instanceof Error ? err.message : err);
      }
    }, 100);
  };
  const reporter: esbuild.Plugin = {
    name: "fluxdown-dev",
    setup(b) {
      b.onEnd((result) => {
        if (result.errors.length) {
          console.error(esbuild.formatMessagesSync(result.errors, { kind: "error", color: false }).join("\n"));
        } else finalize();
      });
    },
  };
  const contexts = await Promise.all(
    buildOptionsList(root, config, [reporter]).map((options) => esbuild.context(options)),
  );
  await Promise.all(contexts.map((ctx) => ctx.watch()));
  copyStatic(root, config, manifest);
  for (const file of ["manifest.json", ...config.assets, ...referencedFiles(manifest)]) {
    const path = join(root, file);
    if (existsSync(path)) watch(path, finalize);
  }
  console.log("watching for changes (Ctrl+C to stop)");
  await new Promise<never>(() => {});
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

function sdkVersion(): string {
  const pkgPath = join(dirname(fileURLToPath(import.meta.url)), "..", "package.json");
  const pkg = readJson(pkgPath);
  return isRecord(pkg) && typeof pkg.version === "string" ? pkg.version : "0.1.0";
}

function slug(input: string): string {
  const s = input.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
  return s || "plugin";
}

function create(dir: string, flags: Record<string, string>): void {
  const root = resolve(dir);
  if (existsSync(root) && readdirSync(root).length > 0) throw new CliError(`${root} is not empty`);
  const name = flags.name ?? basename(root);
  const identity = flags.identity ?? `me@${slug(basename(root))}`;
  const sdkSpec = flags.sdk ? `file:${resolve(flags.sdk)}` : `^${sdkVersion()}`;
  const manifest = {
    identity,
    name,
    version: "0.1.0",
    description: "A FluxDown plugin built with fluxdown-plugin-sdk.",
    main: { entry: "main.js" },
    contributes: {
      views: [{ id: "dashboard", title: name.slice(0, 40), location: "activity" }],
      commands: [{ id: "hello", title: "Say hello", palette: true }],
    },
  };
  const config = { entries: { "main.js": "src/main.tsx" }, desktop: {}, assets: [] };
  const pkg = {
    name: slug(basename(root)),
    private: true,
    version: "0.1.0",
    type: "module",
    scripts: {
      build: "fluxdown-plugin build",
      validate: "fluxdown-plugin validate",
      pack: "fluxdown-plugin pack",
      dev: "fluxdown-plugin dev",
      typecheck: "tsc --noEmit",
    },
    dependencies: { [SDK_NAME]: sdkSpec },
    devDependencies: { typescript: "^5.7.0" },
  };
  const tsconfig = {
    compilerOptions: {
      target: "ES2022",
      module: "ESNext",
      moduleResolution: "bundler",
      lib: ["ES2022"],
      types: [],
      strict: true,
      noEmit: true,
      skipLibCheck: true,
      jsx: "react-jsx",
      jsxImportSource: `${SDK_NAME}/ui`,
    },
    include: ["src"],
  };
  const mainTsx = `import { defineMain } from "${SDK_NAME}/runtime";
import type { ViewEvent } from "${SDK_NAME}";

interface State {
  count: number;
}

defineMain((flux) => {
  flux.ui.view<State>("dashboard", {
    init: () => ({ count: 0 }),
    render: (state) => (
      <stack gap="md" padding="md">
        <heading text="Hello from FluxDown" />
        <text text={\`Clicked \${state.count} times\`} />
        <button key="inc" text="Click me" variant="primary" />
      </stack>
    ),
    event(state, ev: ViewEvent) {
      if (ev.type === "press" && ev.node === "root/inc") state.count += 1;
    },
  });

  flux.commands.register("hello", () => {
    flux.ui.toast("success", "Hello from ${name}!");
  });
});
`;
  const files: Record<string, string> = {
    "manifest.json": `${JSON.stringify(manifest, null, 2)}\n`,
    [CONFIG_FILE]: `${JSON.stringify(config, null, 2)}\n`,
    "package.json": `${JSON.stringify(pkg, null, 2)}\n`,
    "tsconfig.json": `${JSON.stringify(tsconfig, null, 2)}\n`,
    ".gitignore": "node_modules/\ndist/\n*.fxplug\n",
    "src/env.d.ts": `/// <reference types="${SDK_NAME}" />\n`,
    "src/main.tsx": mainTsx,
  };
  for (const [file, content] of Object.entries(files)) {
    const path = join(root, file);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, content);
  }
  const pm = detectPackageManager();
  console.log(`created ${root}\n\nNext:\n  cd ${dir}\n  ${pm} install\n  ${pm} run pack   # produces ${identity}-0.1.0.fxplug`);
}

/** Package manager that launched the CLI (`npm_config_user_agent`), npm by default. */
function detectPackageManager(): "npm" | "pnpm" | "yarn" | "bun" {
  const name = (process.env.npm_config_user_agent ?? "").split("/")[0];
  return name === "pnpm" || name === "yarn" || name === "bun" ? name : "npm";
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

function parseArgs(argv: string[]): { positional: string[]; flags: Record<string, string> } {
  const positional: string[] = [];
  const flags: Record<string, string> = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i] as string;
    if (arg.startsWith("--")) {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new CliError(`${arg} needs a value`);
      flags[arg.slice(2)] = value;
      i++;
    } else positional.push(arg);
  }
  return { positional, flags };
}

async function main(argv: string[]): Promise<void> {
  const [command, ...rest] = argv;
  const { positional, flags } = parseArgs(rest);
  const dir = positional[0] ?? ".";
  switch (command) {
    case "create":
      if (!positional[0]) throw new CliError("create needs a target directory");
      return create(positional[0], flags);
    case "build":
      return build(resolve(dir));
    case "validate":
      return validate(resolve(dir));
    case "pack":
      await pack(resolve(dir));
      return;
    case "dev":
      return dev(resolve(dir));
    default:
      console.log(USAGE);
      if (command !== undefined && command !== "help" && command !== "--help") process.exitCode = 1;
  }
}

main(process.argv.slice(2)).catch((err) => {
  console.error(err instanceof CliError ? err.message : err);
  process.exit(1);
});
