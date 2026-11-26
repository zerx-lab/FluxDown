/**
 * 语言路由工具:页面统一放在 `src/pages/[...lang]/` 下,一个文件为 `LANGS` 的每种语言各产出一条路径。
 *
 * ```astro
 * ---
 * import { langStaticPaths, resolveLang } from "@/i18n/routing";
 * export const getStaticPaths = langStaticPaths;
 * const lang = resolveLang(Astro.params.lang);
 * ---
 * ```
 */
import { stripBase, withBase } from "@/lib/base";
import { DEFAULT_LANG, LANGS, isLang, type Lang } from "./config";

/** 带路径前缀的语言(除默认语言外的全部);默认语言位于根路径。 */
const PREFIX_LANGS = LANGS.filter((lang) => lang !== DEFAULT_LANG);
/** 站点路径开头的语言前缀:`/zh/x` → `["zh", "/x"]`;`zh-tw` 与 `zh` 互不吞并(后随 `/` 或结尾)。 */
const LANG_PREFIX_RE = new RegExp(`^/(${PREFIX_LANGS.join("|")})(/.*)?$`);
const DOCS_LANG_RE = new RegExp(`^/docs/(${LANGS.join("|")})(/.*)?$`);

/** `[...lang]` 目录页面的静态路径:en → 无前缀,其余语言 → 各自的语言码。 */
export function langStaticPaths() {
  return LANGS.map((lang) => ({
    params: { lang: lang === DEFAULT_LANG ? undefined : lang },
  }));
}

/** 把路由参数解析成语言;未知值按默认语言处理(静态路径下不会出现)。 */
export function resolveLang(param: string | undefined): Lang {
  return isLang(param) ? param : DEFAULT_LANG;
}

/** 去掉路径中的挂载前缀与语言前缀,返回规范化的无语言站点路径(以 `/` 开头)。 */
export function stripLang(pathname: string): string {
  const sitePath = stripBase(pathname);
  const match = sitePath.match(LANG_PREFIX_RE);
  if (!match) return sitePath;
  return match[2] && match[2] !== "" ? match[2] : "/";
}

/** 从 URL 路径(可带挂载前缀)识别语言。 */
export function langFromPath(pathname: string): Lang {
  const match = stripBase(pathname).match(LANG_PREFIX_RE);
  return match ? (match[1] as Lang) : DEFAULT_LANG;
}

/**
 * 站内链接本地化、规范为尾斜杠(与 canonical / sitemap 一致)并补挂载前缀:
 * `href("/download", "zh")` → `/zh/download/`,`href("/#features", "zh")` → `/zh/#features`。
 * 外链原样返回;`/api/` 只补前缀;`/docs/` 自带语言结构,只补尾斜杠。
 */
export function href(path: string, lang: Lang): string {
  if (!path.startsWith("/") || path.startsWith("//")) return path;
  if (path.startsWith("/api/")) return withBase(path);
  const cut = path.search(/[?#]/);
  const pathname = cut === -1 ? path : path.slice(0, cut);
  const suffix = cut === -1 ? "" : path.slice(cut);
  const slashed = pathname.endsWith("/") ? pathname : `${pathname}/`;
  if (slashed.startsWith("/docs/") || lang === DEFAULT_LANG) return withBase(slashed + suffix);
  return withBase((slashed === "/" ? `/${lang}/` : `/${lang}${slashed}`) + suffix);
}

/** 同一页面在另一语言下的路径(语言切换器 / hreflang);入参可带挂载前缀。 */
export function switchLang(pathname: string, target: Lang): string {
  const docs = stripBase(pathname).match(DOCS_LANG_RE);
  if (docs) return withBase(`/docs/${target}${docs[2] ?? "/"}`);
  return href(stripLang(pathname), target);
}
