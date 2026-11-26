/**
 * Release body 处理:双语区块挑选、安全的 Markdown → HTML(先转义再标记)、纯文本导出、资产归类。
 * 输出的 HTML 只含本文件生成的标签,链接仅放行 http(s)。
 */
import { toBaselineLang, type Lang } from "@/i18n/config";
import type { changelog } from "@/i18n/messages/changelog";
import type { ChangelogRelease, ReleaseAsset } from "@/lib/release-format";

type T = (typeof changelog)[Lang];

/** 双语 release body 的语言标记(release 工作流翻译步骤写入;只有 en / zh 两段)。 */
const LANG_MARKER_RE = /<!--\s*fluxdown:lang:(zh|en)\s*-->/g;

/** 取当前语言区块(机器翻译语言沿用 en 区块);无标记(历史版本 / 翻译失败)时原样返回全文。 */
export function pickLocaleBody(body: string, lang: Lang): string {
  const matches = [...body.matchAll(LANG_MARKER_RE)];
  if (matches.length === 0) return body;
  const sections: Partial<Record<Lang, string>> = {};
  matches.forEach((match, i) => {
    const start = (match.index ?? 0) + match[0].length;
    const end = matches[i + 1]?.index ?? body.length;
    sections[match[1] as Lang] = body.slice(start, end).trim();
  });
  return sections[toBaselineLang(lang)] ?? sections.zh ?? body;
}

