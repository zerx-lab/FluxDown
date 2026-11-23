/**
 * 规则求值器（纯模块）：把规则包 + 覆盖 + 吊销文档编译成一组查询函数。
 * 求值语义见计划 C3。
 *
 * 求值顺序：层级（local < official < plugin < community < builtin）→ priority（大者先）→ 包内顺序。
 * response 阶段的 `classifyResponse` 与 `classify` 共用这一顺序：同一 URL / MIME 下两者结论一致，
 * 扩展名规则（builtin priority 20）先于 MIME 规则（priority 10–11）。二者的区别只有输入与过滤：
 * `classifyResponse` 看全部 response 规则并遵守 requires / classifyOnly / size，`classify` 只看带
 * ext 或 mime 条件的 resource 规则并忽略 requires / classifyOnly（供无响应上下文的调用方使用）。
 */
import type { ConfidenceLevel } from "../utils/resource-types";
import builtinJson from "./builtin.json";
import officialJson from "./official.json";
import {
  asciiLower,
  compileGlob,
  hostMatchesLower,
  parseHostPattern,
  type GlobMatcher,
  type HostPattern,
} from "./glob";
import { REGEX_INPUT_CAP } from "./regex-safety";
import {
  RULE_ENGINE_VERSION,
  TIER_RANK,
  type EmitSpec,
  type MatchSpec,
  type ResourceType,
  type RevocationDoc,
  type RuleAction,
  type RuleOverrides,
  type RulePack,
  type RuleStage,
  type RuleTier,
  type SniffRuleV1,
} from "./schema";
import { extractExtension, hostOf, parseUrlParts } from "./url";
import { PackRejectedError, validatePack } from "./validate";

// ===== 随扩展打包的规则 =====

export const BUILTIN_PACK = builtinJson as unknown as RulePack;

/**
 * 官方包快照：规则仓 zerx-lab/fluxdown-sniff-rules 已发布的 `dist/official.json` 载荷，
 * 由 `bun scripts/sniff-rules/sync-repo.ts seed` 生成，勿手改。远程同步到更高 sequence 时被替换。
 */
export const BUNDLED_OFFICIAL_PACK = officialJson as unknown as RulePack;

/** 随包规则已由 engine.test 保证通过校验：编译时跳过 validatePack（页面 / 内容脚本每帧都会编译）。 */
const TRUSTED_PACKS = new WeakSet<object>([BUILTIN_PACK, BUNDLED_OFFICIAL_PACK]);

/** 一个存储槽位：规则包 + 该槽位对应的层级（校验以槽位层级为准，不信任包自报的 packId）。 */
export interface PackSlot {
  tier: RuleTier;
  pack: RulePack;
}

export interface CompileInput {
  packs: PackSlot[];
  overrides: RuleOverrides;
  revocations: RevocationDoc | null;
  options: { sniffImages: boolean; communityEnabled: boolean; now: number };
}

export interface RuleRef {
  id: string;
  version: number;
  tier: SniffRuleV1["tier"];
}

export interface ResponseInput {
  url: string;
  pageUrl: string;
  resourceType?: string;
  mime?: string;
  size: number;
  status?: number;
  headers?: Record<string, string>;
  attachment: boolean;
}

export type ResponseDecision =
  | {
      decision: "resource";
      type: ResourceType;
      confidence?: ConfidenceLevel;
      rule: RuleRef;
    }
  | { decision: "ignore"; rule: RuleRef }
  | { decision: "none" };

export interface Emitted {
  mode: "resource" | "preempt";
  url: string;
  filename?: string;
  type?: ResourceType;
  quality?: string;
}

export type RuleStatus =
  | "active"
  | "disabled"
  /** 所在层级整体停用（社区规则关闭），由开关决定，单条不可操作。 */
  | "paused"
  | "soft-revoked"
  | "hard-revoked"
  | "expired"
  | "engine-too-new"
  /** 未通过校验或编译失败；`detail` 为原因。 */
  | "rejected";

/** `expired` 的具体原因（稳定代码，由 UI 本地化）。 */
export type RuleStatusReason = "pack-expired" | "revocations-stale";

/** 生效但当前不会改变 classifyResponse 结果的原因。 */
export type RuleInert = "requires-setting" | "classify-only";

export interface RuleListEntry {
  rule: SniffRuleV1;
  status: RuleStatus;
  reason?: RuleStatusReason;
  /** `rejected` 时的校验 / 编译错误原文（中文）。 */
  detail?: string;
  /** `rejected` 时的稳定 snake_case 代码（UI 映射 `rules.validate.<code>`，缺省回退 detail）。 */
  code?: string;
  /** `rejected` 时出问题的字段 / 取值。 */
  param?: string;
  /** `active` 时：规则在求值顺序中的位置（0 起，全部 active 规则统一编号；同一 stage 内越小越先）。 */
  evalOrder?: number;
  /**
   * `active` 但惰性：`requires-setting` = 所需设置（sniffImages）未开启，classifyResponse 跳过；
   * `classify-only` = 只参与 classify()，classifyResponse 不用。
   */
  inert?: RuleInert;
}

interface Problem {
  detail: string;
  code: string;
  param?: string;
}

