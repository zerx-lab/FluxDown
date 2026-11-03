import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { unzipSync } from "fflate";
import { validateManifest } from "./manifest";

const cli = fileURLToPath(new URL("./cli.ts", import.meta.url));
const temps: string[] = [];
afterAll(() => {
  for (const dir of temps) rmSync(dir, { recursive: true, force: true });
});

function run(...args: string[]) {
  const proc = Bun.spawnSync([process.execPath, cli, ...args], { stdout: "pipe", stderr: "pipe" });
  return { code: proc.exitCode, out: proc.stdout.toString(), err: proc.stderr.toString() };
}

function project(manifest: unknown, files: Record<string, string>): string {
  const dir = mkdtempSync(join(tmpdir(), "fxplug-test-"));
  temps.push(dir);
  writeFileSync(join(dir, "manifest.json"), JSON.stringify(manifest));
  for (const [path, content] of Object.entries(files)) {
    mkdirSync(join(dir, path, ".."), { recursive: true });
    writeFileSync(join(dir, path), content);
  }
  return dir;
}

describe("fluxdown-plugin cli", () => {
  test("pack bundles entries, copies assets and zips a valid package", () => {
    const dir = project(
      {
        identity: "t@pack",
        name: "Pack",
        version: "1.2.3",
        permissions: ["browserRules"],
        main: { entry: "main.js" },
        contributes: { commands: [{ id: "go", title: "Go" }], sniffRules: "rules.json" },
      },
      {
        "fluxdown-plugin.config.json": JSON.stringify({ entries: { "main.js": "src/main.ts" }, assets: ["rules.json"] }),
        "src/main.ts": `import { double } from "./lib";\nflux.commands.register("go", () => { flux.logger.info(String(double(2))); });\n`,
        "src/lib.ts": "export const double = (n: number) => n * 2;\n",
        "rules.json": JSON.stringify({ format: "fluxdown-sniff-pack", rules: [] }),
      },
    );
    const result = run("pack", dir);
    expect(result.code, result.err).toBe(0);
    const archive = join(dir, "t@pack-1.2.3.fxplug");
    expect(existsSync(archive)).toBe(true);
    const entries = unzipSync(new Uint8Array(readFileSync(archive)));
    expect(Object.keys(entries).sort()).toEqual(["main.js", "manifest.json", "rules.json"]);
    const main = new TextDecoder().decode(entries["main.js"]);
    expect(main).not.toMatch(/^\s*(import|export)\s/m);
    const manifest = JSON.parse(new TextDecoder().decode(entries["manifest.json"]));
    expect(validateManifest(manifest, new Set(Object.keys(entries)), { strict: true }).ok).toBe(true);
  }, 30_000);

  test("validate fails on a manifest the engine would reject", () => {
    const dir = project(
      { identity: "t@bad", name: "Bad", version: "1.0.0", main: { entry: "main.js" }, contributes: { views: [{ id: "Bad", title: "T", location: "activity" }] } },
      { "fluxdown-plugin.config.json": JSON.stringify({ entries: { "main.js": "src/main.ts" } }), "src/main.ts": "export {};\n" },
    );
    const result = run("build", dir);
    expect(result.code).toBe(1);
  }, 30_000);

  test("desktop entries stay ESM with bare imports for host modules", () => {
    const dir = project(
      {
        identity: "t@desk",
        name: "Desk",
        version: "1.0.0",
        permissions: ["desktop"],
        contributes: { views: [{ id: "w", title: "W", location: "window", desktop: { entry: "ui/w.js" } }] },
      },
      {
        "fluxdown-plugin.config.json": JSON.stringify({ desktop: { "ui/w.js": "src/w.ts" } }),
        "src/w.ts": `import { fluxdown } from "fluxdown";\nexport default function view() { return fluxdown; }\n`,
      },
    );
    const result = run("build", dir);
    expect(result.code, result.err).toBe(0);
    const out = readFileSync(join(dir, "dist/ui/w.js"), "utf8");
    expect(out).toContain('from "fluxdown"');
    expect(out).toContain("export");
  }, 30_000);
});
