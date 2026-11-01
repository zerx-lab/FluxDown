// Builds dist/: zero-dependency manifest.mjs, library entries, the CLI, and declarations.
import * as esbuild from "esbuild";
import { chmodSync, cpSync, mkdirSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const dist = join(root, "dist");
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const common = { absWorkingDir: root, bundle: true, logLevel: "info", legalComments: "none" } as const;

// Zero-dependency validator: the plugin index vendors this single file from the published npm
// package. The first line names the exact release; the index's sync workflow reads it.
const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { version: string };
await esbuild.build({
  ...common,
  entryPoints: ["src/manifest.ts"],
  outfile: "dist/manifest.mjs",
  format: "esm",
  platform: "neutral",
  target: "es2020",
  banner: { js: `// fluxdown-plugin-sdk@${version} manifest validator (generated; do not edit)` },
});

const libraryEntries: Record<string, string> = {
  "dist/index": "src/index.ts",
  "dist/runtime": "src/runtime.ts",
  "dist/ui/jsx-runtime": "src/ui/jsx-runtime.ts",
};
await esbuild.build({
  ...common,
  entryPoints: libraryEntries,
  outdir: ".",
  outExtension: { ".js": ".js" },
  format: "esm",
  platform: "neutral",
  target: "es2022",
});

// The dev runtime must share Fragment with the production runtime, so keep the import external.
await esbuild.build({
  ...common,
  entryPoints: { "dist/ui/jsx-dev-runtime": "src/ui/jsx-dev-runtime.ts" },
  outdir: ".",
  format: "esm",
  platform: "neutral",
  target: "es2022",
  plugins: [
    {
      name: "external-jsx-runtime",
      setup(build) {
        build.onResolve({ filter: /^\.\/jsx-runtime$/ }, () => ({ path: "./jsx-runtime.js", external: true }));
      },
    },
  ],
});

await esbuild.build({
  ...common,
  entryPoints: ["src/cli.ts"],
  outfile: "dist/cli.js",
  format: "esm",
  platform: "node",
  target: "node20",
  packages: "external",
});
chmodSync(join(dist, "cli.js"), 0o755);

// Declarations (cli and tests excluded), then the hand-written / generated ambient types.
const tsc = Bun.spawnSync([process.execPath, join(root, "node_modules/typescript/bin/tsc"), "-p", "tsconfig.build.json"], {
  cwd: root,
  stdout: "inherit",
  stderr: "inherit",
});
if (tsc.exitCode !== 0) process.exit(tsc.exitCode ?? 1);

mkdirSync(join(dist, "types"), { recursive: true });
for (const file of readdirSync(join(root, "src/types"))) {
  cpSync(join(root, "src/types", file), join(dist, "types", file));
}