/** 被拒规则的展示条目：原始输入可能畸形，补齐列表渲染所需字段。 */
function rejectedEntry(raw: unknown, tier: RuleTier, index: number, problem: Problem): RuleListEntry {
  const r = raw !== null && typeof raw === "object" && !Array.isArray(raw) ? (raw as Partial<SniffRuleV1>) : {};
  const rule = {
    ...r,
    id: typeof r.id === "string" && r.id !== "" ? r.id : `${tier}.#${index + 1}`,
    tier,
    version: Number.isInteger(r.version) ? (r.version as number) : 0,
    stage: (typeof r.stage === "string" ? r.stage : "") as RuleStage,
    title: typeof r.title === "string" ? r.title : undefined,
  } as SniffRuleV1;
  const entry: RuleListEntry = { rule, status: "rejected", detail: problem.detail, code: problem.code };
  if (problem.param !== undefined) entry.param = problem.param;
  return entry;
}

export interface BodyWatcher {
  rule: RuleRef;
  url?: string[];
  host?: string[];
  pathRegex?: string;
}

export interface BodyExtraction {
  rule: RuleRef;
  emits: Emitted[];
}

export interface DomRuleEntry {
  rule: RuleRef;
  selector: string;
  attr: string;
  emit: EmitSpec;
}

/** page 阶段规则给内容脚本的提示。 */
export interface PageHints {
  titles: { selector: string; attr?: string }[];
  /** `float: "hide"`：这些容器内的媒体元素不显示浮钮。 */
  players: { selector: string }[];
  /** `float: "inset"`：这些容器内的媒体元素，贴顶部的浮钮离元素顶部至少 `top` px。 */
  insets: { selector: string; top: number }[];
}

/**
 * 可序列化的 classify 规则（按求值顺序）：background 把用户实际生效的集合经 PageConfig 下发给
 * 页面 / 内容脚本，由 {@link createClassifier} 还原，避免各上下文各自拿「仅随包规则」判定。
 */
export interface ClassifyRule {
  type: ResourceType;
  tier: RuleTier;
  match: MatchSpec;
  exclude?: MatchSpec[];
}

/** `explainResponse` 的一项：一条命中的 response 规则。 */
export interface ResponseExplanation {
  ref: RuleRef;
  decision: "resource" | "ignore";
  /** 是 classifyResponse 实际采用的那条（第一条命中者） */
  winner: boolean;
}

export interface CompiledRules {
  /** 受 requires / classifyOnly 约束 */
  classifyResponse(i: ResponseInput): ResponseDecision;
  /**
   * 命中的全部 response 规则（求值顺序；与 classifyResponse 同样跳过 classifyOnly 与未满足
   * requires 的规则），第一条为 winner。供「规则测试」展示被遮蔽的规则。
   */
  explainResponse(i: ResponseInput): ResponseExplanation[];
  /**
   * 只看带 ext / mime 条件的 resource 规则（忽略 requires、classifyOnly、size）；
   * 与 classifyResponse 同一求值顺序（扩展名规则先于 MIME 规则），都不中为 "other"。
   */
  classify(url: string, mime?: string): ResourceType;
  /** classify 所用规则的可序列化形式（求值顺序），经 PageConfig 下发给页面上下文。 */
  classifyRules(): ClassifyRule[];
  /** `mime` 可选：带上响应 MIME 时，按 MIME 排除的噪音规则（如 `/api/v1/…` 下的 PDF）与 classifyResponse 一致。 */
  isIgnored(url: string, pageUrl: string, attachment: boolean, mime?: string): RuleRef | null;
  /** 替代 normalizeUrlForDedup */
  dedupeKey(url: string, pageUrl: string): string;
  /** 先取 path 扩展名，再依次尝试命中的 filename 规则（取第一个非空结果） */
  fileExtension(url: string, pageUrl: string): string;
  trackKind(input: {
    url: string;
    filename?: string;
    mime?: string;
    pageUrl: string;
  }): "video" | "audio" | null;
  bodyWatchers(pageUrl: string): BodyWatcher[];
  extractFromBody(url: string, pageUrl: string, json: unknown): BodyExtraction[];
  /** body 阶段 manifest 规则命中时，按规则 path 取出的 DASH 结构子树。 */
  manifestsFromBody(url: string, pageUrl: string, json: unknown): unknown[];
  domRules(pageUrl: string): DomRuleEntry[];
  pageHints(pageUrl: string): PageHints;
  /** request 阶段 headers 规则为该 URL 补充的请求头（名字小写；先命中的规则优先）。 */
  requestHeaders(url: string, pageUrl: string): Record<string, string>;
  /** normalize 阶段 mirror 规则声明的镜像域名组。 */
  mirrorGroups(): string[][];
  renderDomEmit(
    rule: RuleRef,
    emit: EmitSpec,
    fields: { value: string; text: string; page: string },
  ): Emitted | null;
  listRules(): RuleListEntry[];
  /** 所有生效 resource 规则 match.ext 的并集（含 image），供 UI / 其它模块使用 */
  knownExtensions(): ReadonlySet<string>;
}

// ===== 匹配 =====

