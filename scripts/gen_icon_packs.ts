#!/usr/bin/env bun
/**
 * gen_icon_packs.ts — 生成内置文件图标包（`assets/icon-packs/*.json`）。
 *
 * 用法：
 *     bun scripts/gen_icon_packs.ts [lucide|material|catppuccin|thunder ...]   # 不带参数 = 全部
 *
 * 内置包与用户图标包同一份格式（`fluxdown.icon-pack` v1，解析见 `crates/icon_pack` 与
 * `website-v2/src/lib/icon-pack`），只是编译期嵌入：
 *
 *   lucide.json      简约线性：`crates/components/assets/icons` 的 Lucide 子集，单色 mask，跟随主题文字色
 *   material.json    Material Icon Theme（VS Code 安装量第一，MIT）：npm `material-icon-theme@MATERIAL_VERSION`
 *   catppuccin.json  Catppuccin Icons（MIT）：GitHub `catppuccin/vscode-icons@CATPPUCCIN_TAG`，暗色 mocha / 亮色 latte
 *   thunder.json     迅雷文件图标：`assets/icon-packs/thunder/*.png`（72×72），每个 PNG 内嵌为彩色 SVG
 *
 * 上游图标集有上千个图标，下载管理器只挑下载场景常见的文件类型（`*_ICONS` 列表），并从上游
 * 自带的关联表里取指向这些图标的全部扩展名 / 文件名。升级上游：改版本号后重跑，检查 diff。
 */

import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { $ } from "bun";

const ROOT = resolve(import.meta.dir, "..");
const OUT_DIR = join(ROOT, "assets/icon-packs");
const LUCIDE_DIR = join(ROOT, "crates/components/assets/icons");
const THUNDER_DIR = join(OUT_DIR, "thunder");

const MATERIAL_VERSION = "5.39.0";
const CATPPUCCIN_TAG = "v1.26.0";
const CATPPUCCIN_RAW = `https://raw.githubusercontent.com/catppuccin/vscode-icons/${CATPPUCCIN_TAG}`;

const FORMAT = "fluxdown.icon-pack";
const SCHEMA_VERSION = 1;

/** 文件大类（与 `kinds.json` / `FileKind` 一致）。 */
const KINDS = ["application", "diskImage", "mobile", "video", "audio", "document", "image", "archive", "other"] as const;
type Kind = (typeof KINDS)[number];

interface IconEntry {
  mode?: "mask" | "color";
  svg: string;
  light?: string;
  dark?: string;
}

interface Pack {
  format: string;
  schemaVersion: number;
  meta: Record<string, string>;
  extends?: string;
  icons: Record<string, IconEntry>;
  fileNames?: Record<string, string>;
  fileExtensions?: Record<string, string>;
  kinds: Partial<Record<Kind, string>>;
  default: string;
}

/** 去掉注释 / XML 声明与标签间空白；不改动属性与路径数据。 */
function minifySvg(text: string): string {
  return text
    .replace(/<\?xml[^>]*\?>/g, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/>\s+</g, "><")
    .replace(/\s+/g, " ")
    .trim();
}

