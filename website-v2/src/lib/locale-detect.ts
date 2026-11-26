/**
 * 服务端 Accept-Language 解析,判定规则与 Layout.astro 首帧内联脚本的浏览器语言判定对齐:
 * 按优先级(q 值)顺序遍历,逐个经 `matchLangTag` 映射到站点语言(zh-Hant / zh-TW / zh-HK / zh-MO → zh-tw,
 * 其余 zh → zh,es / ru / tr / en 同名),都不匹配回退 `DEFAULT_LANG`。
 */
import { DEFAULT_LANG, matchLangTag, type Lang } from "@/i18n/config";

export function parseAcceptLanguage(header: string | null): Lang {
  if (!header) return DEFAULT_LANG;
  const tags = header
    .split(",")
    .map((part) => {
      const [tag, ...params] = part.trim().split(";");
      const q = params
        .map((p) => p.trim())
        .find((p) => p.startsWith("q="));
      return { tag: (tag ?? "").trim(), q: q ? Number.parseFloat(q.slice(2)) : 1 };
    })
    .filter((t) => t.tag.length > 0)
    .sort((a, b) => b.q - a.q);
  for (const { tag } of tags) {
    const lang = matchLangTag(tag);
    if (lang) return lang;
  }
  return DEFAULT_LANG;
}