interface Ctx {
  url: string;
  host: string;
  pageHost: string;
  path: string;
  ext: string;
  mime: string;
  resourceType?: string;
  status?: number;
  size: number;
  attachment: boolean;
  /** 调用方给的响应头（键大小写不定）；仅在有规则用到 header 谓词时才建小写索引 */
  rawHeaders: Record<string, string> | undefined;
  lowerUrl: string | undefined;
  lowerHeaders: Map<string, string> | undefined;
}

type Pred = (c: Ctx) => boolean;

function lowerUrlOf(c: Ctx): string {
  return (c.lowerUrl ??= asciiLower(c.url));
}

function headerOf(c: Ctx, name: string): string | undefined {
  if (c.rawHeaders === undefined) return undefined;
  if (c.lowerHeaders === undefined) {
    const map = new Map<string, string>();
    for (const [k, v] of Object.entries(c.rawHeaders)) map.set(k.toLowerCase(), v);
    c.lowerHeaders = map;
  }
  return c.lowerHeaders.get(name);
}

/** 同一标签页的 pageUrl 几乎不变：缓存最近一次的解析结果。 */
let lastPageUrl = "";
let lastPageHost = "";

function pageHostOf(pageUrl: string): string {
  if (pageUrl === "") return "";
  if (pageUrl !== lastPageUrl) {
    lastPageHost = hostOf(pageUrl);
    lastPageUrl = pageUrl;
  }
  return lastPageHost;
}

function normalizeMime(mime: string | undefined): string {
  if (mime === undefined) return "";
  const semi = mime.indexOf(";");
  return (semi < 0 ? mime : mime.slice(0, semi)).trim().toLowerCase();
}

function makeCtx(
  url: string,
  pageUrl: string,
  extra: Partial<Pick<Ctx, "resourceType" | "status" | "size" | "attachment">> & {
    mime?: string;
    headers?: Record<string, string>;
  } = {},
): Ctx {
  const parts = parseUrlParts(url);
  return {
    url,
    host: parts.host,
    pageHost: pageUrl === url ? parts.host : pageHostOf(pageUrl),
    path: parts.path,
    ext: parts.ext,
    mime: normalizeMime(extra.mime),
    resourceType: extra.resourceType,
    status: extra.status,
    size: extra.size ?? 0,
    attachment: extra.attachment ?? false,
    rawHeaders: extra.headers,
    lowerUrl: undefined,
    lowerHeaders: undefined,
  };
}

function compileHosts(list: string[]): HostPattern[] {
  const out: HostPattern[] = [];
  for (const p of list) {
    const parsed = parseHostPattern(p);
    if (parsed) out.push(parsed);
  }
  return out;
}

/** 非官方层级（local / plugin / community）执行正则时受输入长度上限约束。 */
function isCappedTier(tier: RuleTier): boolean {
  return tier !== "builtin" && tier !== "official";
}

/** 编译正则测试函数；`capped` 时超过 {@link REGEX_INPUT_CAP} 的输入一律不匹配。非法正则返回 null。 */
function compileRegexTest(source: string, capped: boolean): ((s: string) => boolean) | null {
  let re: RegExp;
  try {
    re = new RegExp(source, "i");
  } catch {
    return null;
  }
  return capped ? (s) => s.length <= REGEX_INPUT_CAP && re.test(s) : (s) => re.test(s);
}

/**
 * 编译单个 MatchSpec。谓词按开销从低到高排列（纯 AND，次序不影响结果）：
 * 标量 → host → ext / mime → pathRegex → url glob → header。
 */
function compileMatch(m: MatchSpec, capped: boolean): Pred {
  const preds: Pred[] = [];
  if (m.resourceType) {
    const types = m.resourceType;
    preds.push((c) => c.resourceType !== undefined && types.includes(c.resourceType));
  }
  if (m.status) {
    const [lo, hi] = m.status;
    preds.push((c) => c.status !== undefined && c.status >= lo && c.status <= hi);
  }
  if (m.minSize !== undefined) {
    const min = m.minSize;
    preds.push((c) => c.size <= 0 || c.size >= min);
  }
  if (m.maxSize !== undefined) {
    const max = m.maxSize;
    preds.push((c) => c.size <= 0 || c.size <= max);
  }
  if (m.attachment !== undefined) {
    const want = m.attachment;
    preds.push((c) => c.attachment === want);
  }
  if (m.host) {
    const hs = compileHosts(m.host);
    preds.push((c) => c.host !== "" && hs.some((p) => hostMatchesLower(c.host, p)));
  }
  if (m.pageHost) {
    const hs = compileHosts(m.pageHost);
    preds.push((c) => c.pageHost !== "" && hs.some((p) => hostMatchesLower(c.pageHost, p)));
  }
  if (m.ext) {
    const exts = new Set(m.ext.map((e) => e.toLowerCase()));
    preds.push((c) => c.ext !== "" && exts.has(c.ext));
  }
  if (m.hasExt !== undefined) {
    const want = m.hasExt;
    preds.push((c) => (c.ext !== "") === want);
  }
  if (m.mime) {
    const exact = new Set<string>();
    const prefixes: string[] = [];
    for (const spec of m.mime) {
      const s = spec.toLowerCase();
      if (s.endsWith("*")) prefixes.push(s.slice(0, -1));
      else exact.add(s);
    }
    preds.push((c) => c.mime !== "" && (exact.has(c.mime) || prefixes.some((p) => c.mime.startsWith(p))));
  }
  if (m.pathRegex !== undefined) {
    const test = compileRegexTest(m.pathRegex, capped);
    preds.push((c) => test !== null && test(c.path));
  }
  if (m.url) {
    const globs: GlobMatcher[] = m.url.map(compileGlob);
    preds.push((c) => {
      const lower = lowerUrlOf(c);
      return globs.some((g) => g(lower));
    });
  }
  if (m.header) {
    const hs = m.header.map((h) => ({ name: h.name.toLowerCase(), test: compileRegexTest(h.regex, capped) }));
    preds.push((c) =>
      hs.every((h) => {
        const v = headerOf(c, h.name);
        return v !== undefined && h.test !== null && h.test(v);
      }),
    );
  }
  // 常见的 0 / 1 / 2 个谓词免去 every 的闭包遍历。
  switch (preds.length) {
    case 0:
      return () => true;
    case 1:
      return preds[0];
    case 2: {
      const [a, b] = preds;
      return (c) => a(c) && b(c);
    }
    default:
      return (c) => preds.every((p) => p(c));
  }
}