function escapeHtml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function renderInline(text: string): string {
  const codes: string[] = [];
  // 行内代码先占位,避免其中的 * / [ 被当成标记
  const withCodes = escapeHtml(text).replace(/`([^`]+)`/g, (_, code: string) => {
    codes.push(`<code>${code}</code>`);
    return `\u0000${codes.length - 1}\u0000`;
  });
  return withCodes
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\s][^*]*?)\*(?!\*)/g, "$1<em>$2</em>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
    .replace(/\u0000(\d+)\u0000/g, (_, i: string) => codes[Number(i)] ?? "");
}

/** 行级 Markdown:标题(## → h3,### → h4)、无序列表、围栏代码、段落。 */
export function renderMarkdown(markdown: string): string {
  const lines = markdown.replace(/<!--[\s\S]*?-->/g, "").replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let paragraph: string[] = [];
  let list: string[] = [];

  const flush = () => {
    if (paragraph.length) out.push(`<p>${renderInline(paragraph.join(" "))}</p>`);
    if (list.length) out.push(`<ul>${list.map((item) => `<li>${renderInline(item)}</li>`).join("")}</ul>`);
    paragraph = [];
    list = [];
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const fence = /^(\s*)```/.exec(line);
    if (fence) {
      flush();
      const indent = fence[1]!.length;
      const code: string[] = [];
      while (++i < lines.length && !/^\s*```\s*$/.test(lines[i]!)) code.push(lines[i]!.slice(indent));
      out.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
      continue;
    }
    const heading = /^(#{1,6})\s+(.+?)\s*#*$/.exec(line);
    if (heading) {
      flush();
      const tag = heading[1]!.length <= 2 ? "h3" : "h4";
      out.push(`<${tag}>${renderInline(heading[2]!)}</${tag}>`);
      continue;
    }
    const item = /^\s*[-*+]\s+(.+)$/.exec(line);
    if (item) {
      if (paragraph.length) {
        out.push(`<p>${renderInline(paragraph.join(" "))}</p>`);
        paragraph = [];
      }
      list.push(item[1]!);
      continue;
    }
    if (!line.trim()) {
      flush();
      continue;
    }
    if (list.length && /^\s{2,}\S/.test(line)) {
      list[list.length - 1] += ` ${line.trim()}`;
      continue;
    }
    if (list.length) flush();
    paragraph.push(line.trim());
  }
  flush();
  return out.join("");
}

const stripInline = (text: string) =>
  text.replace(/\*\*(.+?)\*\*/g, "$1").replace(/\*(.+?)\*/g, "$1").replace(/`([^`]+)`/g, "[$1]");

/** 适合粘贴到群公告的纯文本。 */
export function toPlainText(release: ChangelogRelease, lang: Lang, t: T, date: string): string {
  const result = [`【${t.textHeader(release.tag)}】`, date, ""];
  let counter = 0;
  let blank = true;
  for (const raw of pickLocaleBody(release.body, lang).replace(/<!--[\s\S]*?-->/g, "").split("\n")) {
    const line = raw.trim();
    if (!line) {
      if (!blank) result.push("");
      blank = true;
      continue;
    }
    blank = false;
    const heading = /^#{1,6}\s+(.+)$/.exec(line);
    if (heading) {
      counter = 0;
      result.push(`▌ ${stripInline(heading[1]!)}`);
    } else if (/^[-*+]\s+/.test(line)) {
      counter += 1;
      result.push(`${counter}. ${stripInline(line.replace(/^[-*+]\s+/, ""))}`);
    } else {
      result.push(stripInline(line));
    }
  }
  while (result.at(-1) === "") result.pop();
  return result.join("\n");
}

export interface AssetGroup {
  group: string;
  items: { asset: ReleaseAsset; sub: string }[];
}

const GROUP_ORDER = ["Windows", "macOS", "Linux", "Server", "NAS", "Android", "extension", "CLI", "other"];

function classify(name: string, t: T): { group: string; sub: string } {
  const n = name.toLowerCase();
  const plat = /-(windows|linux|macos)-(x64|arm64)\.(zip|tar\.gz)$/.exec(n);
  const platLabel = plat ? `${plat[1]} ${plat[2]}` : "";
  if (n.startsWith("fluxdown-cli-")) return { group: "CLI", sub: platLabel || name };
  if (n.startsWith("fluxdown-server-")) {
    const qnap = /-qnap-(x64|arm64)\.qpkg$/.exec(n);
    if (qnap) return { group: "NAS", sub: `QNAP ${qnap[1]}` };
    const syno = /-synology-(dsm6|dsm7)-(x64|arm64)\.spk$/.exec(n);
    if (syno) return { group: "NAS", sub: `Synology ${syno[1]!.toUpperCase()} ${syno[2]}` };
    const fnos = /-fnos-(x64|arm64)\.fpk$/.exec(n);
    if (fnos) return { group: "NAS", sub: `fnOS ${fnos[1]}` };
    return { group: "Server", sub: platLabel || name };
  }
  const openwrt = /\.(ipk|apk)$/.exec(n);
  if (openwrt && n.startsWith("luci-app-fluxdown_")) {
    return { group: "NAS", sub: `OpenWrt LuCI ${openwrt[1]}` };
  }
  if (openwrt && n.startsWith("fluxdown-server_")) {
    return { group: "NAS", sub: `OpenWrt ${/_([a-z0-9_-]+)\.(?:ipk|apk)$/.exec(n)?.[1] ?? ""} ${openwrt[1]}`.replace(/ +/g, " ") };
  }
  const abi = /-android-([a-z0-9_-]+)\.apk$/.exec(n);
  if (abi) return { group: "Android", sub: `${abi[1]} APK` };

  const arch = /arm64/.test(n) ? "ARM64" : /x64/.test(n) ? "x64" : "";
  const macArch = /arm64/.test(n) ? "Apple Silicon" : /x64/.test(n) ? "Intel" : "";
  if (n.endsWith("-setup.exe")) return { group: "Windows", sub: `${arch || "x64"} ${t.asset.installer}` };
  if (/-windows(-x64|-arm64)?-portable\.zip$/.test(n)) return { group: "Windows", sub: `${arch || "x64"} ${t.asset.portable}` };
  if (n.endsWith(".msi")) return { group: "Windows", sub: `${arch} ${t.asset.installer}`.trim() };
  if (n.endsWith(".dmg")) return { group: "macOS", sub: `${macArch} ${t.asset.dmg}`.trim() };
  if (n.endsWith(".pkg")) return { group: "macOS", sub: `${macArch} ${t.asset.pkg}`.trim() };
  if (/-macos-(x64|arm64)\.tar\.gz$/.test(n)) return { group: "macOS", sub: `${macArch} ${t.asset.archive}` };
  if (n.endsWith(".appimage")) return { group: "Linux", sub: "AppImage x64" };
  if (n.endsWith(".deb")) return { group: "Linux", sub: "Debian / Ubuntu" };
  if (n.endsWith(".pkg.tar.zst")) return { group: "Linux", sub: "Arch Linux" };
  if (n.endsWith(".rpm")) return { group: "Linux", sub: t.asset.rpm };
  if (/-linux-(x64|arm64)\.tar\.gz$/.test(n)) return { group: "Linux", sub: `${arch} ${t.asset.archive}` };
  if (n.endsWith("-chrome.crx")) return { group: "extension", sub: `Chrome ${t.asset.storeSigned}` };
  if (n.endsWith("-chrome.zip") || n.endsWith("-extension.zip")) return { group: "extension", sub: `Chrome / Edge ${t.asset.unpacked}` };
  if (n.endsWith("-firefox.xpi")) return { group: "extension", sub: "Firefox" };
  if (n.endsWith("-firefox-unsigned.zip")) return { group: "extension", sub: `Firefox ${t.asset.unsigned}` };
  return { group: "other", sub: name };
}

/** 按平台分组(组名中的 extension / other 由调用方本地化)。 */
export function groupAssets(assets: ReleaseAsset[], t: T): AssetGroup[] {
  const groups: Record<string, AssetGroup> = {};
  for (const asset of assets) {
    const { group, sub } = classify(asset.name, t);
    (groups[group] ??= { group, items: [] }).items.push({ asset, sub });
  }
  const rank = (g: string) => (GROUP_ORDER.includes(g) ? GROUP_ORDER.indexOf(g) : GROUP_ORDER.length);
  return Object.values(groups).sort((a, b) => rank(a.group) - rank(b.group));
}
