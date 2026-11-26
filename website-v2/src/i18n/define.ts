/**
 * 文案按命名空间拆成 `src/i18n/messages/<ns>.ts`,每个模块用 `defineMessages("<ns>", { en, zh })` 声明手写基线。
 *
 * en 是基线:zh 必须与 en 结构完全一致(键、数组、函数签名),缺键/多键在 `astro check` 阶段报错,
 * 所以基线语言不存在运行时回退。需要插值时直接写函数:`count: (n: number) => \`${n} items\``。
 *
 * 其余语言(`MT_LANGS`)由 `scripts/i18n-messages.ts generate` 生成 `src/i18n/locales/<lang>/<ns>.ts`
 * (默认导出与 en 同构,`satisfies` 保证结构一致),这里按命名空间装配;生成模块缺失时回退 en。
 * 生成模块只 `import type` 对应的 messages 模块,所以这里的 glob 不会成环。
 */
import { MT_LANGS, type Lang } from "./config";

/** 把 en 的字面量类型放宽为同构的可替换形状(字符串 → string,保留结构与函数签名)。 */
export type Shape<T> = T extends string
  ? string
  : T extends (...args: infer A) => infer R
    ? (...args: A) => R extends string ? string : Shape<R>
    : T extends readonly (infer U)[]
      ? readonly Shape<U>[]
      : T extends object
        ? { readonly [K in keyof T]: Shape<T[K]> }
        : T;

export type Messages<T> = Record<Lang, Shape<T>>;

/** `<lang>/<ns>` → 生成模块的默认导出。 */
const generated = new Map<string, unknown>(
  Object.entries(import.meta.glob("./locales/*/*.ts", { eager: true, import: "default" })).flatMap(([path, value]) => {
    const match = /^\.\/locales\/([^/]+)\/([^/]+)\.ts$/.exec(path);
    return match ? [[`${match[1]}/${match[2]}`, value] as const] : [];
  }),
);

export function defineMessages<const T>(ns: string, messages: { en: T; zh: Shape<T> }): Messages<T> {
  const result: Record<string, unknown> = { en: messages.en, zh: messages.zh };
  for (const lang of MT_LANGS) result[lang] = generated.get(`${lang}/${ns}`) ?? messages.en;
  return result as Messages<T>;
}