/** main 命中且不被任一 exclude 命中。 */
function compileRuleMatcher(match: MatchSpec, exclude: MatchSpec[] | undefined, capped: boolean): Pred {
  const main = compileMatch(match, capped);
  const ex = (exclude ?? []).map((e) => compileMatch(e, capped));
  if (ex.length === 0) return main;
  return (c) => main(c) && !ex.some((p) => p(c));
}

// ===== 模板 / 路径 =====

function getPath(root: unknown, path: string): unknown {
  let cur: unknown = root;
  for (const seg of path.split(".")) {
    if (cur === null || typeof cur !== "object") return undefined;
    if (Array.isArray(cur)) {
      if (!/^\d+$/.test(seg)) return undefined;
      cur = cur[Number(seg)];
    } else {
      if (!Object.hasOwn(cur, seg)) return undefined;
      cur = (cur as Record<string, unknown>)[seg];
    }
  }
  return cur;
}

type Fields = Record<string, string>;

/** `${x}` 替换；字段缺失返回 null。 */
function render(t: string, fields: Fields): string | null {
  let missing = false;
  const out = t.replace(/\$\{([^}]*)\}/g, (_m, key: string) => {
    if (!Object.hasOwn(fields, key)) {
      missing = true;
      return "";
    }
    return fields[key];
  });
  return missing ? null : out;
}

function resolveUrl(base: string, ref: string, bareQueryPrefix?: string): string | null {
  let out: string;
  if (ref.startsWith("http://") || ref.startsWith("https://")) {
    out = ref;
  } else if (ref.startsWith("/")) {
    try {
      out = new URL(base).origin + ref;
    } catch {
      return null;
    }
  } else if (bareQueryPrefix !== undefined) {
    out = base + bareQueryPrefix + ref;
  } else {
    return null;
  }
  return out.startsWith("http") ? out : null;
}

// ===== 编译 =====

interface CRule {
  ref: RuleRef;
  rule: SniffRuleV1;
  matches: Pred;
  /** match.ext 的小写集合；无 ext 条件为 null（用于按扩展名建索引） */
  exts: Set<string> | null;
  /** 仅 match.pageHost 的谓词（bodyWatchers 用，编译一次） */
  pageHostPred: Pred | null;
}

/**
 * 按扩展名分桶的候选规则表：每桶保持原求值顺序，无扩展名条件的规则出现在每个桶里。
 * 桶在首次查询该扩展名时才构建并缓存（页面 / 内容脚本每帧编译一次，只会遇到少数扩展名）。
 */
interface ExtIndex<R extends { exts: Set<string> | null }> {
  rules: R[];
  /** 出现在任一规则 ext 条件里的扩展名 */
  keys: Set<string>;
  byExt: Map<string, R[]>;
  /** 扩展名为空 / 不在任何规则的 ext 条件里时：只有无 ext 条件的规则可能命中 */
  noExt: R[];
}

function buildExtIndex<R extends { exts: Set<string> | null }>(rules: R[]): ExtIndex<R> {
  const keys = new Set<string>();
  for (const r of rules) for (const e of r.exts ?? []) keys.add(e);
  return { rules, keys, byExt: new Map(), noExt: rules.filter((r) => r.exts === null) };
}

function candidates<R extends { exts: Set<string> | null }>(index: ExtIndex<R>, ext: string): R[] {
  if (ext === "" || !index.keys.has(ext)) return index.noExt;
  let list = index.byExt.get(ext);
  if (list === undefined) {
    list = index.rules.filter((r) => r.exts === null || r.exts.has(ext));
    index.byExt.set(ext, list);
  }
  return list;
}

function isoMs(s: string | undefined): number {
  return s === undefined ? Number.NaN : Date.parse(s);
}

const GLOBAL_MATCH_KEYS: (keyof MatchSpec)[] = [
  "host", "pageHost", "url", "pathRegex", "ext", "hasExt", "mime",
  "resourceType", "status", "minSize", "maxSize", "attachment", "header",
];