/** 与运行时 `svg_is_safe` / `svgIsSafe` 相同的拒收规则：生成期就拦下，内置包零诊断。 */
function assertSafe(name: string, svg: string): void {
  const lower = svg.toLowerCase();
  const bad =
    !lower.startsWith("<svg") ||
    /<script|<foreignobject|<iframe|<object|<embed|@import|javascript:/.test(lower) ||
    /(?:xlink:)?href\s*=\s*["'](?!#|data:image\/png;base64,)/.test(lower) ||
    /url\(\s*["']?(?!#)/.test(lower) ||
    /\son[a-z]+\s*=/.test(lower);
  if (bad) throw new Error(`unsafe svg rejected: ${name}`);
}

function sortRecord<T>(record: Record<string, T>): Record<string, T> {
  return Object.fromEntries(Object.entries(record).sort(([a], [b]) => a.localeCompare(b)));
}

async function writePack(file: string, pack: Pack): Promise<void> {
  for (const [name, icon] of Object.entries(pack.icons)) {
    for (const svg of [icon.svg, icon.light, icon.dark]) if (svg) assertSafe(`${file}:${name}`, svg);
  }
  const referenced = [
    ...Object.values(pack.fileNames ?? {}),
    ...Object.values(pack.fileExtensions ?? {}),
    ...Object.values(pack.kinds),
    pack.default,
  ];
  for (const name of referenced) {
    if (!(name in pack.icons)) throw new Error(`${file}: mapping references undefined icon ${name}`);
  }
  const text = `${JSON.stringify(pack, null, 1)}\n`;
  await writeFile(join(OUT_DIR, file), text);
  console.log(`${file}: ${Object.keys(pack.icons).length} icons, ${Object.keys(pack.fileExtensions ?? {}).length} extensions, ${text.length} bytes`);
}

// ─────────────────────────── Lucide（简约线性）───────────────────────────

const LUCIDE_KINDS: Record<Kind, string> = {
  application: "app-window",
  diskImage: "disc-3",
  mobile: "smartphone",
  video: "file-play",
  audio: "file-music",
  document: "file-text",
  image: "file-image",
  archive: "file-archive",
  other: "file",
};

async function lucidePack(): Promise<Pack> {
  const icons: Record<string, IconEntry> = {};
  for (const name of new Set(Object.values(LUCIDE_KINDS))) {
    const svg = minifySvg(await readFile(join(LUCIDE_DIR, `${name}.svg`), "utf8"));
    icons[name] = { mode: "mask", svg };
  }
  return {
    format: FORMAT,
    schemaVersion: SCHEMA_VERSION,
    meta: {
      id: "lucide",
      name: "Lucide",
      author: "Lucide Contributors",
      license: "ISC",
      version: "1.48.0",
      homepage: "https://lucide.dev",
    },
    icons: sortRecord(icons),
    kinds: LUCIDE_KINDS,
    default: "file",
  };
}

// ─────────────────────────── Material Icon Theme ───────────────────────────

const MATERIAL_ICONS = [
  "file", "video", "audio", "image", "svg", "pdf", "word", "table", "powerpoint", "document", "markdown",
  "epub", "subtitles", "lyric", "font", "3d", "zip", "exe", "android", "disc", "jar", "hex", "console",
  "certificate", "key", "lock", "log", "database", "settings", "html", "css", "javascript", "typescript",
  "json", "xml", "yaml", "toml", "python", "java", "c", "cpp", "go", "rust", "php", "csharp", "ruby",
  "kotlin", "swift", "lua", "powershell", "git",
];

const MATERIAL_KINDS: Record<Kind, string> = {
  application: "exe",
  diskImage: "disc",
  mobile: "exe",
  video: "video",
  audio: "audio",
  document: "document",
  image: "image",
  archive: "zip",
  other: "file",
};

interface MaterialManifest {
  iconDefinitions: Record<string, { iconPath: string }>;
  file: string;
  fileExtensions: Record<string, string>;
  fileNames: Record<string, string>;
  light?: { fileExtensions?: Record<string, string>; fileNames?: Record<string, string> };
}

async function materialPack(work: string): Promise<Pack> {
  const tarball = join(work, "material.tgz");
  const response = await fetch(`https://registry.npmjs.org/material-icon-theme/-/material-icon-theme-${MATERIAL_VERSION}.tgz`);
  if (!response.ok) throw new Error(`material-icon-theme download failed: ${response.status}`);
  await writeFile(tarball, new Uint8Array(await response.arrayBuffer()));
  await $`tar xzf ${tarball} -C ${work}`.quiet();
  const root = join(work, "package");
  await writeFile(join(OUT_DIR, "LICENSE-material.txt"), await readFile(join(root, "LICENSE"), "utf8"));
  const manifest = JSON.parse(await readFile(join(root, "dist/material-icons.json"), "utf8")) as MaterialManifest;
  const wanted = new Set(MATERIAL_ICONS);
  const svgOf = async (definition: string): Promise<string> => {
    const path = manifest.iconDefinitions[definition]?.iconPath;
    if (!path) throw new Error(`material: missing icon definition ${definition}`);
    return minifySvg(await readFile(join(root, "dist", path), "utf8"));
  };
  const icons: Record<string, IconEntry> = {};
  for (const name of MATERIAL_ICONS) {
    const entry: IconEntry = { svg: await svgOf(name) };
    if (manifest.iconDefinitions[`${name}_light`]) entry.light = await svgOf(`${name}_light`);
    icons[name] = entry;
  }
  const [fileNames, fileExtensions] = [manifest.fileNames, manifest.fileExtensions].map((record) =>
    sortRecord(Object.fromEntries(Object.entries(record).filter(([, icon]) => wanted.has(icon)).map(([key, icon]) => [key.toLowerCase(), icon]))),
  );
  return {
    format: FORMAT,
    schemaVersion: SCHEMA_VERSION,
    meta: {
      id: "material",
      name: "Material",
      author: "Material Extensions",
      license: "MIT",
      version: MATERIAL_VERSION,
      homepage: "https://github.com/material-extensions/vscode-material-icon-theme",
    },
    icons,
    fileNames,
    fileExtensions,
    kinds: MATERIAL_KINDS,
    default: "file",
  };
}

// ─────────────────────────── Catppuccin Icons ───────────────────────────

const CATPPUCCIN_ICONS = [
  "_file", "video", "audio", "image", "svg", "pdf", "ms-word", "ms-excel", "ms-powerpoint", "text", "markdown",
  "font", "3d", "zip", "exe", "android", "apple", "binary", "bash", "certificate", "key", "log", "database",
  "config", "csv", "html", "css", "javascript", "typescript", "json", "xml", "yaml", "toml", "python", "java",
  "c", "cpp", "go", "rust", "php", "csharp", "ruby", "kotlin", "swift", "lua", "powershell", "git",
];

const CATPPUCCIN_KINDS: Record<Kind, string> = {
  application: "exe",
  diskImage: "binary",
  mobile: "android",
  video: "video",
  audio: "audio",
  document: "text",
  image: "image",
  archive: "zip",
  other: "_file",
};

async function fetchText(url: string): Promise<string> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: ${response.status}`);
  return response.text();
}

async function catppuccinPack(work: string): Promise<Pack> {
  await writeFile(join(OUT_DIR, "LICENSE-catppuccin.txt"), await fetchText(`${CATPPUCCIN_RAW}/LICENSE`));
  const source = join(work, "catppuccin-fileIcons.ts");
  await writeFile(source, await fetchText(`${CATPPUCCIN_RAW}/src/defaults/fileIcons.ts`));
  // 上游关联表按固定 tag 现下载到临时目录再执行：模块路径运行期才确定，无法静态 import。
  const { fileIcons } = (await import(source)) as {
    fileIcons: Record<string, { fileExtensions?: string[]; fileNames?: string[] }>;
  };
  const wanted = new Set(CATPPUCCIN_ICONS);
  const icons: Record<string, IconEntry> = {};
  for (const name of CATPPUCCIN_ICONS) {
    const [dark, light] = await Promise.all([
      fetchText(`${CATPPUCCIN_RAW}/icons/mocha/${name}.svg`),
      fetchText(`${CATPPUCCIN_RAW}/icons/latte/${name}.svg`),
    ]);
    icons[name] = { svg: minifySvg(dark), light: minifySvg(light) };
  }
  const fileExtensions: Record<string, string> = {};
  const fileNames: Record<string, string> = {};
  for (const [icon, association] of Object.entries(fileIcons)) {
    if (!wanted.has(icon)) continue;
    for (const extension of association.fileExtensions ?? []) fileExtensions[extension.toLowerCase()] = icon;
    for (const name of association.fileNames ?? []) fileNames[name.toLowerCase()] = icon;
  }
  return {
    format: FORMAT,
    schemaVersion: SCHEMA_VERSION,
    meta: {
      id: "catppuccin",
      name: "Catppuccin",
      author: "Catppuccin",
      license: "MIT",
      version: CATPPUCCIN_TAG.replace(/^v/, ""),
      homepage: "https://github.com/catppuccin/vscode-icons",
    },
    icons,
    fileNames: sortRecord(fileNames),
    fileExtensions: sortRecord(fileExtensions),
    kinds: CATPPUCCIN_KINDS,
    default: "_file",
  };
}

// ─────────────────────────── 迅雷 ───────────────────────────

const THUNDER_KINDS: Record<Kind, string> = {
  application: "exe",
  diskImage: "iso",
  mobile: "apk",
  video: "video",
  audio: "music",
  document: "document",
  image: "image",
  archive: "zip",
  other: "unknown",
};

/** 图标 → 扩展名（大类图标之外的具体类型）。 */
const THUNDER_EXTENSIONS: Record<string, string[]> = {
  apk: ["apk", "apks", "xapk"],
  ipa: ["ipa"],
  ipsw: ["ipsw"],
  bt: ["torrent"],
  bookchm: ["chm"],
  bookepub: ["epub"],
  bookmobi: ["mobi", "azw", "azw3"],
  captions: ["srt", "ass", "ssa", "vtt", "sub", "lrc"],
  dll: ["dll", "sys", "so", "dylib"],
  dmg: ["dmg"],
  document: ["rtf", "odt"],
  excel: ["xls", "xlsx", "xlsm", "csv", "ods"],
  exe: ["exe", "msi", "appimage", "bat", "cmd"],
  gif: ["gif"],
  html: ["html", "htm", "xhtml", "mht", "mhtml"],
  iso: ["iso", "img"],
  key: ["key"],
  numbers: ["numbers"],
  pages: ["pages"],
  pdf: ["pdf"],
  pkg: ["pkg", "mpkg", "deb", "rpm"],
  ppt: ["ppt", "pptx", "pps", "ppsx", "odp"],
  text: ["txt", "md", "log", "ini", "nfo", "cfg"],
  url: ["url", "webloc"],
  videom3u: ["m3u"],
  videom3u8: ["m3u8"],
  word: ["doc", "docx", "dot", "dotx", "wps"],
};

async function thunderPack(): Promise<Pack> {
  const names = (await readdir(THUNDER_DIR)).filter((file) => file.endsWith(".png")).map((file) => file.slice(0, -4)).sort();
  const icons: Record<string, IconEntry> = {};
  for (const name of names) {
    const base64 = (await readFile(join(THUNDER_DIR, `${name}.png`))).toString("base64");
    icons[name] = {
      mode: "color",
      svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 72 72"><image width="72" height="72" href="data:image/png;base64,${base64}"/></svg>`,
    };
  }
  const fileExtensions: Record<string, string> = {};
  for (const [icon, extensions] of Object.entries(THUNDER_EXTENSIONS)) {
    for (const extension of extensions) {
      if (extension in fileExtensions) throw new Error(`thunder: duplicate extension ${extension}`);
      fileExtensions[extension] = icon;
    }
  }
  return {
    format: FORMAT,
    schemaVersion: SCHEMA_VERSION,
    meta: {
      id: "thunder",
      name: "Thunder",
      author: "Xunlei",
      license: "Proprietary",
      version: "1",
      homepage: "https://www.xunlei.com",
    },
    icons,
    fileExtensions: sortRecord(fileExtensions),
    kinds: THUNDER_KINDS,
    default: "unknown",
  };
}

// ─────────────────────────── main ───────────────────────────

await mkdir(OUT_DIR, { recursive: true });
const work = await mkdtemp(join(tmpdir(), "fluxdown-icon-packs-"));
const only = new Set(process.argv.slice(2));
const wanted = (id: string) => only.size === 0 || only.has(id);
try {
  if (wanted("lucide")) await writePack("lucide.json", await lucidePack());
  if (wanted("material")) await writePack("material.json", await materialPack(work));
  if (wanted("catppuccin")) await writePack("catppuccin.json", await catppuccinPack(work));
  if (wanted("thunder")) await writePack("thunder.json", await thunderPack());
} finally {
  await rm(work, { recursive: true, force: true });
}
console.log(`wrote ${OUT_DIR}: ${(await readdir(OUT_DIR)).join(", ")}`);