function isGlobalMatch(r: SniffRuleV1): boolean {
  return (
    GLOBAL_MATCH_KEYS.every((k) => r.match[k] === undefined) &&
    (r.exclude ?? []).length === 0
  );
}

type Classifier = (url: string, mime?: string) => ResourceType;

/** 按求值顺序单次扫描：第一条命中的 resource 规则决定类型。 */
function buildClassifier(rules: { type: ResourceType; exts: Set<string> | null; matches: Pred }[]): Classifier {
  const index = buildExtIndex(rules);
  return (url, mime) => {
    const c = makeCtx(url, "", { mime });
    for (const r of candidates(index, c.ext)) {
      if (r.matches(c)) return r.type;
    }
    return "other";
  };
}

/** 由 {@link CompiledRules.classifyRules} 的结果还原 classify（页面 / 内容脚本上下文用）。 */
export function createClassifier(rules: readonly ClassifyRule[]): Classifier {
  return buildClassifier(
    rules.map((r) => ({
      type: r.type,
      exts: r.match.ext ? new Set(r.match.ext.map((e) => e.toLowerCase())) : null,
      matches: compileRuleMatcher(r.match, r.exclude, isCappedTier(r.tier)),
    })),
  );
}

/** 编译规则集。 */
export function compileRules(input: CompileInput): CompiledRules {
  const { packs, overrides, revocations, options } = input;
  const now = options.now;
  const revokeValid = revocations !== null && isoMs(revocations.expiresAt) > now;

  const listing: RuleListEntry[] = [];
  const active: { rule: SniffRuleV1; order: number; entry: RuleListEntry }[] = [];
  let order = 0;

  const revocationOf = (rule: SniffRuleV1): "soft" | "hard" | null => {
    let found: "soft" | "hard" | null = null;
    for (const e of revocations?.entries ?? []) {
      if (e.ruleId !== rule.id) continue;
      // 吊销条目来自签名文档，但仍按畸形输入防御：versions 缺失视为不命中。
      const max = e.versions === "*" ? Number.POSITIVE_INFINITY : e.versions?.max;
      if (typeof max !== "number" || !(rule.version <= max)) continue;
      if (e.action === "hard") return "hard";
      found = "soft";
    }
    return found;
  };

  for (const { tier, pack: rawPack } of packs) {
    if (rawPack.engine > RULE_ENGINE_VERSION) {
      for (const rule of Array.isArray(rawPack.rules) ? rawPack.rules : []) {
        listing.push({ rule, status: "engine-too-new" });
      }
      continue;
    }
    let pack: RulePack;
    if (TRUSTED_PACKS.has(rawPack) && rawPack.packId === tier) {
      pack = rawPack;
    } else {
      try {
        const validated = validatePack(rawPack, tier);
        pack = validated.pack;
        for (const r of validated.rejected) {
          listing.push(
            rejectedEntry(r.rule, tier, rawPack.rules.indexOf(r.rule as SniffRuleV1), {
              detail: r.reason,
              code: r.code,
              param: r.param,
            }),
          );
        }
      } catch (e) {
        if (!(e instanceof PackRejectedError)) throw e;
        // 整包被拒（格式 / 数量上限等）：逐条列出并带原因，不让规则静默消失。
        if (Array.isArray(rawPack?.rules)) {
          rawPack.rules.forEach((rule, i) =>
            listing.push(rejectedEntry(rule, tier, i, { detail: e.message, code: e.code })),
          );
        }
        continue;
      }
    }

    let paused: { status: RuleStatus; reason?: RuleStatusReason } | null = null;
    if (tier === "community") {
      if (!options.communityEnabled) {
        paused = { status: "paused" };
      } else if (pack.expiresAt !== undefined && !(isoMs(pack.expiresAt) > now)) {
        paused = { status: "expired", reason: "pack-expired" };
      } else if (!revokeValid) {
        paused = { status: "expired", reason: "revocations-stale" };
      }
    }

    for (const rule of pack.rules) {
      const ov = overrides[rule.id];
      const rev = revocationOf(rule);
      if (rev === "hard") {
        listing.push({ rule, status: "hard-revoked" });
        continue;
      }
      if (paused) {
        listing.push({ rule, ...paused });
        continue;
      }
      if (
        rev === "soft" &&
        !(ov?.enabled === true && ov.ackRevocation === revocations?.sequence)
      ) {
        listing.push({ rule, status: "soft-revoked" });
        continue;
      }
      const enabled = ov ? ov.enabled : rule.enabled !== false;
      if (!enabled) {
        listing.push({ rule, status: "disabled" });
        continue;
      }
      const entry: RuleListEntry = { rule, status: "active" };
      listing.push(entry);
      active.push({ rule, order: order++, entry });
    }
  }

  active.sort(
    (a, b) =>
      TIER_RANK[a.rule.tier] - TIER_RANK[b.rule.tier] ||
      (b.rule.priority ?? 0) - (a.rule.priority ?? 0) ||
      a.order - b.order,
  );

  const toC = (rule: SniffRuleV1): CRule => {
    const capped = isCappedTier(rule.tier);
    return {
      ref: { id: rule.id, version: rule.version, tier: rule.tier },
      rule,
      matches: compileRuleMatcher(rule.match, rule.exclude, capped),
      exts: rule.match.ext ? new Set(rule.match.ext.map((e) => e.toLowerCase())) : null,
      pageHostPred: rule.match.pageHost ? compileMatch({ pageHost: rule.match.pageHost }, capped) : null,
    };
  };

  // 校验已约束字段类型；仍兜底单条编译失败，一条坏规则不得拖垮整个引擎。
  const compiled = new Map<SniffRuleV1, CRule>();
  active.forEach((a, i) => {
    try {
      compiled.set(a.rule, toC(a.rule));
      a.entry.evalOrder = i;
      if (a.rule.stage === "response") {
        if (a.rule.classifyOnly) a.entry.inert = "classify-only";
        else if (a.rule.requires === "sniffImages" && !options.sniffImages) a.entry.inert = "requires-setting";
      }
    } catch (e) {
      a.entry.status = "rejected";
      a.entry.detail = e instanceof Error ? e.message : String(e);
      a.entry.code = "compile_failed";
    }
  });

  const byKind = <K extends RuleAction["kind"]>(...kinds: K[]): CRule[] =>
    active
      .filter((a) => (kinds as string[]).includes(a.rule.action.kind))
      .flatMap((a) => compiled.get(a.rule) ?? []);

  const responseRules = byKind("resource", "ignore");
  const stripRules = byKind("strip-params");
  const filenameRules = byKind("filename");
  const trackRules = byKind("track");
  const bodyRules = byKind("extract", "manifest");
  const domRuleList = byKind("dom");
  const pageRules = byKind("title", "player");
  const headerRules = byKind("headers");
  const mirrorRules = byKind("mirror");

  // classifyResponse / explainResponse 只看参与求值的规则（classifyOnly 与未满足 requires 的不算）。
  const participates = (r: CRule): boolean =>
    !r.rule.classifyOnly && !(r.rule.requires === "sniffImages" && !options.sniffImages);
  const responseIndex = buildExtIndex(responseRules.filter(participates));
  const ignoreIndex = buildExtIndex(responseRules.filter((r) => r.rule.action.kind === "ignore"));

  const resourceType = (r: CRule): ResourceType =>
    (r.rule.action as Extract<RuleAction, { kind: "resource" }>).type;
  // classify 只看带 ext / mime 条件的 resource 规则；与 classifyResponse 同一求值顺序。
  const classifyCRules = responseRules.filter(
    (r) => r.rule.action.kind === "resource" && (r.rule.match.mime || r.rule.match.ext),
  );
  const classify = buildClassifier(
    classifyCRules.map((r) => ({ type: resourceType(r), exts: r.exts, matches: r.matches })),
  );
  let classifyRuleList: ClassifyRule[] | null = null;

  const known = new Set<string>();
  for (const c of responseRules) {
    if (c.rule.action.kind !== "resource") continue;
    for (const e of c.rule.match.ext ?? []) known.add(e.toLowerCase());
  }

  // community / plugin 规则产出的地址只能落在该规则 match.host / pageHost 声明的站点内，
  // 防止规则把任意外部地址伪装成下载资源。
  const emitScope = new Map<string, HostPattern[]>();
  for (const { rule } of active) {
    if (rule.tier === "community" || rule.tier === "plugin") {
      emitScope.set(rule.id, compileHosts([...(rule.match.host ?? []), ...(rule.match.pageHost ?? [])]));
    }
  }
  const emitAllowed = (rule: RuleRef, url: string): boolean => {
    const scope = emitScope.get(rule.id);
    if (scope === undefined) return true;
    const host = hostOf(url);
    return host !== "" && scope.some((p) => hostMatchesLower(host, p));
  };

  const globalStrip = new Set<string>();
  const scopedStrip: CRule[] = [];
  for (const c of stripRules) {
    if (isGlobalMatch(c.rule)) {
      const a = c.rule.action;
      if (a.kind === "strip-params") for (const p of a.params) globalStrip.add(p.toLowerCase());
    } else {
      scopedStrip.push(c);
    }
  }

  const trackTest = new Map<string, RegExp | null>();
  for (const c of trackRules) {
    const a = c.rule.action;
    if (a.kind !== "track") continue;
    try {
      trackTest.set(c.rule.id, new RegExp(a.pattern, "i"));
    } catch {
      trackTest.set(c.rule.id, null);
    }
  }

  const emitOne = (spec: EmitSpec, fields: Fields): Emitted | null => {
    let url: string | null;
    if (typeof spec.url === "string") {
      url = render(spec.url, fields);
    } else {
      const base = render(spec.url.resolve.base, fields);
      const ref = render(spec.url.resolve.ref, fields);
      url = base === null || ref === null ? null : resolveUrl(base, ref, spec.url.resolve.bareQueryPrefix);
    }
    if (url === null) return null;
    const out: Emitted = { mode: spec.mode, url, type: spec.type ?? classify(url) };
    if (spec.filename !== undefined) {
      const f = render(spec.filename, fields);
      if (f !== null && f !== "") out.filename = f;
    }
    if (spec.quality !== undefined) {
      const q = render(spec.quality, fields);
      if (q !== null && q !== "") out.quality = q;
    }
    return out;
  };

  const scalarFields = (el: unknown, defs: Record<string, string>): Fields => {
    const out: Fields = {};
    for (const [name, path] of Object.entries(defs)) {
      const v = getPath(el, path);
      if (typeof v === "string") out[name] = v;
      else if (typeof v === "number" || typeof v === "boolean" || typeof v === "bigint") out[name] = String(v);
    }
    return out;
  };

  /** 仅看 query 里的 `mime=audio…` / `mime=video…`（如 googlevideo）；路径里的 `/video/` 目录不算线索。 */
  const trackFromUrlHint = (url: string): "video" | "audio" | null => {
    const m = /[?&]mime=([^&#]*)/i.exec(url);
    if (!m) return null;
    let value: string;
    try {
      value = decodeURIComponent(m[1]).toLowerCase();
    } catch {
      value = m[1].toLowerCase();
    }
    if (value.startsWith("audio")) return "audio";
    if (value.startsWith("video")) return "video";
    return null;
  };

  const responseCtx = (i: ResponseInput): Ctx =>
    makeCtx(i.url, i.pageUrl, {
      mime: i.mime,
      size: i.size,
      status: i.status,
      headers: i.headers,
      attachment: i.attachment,
      resourceType: i.resourceType,
    });

  return {
    classifyResponse(i) {
      const c = responseCtx(i);
      for (const r of candidates(responseIndex, c.ext)) {
        if (!r.matches(c)) continue;
        const a = r.rule.action;
        if (a.kind === "ignore") return { decision: "ignore", rule: r.ref };
        if (a.kind === "resource") {
          return { decision: "resource", type: a.type, confidence: a.confidence, rule: r.ref };
        }
      }
      return { decision: "none" };
    },

    explainResponse(i) {
      const c = responseCtx(i);
      const out: ResponseExplanation[] = [];
      for (const r of candidates(responseIndex, c.ext)) {
        if (!r.matches(c)) continue;
        const kind = r.rule.action.kind;
        if (kind !== "ignore" && kind !== "resource") continue;
        out.push({ ref: r.ref, decision: kind, winner: out.length === 0 });
      }
      return out;
    },

    classify,

    classifyRules() {
      classifyRuleList ??= classifyCRules.map((r) => {
        const out: ClassifyRule = { type: resourceType(r), tier: r.rule.tier, match: r.rule.match };
        if (r.rule.exclude?.length) out.exclude = r.rule.exclude;
        return out;
      });
      return classifyRuleList;
    },

    isIgnored(url, pageUrl, attachment, mime) {
      const c = makeCtx(url, pageUrl, { attachment, mime });
      for (const r of candidates(ignoreIndex, c.ext)) {
        if (r.matches(c)) return r.ref;
      }
      return null;
    },

    dedupeKey(url, pageUrl) {
      try {
        const u = new URL(url);
        u.hash = "";
        // 只有作用域 strip 规则命中时才复制全局集合（高频调用，绝大多数情况无需复制）。
        let strip = globalStrip;
        if (scopedStrip.length > 0) {
          const c = makeCtx(url, pageUrl);
          for (const r of scopedStrip) {
            const a = r.rule.action;
            if (a.kind === "strip-params" && r.matches(c)) {
              if (strip === globalStrip) strip = new Set(globalStrip);
              for (const p of a.params) strip.add(p.toLowerCase());
            }
          }
        }
        const del: string[] = [];
        u.searchParams.forEach((_v, k) => {
          if (strip.has(k.toLowerCase())) del.push(k);
        });
        for (const k of del) u.searchParams.delete(k);
        u.searchParams.sort();
        return u.toString();
      } catch {
        return url;
      }
    },

    fileExtension(url, pageUrl) {
      const fromPath = extractExtension(url);
      if (fromPath) return fromPath;
      const c = makeCtx(url, pageUrl);
      // 逐条尝试命中的 filename 规则：前一条无结果不遮蔽后面的（如 builtin 的 query-ext）。
      for (const r of filenameRules) {
        const a = r.rule.action;
        if (a.kind !== "filename" || !r.matches(c)) continue;
        const ext = extractExtension(url, a.extFromQuery, known);
        if (ext) return ext;
      }
      return "";
    },

    trackKind(input) {
      const mime = (input.mime ?? "").toLowerCase();
      if (mime.startsWith("audio/")) return "audio";
      const c = makeCtx(input.url, input.pageUrl, { mime: input.mime });
      let hostHit = false;
      for (const r of trackRules) {
        const a = r.rule.action;
        if (a.kind !== "track" || !r.matches(c)) continue;
        hostHit = true;
        const re = trackTest.get(r.rule.id);
        if (re) {
          const capped = isCappedTier(r.rule.tier);
          // 先试文件名，取不到轨道编号再试 URL（文件名可能被用户 / 站点改过）。
          for (const text of [input.filename, input.url]) {
            if (!text || (capped && text.length > REGEX_INPUT_CAP)) continue;
            const g = re.exec(text)?.[a.group];
            if (g === undefined) continue;
            if (a.audio.includes(g)) return "audio";
            if (!a.video || a.video.includes(g)) return "video";
          }
        }
        if (a.fallback !== "none") return a.fallback;
      }
      if (mime.startsWith("video/") && !hostHit) return "video";
      return trackFromUrlHint(input.url);
    },

    bodyWatchers(pageUrl) {
      const c = makeCtx(pageUrl, pageUrl);
      const out: BodyWatcher[] = [];
      for (const r of bodyRules) {
        const m = r.rule.match;
        if (r.pageHostPred && !r.pageHostPred(c)) continue;
        out.push({ rule: r.ref, url: m.url, host: m.host, pathRegex: m.pathRegex });
      }
      return out;
    },

    extractFromBody(url, pageUrl, json) {
      const c = makeCtx(url, pageUrl);
      const out: BodyExtraction[] = [];
      for (const r of bodyRules) {
        const a = r.rule.action;
        if (a.kind !== "extract" || !r.matches(c)) continue;
        if (a.when && !a.when.every((w) => getPath(json, w.path) === w.equals)) continue;
        let elements: unknown[];
        if (a.each !== undefined) {
          const arr = getPath(json, a.each);
          if (!Array.isArray(arr)) continue;
          elements = arr;
        } else {
          elements = [json];
        }
        const emits: Emitted[] = [];
        for (const el of elements) {
          const e = emitOne(a.emit, scalarFields(el, a.fields));
          if (e && emitAllowed(r.ref, e.url)) emits.push(e);
        }
        if (emits.length > 0) out.push({ rule: r.ref, emits });
      }
      return out;
    },

    manifestsFromBody(url, pageUrl, json) {
      const c = makeCtx(url, pageUrl);
      const out: unknown[] = [];
      for (const r of bodyRules) {
        const a = r.rule.action;
        if (a.kind !== "manifest" || !r.matches(c)) continue;
        const node = a.path ? getPath(json, a.path) : json;
        if (node !== undefined && node !== null) out.push(node);
      }
      return out;
    },

    domRules(pageUrl) {
      const c = makeCtx(pageUrl, pageUrl);
      const out: DomRuleEntry[] = [];
      for (const r of domRuleList) {
        const a = r.rule.action;
        if (a.kind === "dom" && r.matches(c)) {
          out.push({ rule: r.ref, selector: a.selector, attr: a.attr, emit: a.emit });
        }
      }
      return out;
    },

    pageHints(pageUrl) {
      const c = makeCtx(pageUrl, pageUrl);
      const hints: PageHints = { titles: [], players: [], insets: [] };
      for (const r of pageRules) {
        if (!r.matches(c)) continue;
        const a = r.rule.action;
        if (a.kind === "title") hints.titles.push(a.attr ? { selector: a.selector, attr: a.attr } : { selector: a.selector });
        else if (a.kind === "player" && a.float === "inset") hints.insets.push({ selector: a.selector, top: a.top });
        else if (a.kind === "player") hints.players.push({ selector: a.selector });
      }
      return hints;
    },

    requestHeaders(url, pageUrl) {
      const c = makeCtx(url, pageUrl);
      const out: Record<string, string> = {};
      for (const r of headerRules) {
        const a = r.rule.action;
        if (a.kind !== "headers" || !r.matches(c)) continue;
        for (const [name, value] of Object.entries(a.set)) {
          const lower = name.toLowerCase();
          if (!(lower in out)) out[lower] = value;
        }
      }
      return out;
    },

    mirrorGroups() {
      const groups: string[][] = [];
      for (const r of mirrorRules) {
        const a = r.rule.action;
        if (a.kind === "mirror") groups.push([...a.hosts]);
      }
      return groups;
    },

    renderDomEmit(rule, emit, fields) {
      const e = emitOne(emit, { value: fields.value, text: fields.text, page: fields.page });
      return e && emitAllowed(rule, e.url) ? e : null;
    },

    listRules() {
      return listing;
    },

    knownExtensions() {
      return known;
    },
  };
}

/** 远程官方包与随包快照取 sequence 较新者（相同时用远程，二者内容一致）。 */
export function effectiveOfficialPack(stored: RulePack | undefined): RulePack {
  return stored && stored.sequence >= BUNDLED_OFFICIAL_PACK.sequence ? stored : BUNDLED_OFFICIAL_PACK;
}

const bundledCache = new Map<boolean, CompiledRules>();

/**
 * 仅含随包规则（builtin + 官方包快照）的求值器：远程包加载前使用。
 * 随包规则无时间 / 社区相关状态，按 sniffImages 缓存为单例（页面 / 内容脚本每帧都会调用）。
 */
export function compileBundled(options: Partial<CompileInput["options"]> = {}): CompiledRules {
  const sniffImages = options.sniffImages ?? false;
  let engine = bundledCache.get(sniffImages);
  if (!engine) {
    engine = compileRules({
      packs: [
        { tier: "builtin", pack: BUILTIN_PACK },
        { tier: "official", pack: BUNDLED_OFFICIAL_PACK },
      ],
      overrides: {},
      revocations: null,
      options: { sniffImages, communityEnabled: true, now: options.now ?? Date.now() },
    });
    bundledCache.set(sniffImages, engine);
  }
  return engine;
}
