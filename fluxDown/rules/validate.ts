/** 规则包校验：整包不合格抛 PackRejectedError，单条不合格则剔除。 */
import { hostMatchesPattern, parseHostPattern } from "./glob";
import { isSafeRegex } from "./regex-safety";
import {
  ALLOWED_RULE_HEADERS,
  RULE_ENGINE_VERSION,
  STAGE_ACTIONS,
  TIER_RANK,
  type EmitSpec,
  type MatchSpec,
  type RulePack,
  type RuleTier,
  type SniffRuleV1,
} from "./schema";

export { isSafeRegex } from "./regex-safety";

export const MAX_RULES_PER_PACK = 2000;
const MAX_SELECTOR_LEN = 512;
/** 单条规则序列化后的字节上限（按 UTF-16 码元计，足够防止超大规则）。 */
const MAX_RULE_JSON_LEN = 16 * 1024;
const MAX_URL_GLOBS = 64;
const MAX_EXTS = 256;
const MAX_MIMES = 64;
const MAX_HEADER_PREDICATES = 8;
const MAX_MIRROR_HOSTS = 16;
const MAX_EXTRACT_FIELDS = 32;
/** player `inset` 的顶部避让上限（px）：足够容纳播放器顶部控件条，又不至于把浮钮推出小播放器。 */
const MAX_PLAYER_INSET_TOP = 200;
const ID_RE = /^(builtin|official|plugin|community|local)\.[a-z0-9][a-z0-9._-]{1,62}$/;
const RESOURCE_TYPES = new Set([
  "video",
  "audio",
  "document",
  "archive",
  "image",
  "executable",
  "torrent",
  "stream",
  "subtitle",
  "magnet",
  "other",
]);

/** 整包不合格。`code` 为稳定的 snake_case 代码（UI 本地化用），`message` 为中文原因。 */
export class PackRejectedError extends Error {
  readonly code: string;
  constructor(message: string, code = "pack_invalid") {
    super(message);
    this.name = "PackRejectedError";
    this.code = code;
  }
}

/** 单条规则不合格的原因：`code` 稳定代码，`param` 出问题的字段 / 取值，`detail` 中文原文。 */
export interface RuleProblem {
  code: string;
  detail: string;
  param?: string;
}

function bad(code: string, detail: string, param?: string): RuleProblem {
  return param === undefined ? { code, detail } : { code, detail, param };
}

function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((x) => typeof x === "string");
}

const ARRAY_LIMITS = { url: MAX_URL_GLOBS, ext: MAX_EXTS, mime: MAX_MIMES } as const;

/**
 * 校验 match。`scoped` = community / plugin 主 match：host / pageHost 不得是单标签（`com`），
 * 否则一条规则能作用于整个 TLD。exclude 只会收窄命中范围，不受此限。
 */
function checkMatch(m: MatchSpec | undefined, scoped = false): RuleProblem | null {
  if (m === undefined) return null;
  if (typeof m !== "object" || m === null || Array.isArray(m)) return bad("match_invalid", "match 不是对象");
  for (const key of ["host", "pageHost"] as const) {
    const list = m[key];
    if (list === undefined) continue;
    if (!Array.isArray(list)) return bad("host_list_invalid", "host/pageHost 须为数组", key);
    for (const h of list) {
      const parsed = typeof h === "string" ? parseHostPattern(h) : null;
      if (parsed === null) return bad("host_invalid", `非法 host 模式: ${String(h)}`, String(h));
      if (scoped && !parsed.domain.includes("."))
        return bad("host_too_broad", `host 范围过宽（单标签域名）: ${String(h)}`, String(h));
    }
  }
  for (const key of ["url", "ext", "mime", "resourceType"] as const) {
    const list = m[key];
    if (list === undefined) continue;
    if (!isStringArray(list)) return bad("string_array_required", `${key} 须为字符串数组`, key);
    const limit = key === "resourceType" ? undefined : ARRAY_LIMITS[key];
    if (limit !== undefined && list.length > limit) return bad("too_many", `${key} 最多 ${limit} 项`, key);
  }
  if (
    m.status !== undefined &&
    !(Array.isArray(m.status) && m.status.length === 2 && m.status.every((n) => Number.isInteger(n)))
  )
    return bad("status_invalid", "status 须为 [整数, 整数]", "status");
  for (const key of ["minSize", "maxSize"] as const) {
    const v = m[key];
    if (v !== undefined && !(typeof v === "number" && Number.isFinite(v) && v >= 0))
      return bad("size_invalid", `${key} 须为非负数`, key);
  }
  for (const key of ["hasExt", "attachment"] as const) {
    if (m[key] !== undefined && typeof m[key] !== "boolean") return bad("boolean_required", `${key} 须为布尔值`, key);
  }
  if (m.pathRegex !== undefined && !isSafeRegex(m.pathRegex)) return bad("unsafe_regex", "pathRegex 不安全", "pathRegex");
  if (m.header !== undefined) {
    if (!Array.isArray(m.header)) return bad("header_invalid", "header 须为数组", "header");
    if (m.header.length > MAX_HEADER_PREDICATES)
      return bad("too_many", `header 最多 ${MAX_HEADER_PREDICATES} 项`, "header");
    for (const h of m.header) {
      if (!h || typeof h.name !== "string" || !isSafeRegex(h.regex))
        return bad("unsafe_regex", "header 正则不安全", "header");
    }
  }
  return null;
}

const TEMPLATE_REF_RE = /\$\{([^}]*)\}/g;

function templateRefs(t: string | undefined): string[] {
  if (typeof t !== "string") return [];
  return [...t.matchAll(TEMPLATE_REF_RE)].map((m) => m[1]);
}

function emitRefs(emit: EmitSpec): string[] {
  const url = emit.url;
  const urlRefs =
    typeof url === "string"
      ? templateRefs(url)
      : [
          ...templateRefs(url?.resolve?.base),
          ...templateRefs(url?.resolve?.ref),
        ];
  return [...urlRefs, ...templateRefs(emit.filename), ...templateRefs(emit.quality)];
}

function checkEmit(emit: EmitSpec, allowed: Set<string>, tier: RuleTier): RuleProblem | null {
  if (!emit || typeof emit !== "object") return bad("emit_missing", "emit 缺失");
  if (emit.mode !== "resource" && emit.mode !== "preempt") return bad("emit_mode_invalid", "emit.mode 非法");
  if (emit.mode === "preempt" && tier !== "builtin" && tier !== "official")
    return bad("preempt_forbidden", "preempt 仅允许 builtin / official");
  if (emit.type !== undefined && !RESOURCE_TYPES.has(emit.type)) return bad("emit_type_invalid", "emit.type 非法");
  if (
    typeof emit.url !== "string" &&
    !(emit.url && typeof emit.url === "object" && emit.url.resolve)
  )
    return bad("emit_url_missing", "emit.url 缺失");
  for (const ref of emitRefs(emit)) {
    if (!allowed.has(ref)) return bad("template_unknown_field", `模板引用了未定义字段: ${ref}`, ref);
  }
  return null;
}

function checkSelector(selector: string): RuleProblem | null {
  if (typeof selector !== "string" || selector.length === 0) return bad("selector_missing", "selector 缺失");
  if (selector.length > MAX_SELECTOR_LEN) return bad("selector_too_long", "selector 过长");
  if (typeof document !== "undefined") {
    try {
      document.createDocumentFragment().querySelector(selector);
    } catch {
      return bad("selector_invalid", "selector 非法");
    }
  }
  return null;
}

function hasHost(m: MatchSpec): boolean {
  return (
    (Array.isArray(m.host) && m.host.length > 0) ||
    (Array.isArray(m.pageHost) && m.pageHost.length > 0)
  );
}

/** 校验单条规则；合格返回 null，否则返回原因。 */
function checkRule(rule: SniffRuleV1, expectedTier: RuleTier): RuleProblem | null {
  if (!rule || typeof rule !== "object") return bad("rule_invalid", "规则不是对象");
  if (typeof rule.id !== "string" || !ID_RE.test(rule.id)) return bad("id_invalid", "id 非法");
  // ID_RE 的长度上限只约束前缀之后的部分；plugin 层整体仍不得超过 64（与宿主 manifest 的约束一致）。
  if (rule.tier === "plugin" && rule.id.length > 64) return bad("id_invalid", "id 非法");
  if (rule.tier !== expectedTier) return bad("tier_mismatch", "tier 与 packId 不符");
  if (!rule.id.startsWith(`${rule.tier}.`)) return bad("id_prefix_mismatch", "id 前缀与 tier 不符");
  if (!Number.isInteger(rule.version) || rule.version < 1) return bad("version_invalid", "version 非法");
  if (!rule.match || typeof rule.match !== "object") return bad("match_missing", "match 缺失");
  if (!rule.action || typeof rule.action !== "object") return bad("action_missing", "action 缺失");
  const allowedKinds = STAGE_ACTIONS[rule.stage];
  if (!allowedKinds) return bad("stage_invalid", "stage 非法");
  if (!allowedKinds.includes(rule.action.kind)) return bad("stage_action_mismatch", "stage 与 action 不匹配");
  if (rule.requires !== undefined && rule.requires !== "sniffImages")
    return bad("requires_invalid", "requires 非法");
  if (rule.title !== undefined && typeof rule.title !== "string") return bad("field_type", "title 须为字符串", "title");
  if (rule.enabled !== undefined && typeof rule.enabled !== "boolean")
    return bad("field_type", "enabled 须为布尔值", "enabled");
  if (rule.classifyOnly !== undefined && typeof rule.classifyOnly !== "boolean")
    return bad("field_type", "classifyOnly 须为布尔值", "classifyOnly");
  if (rule.priority !== undefined && !(typeof rule.priority === "number" && Number.isFinite(rule.priority)))
    return bad("field_type", "priority 须为数字", "priority");
  if (JSON.stringify(rule).length > MAX_RULE_JSON_LEN)
    return bad("rule_too_large", `单条规则不得超过 ${MAX_RULE_JSON_LEN / 1024} KB`);

  const scoped = rule.tier === "community" || rule.tier === "plugin";
  const matchErr = checkMatch(rule.match, scoped);
  if (matchErr) return matchErr;
  if (rule.exclude !== undefined && !Array.isArray(rule.exclude))
    return bad("exclude_invalid", "exclude 须为数组", "exclude");
  for (const ex of rule.exclude ?? []) {
    const e = checkMatch(ex);
    if (e) return { ...e, detail: `exclude: ${e.detail}` };
  }

  if (scoped && !hasHost(rule.match)) return bad("scope_required", "社区规则必须限定 host 或 pageHost");

  const a = rule.action;
  switch (a.kind) {
    case "resource":
      if (!RESOURCE_TYPES.has(a.type)) return bad("resource_type_invalid", "resource.type 非法", String(a.type));
      break;
    case "ignore":
      break;
    case "strip-params":
      if (!isStringArray(a.params)) return bad("string_array_required", "params 须为字符串数组", "params");
      if (
        !hasHost(rule.match) &&
        rule.tier !== "builtin" &&
        rule.tier !== "official" &&
        rule.tier !== "local"
      )
        return bad("global_strip_forbidden", "全局 strip-params 仅允许 builtin / official / local");
      break;
    case "filename":
      if (!isStringArray(a.extFromQuery))
        return bad("string_array_required", "extFromQuery 须为字符串数组", "extFromQuery");
      break;
    case "track":
      if (!isSafeRegex(a.pattern)) return bad("unsafe_regex", "track.pattern 不安全", "track.pattern");
      if (!Number.isInteger(a.group) || a.group < 0) return bad("track_group_invalid", "track.group 非法");
      if (!isStringArray(a.audio)) return bad("string_array_required", "track.audio 须为字符串数组", "track.audio");
      if (a.video !== undefined && !isStringArray(a.video))
        return bad("string_array_required", "track.video 须为字符串数组", "track.video");
      if (a.fallback !== "video" && a.fallback !== "audio" && a.fallback !== "none")
        return bad("track_fallback_invalid", "track.fallback 非法");
      break;
    case "extract": {
      if (!a.fields || typeof a.fields !== "object" || Array.isArray(a.fields))
        return bad("fields_missing", "fields 缺失");
      if (Object.keys(a.fields).length > MAX_EXTRACT_FIELDS)
        return bad("too_many", `fields 最多 ${MAX_EXTRACT_FIELDS} 项`, "fields");
      if (!Object.values(a.fields).every((v) => typeof v === "string"))
        return bad("fields_invalid", "fields 值须为字符串");
      if (a.each !== undefined && typeof a.each !== "string") return bad("each_invalid", "each 须为字符串");
      if (
        a.when !== undefined &&
        !(Array.isArray(a.when) && a.when.every((w) => w && typeof w === "object" && typeof w.path === "string"))
      )
        return bad("when_invalid", "when 非法");
      const err = checkEmit(a.emit, new Set(Object.keys(a.fields)), rule.tier);
      if (err) return err;
      break;
    }
    case "dom": {
      const sel = checkSelector(a.selector);
      if (sel) return sel;
      if (typeof a.attr !== "string") return bad("attr_invalid", "attr 缺失");
      const err = checkEmit(a.emit, new Set(["value", "text", "page"]), rule.tier);
      if (err) return err;
      break;
    }
    case "manifest":
      if (a.path !== undefined && typeof a.path !== "string")
        return bad("manifest_path_invalid", "manifest.path 须为字符串");
      break;
    case "mirror":
      if (!Array.isArray(a.hosts) || a.hosts.length < 2) return bad("mirror_hosts_invalid", "mirror.hosts 至少两个");
      if (a.hosts.length > MAX_MIRROR_HOSTS)
        return bad("too_many", `mirror.hosts 最多 ${MAX_MIRROR_HOSTS} 个`, "mirror.hosts");
      for (const h of a.hosts) {
        if (typeof h !== "string" || !parseHostPattern(h)) return bad("mirror_hosts_invalid", "mirror.hosts 非法");
      }
      if (scoped) {
        const scope = [...(rule.match.host ?? []), ...(rule.match.pageHost ?? [])].flatMap((p) => parseHostPattern(p) ?? []);
        for (const h of a.hosts) {
          const domain = parseHostPattern(h)?.domain ?? "";
          if (!scope.some((p) => hostMatchesPattern(domain, p)))
            return bad("mirror_out_of_scope", `社区规则的 mirror.hosts 必须在 match.host / pageHost 范围内: ${h}`, h);
        }
      }
      break;
    case "title":
    case "player": {
      const sel = checkSelector(a.selector);
      if (sel) return sel;
      if (a.kind === "title" && a.attr !== undefined && typeof a.attr !== "string")
        return bad("attr_invalid", "attr 须为字符串");
      if (a.kind === "player") {
        if (a.float === "inset") {
          if (!Number.isInteger(a.top) || a.top < 1 || a.top > MAX_PLAYER_INSET_TOP)
            return bad("player_top_invalid", `player.top 须为 1–${MAX_PLAYER_INSET_TOP} 的整数`);
        } else if (a.float !== "hide") return bad("player_float_invalid", "player.float 非法");
      }
      if (!hasHost(rule.match) && rule.tier !== "local")
        return bad("page_scope_required", "page 规则必须限定 host 或 pageHost");
      break;
    }
    case "headers": {
      if (rule.tier === "plugin") return bad("headers_plugin_forbidden", "plugin 规则不得改写请求头");
      if (!a.set || typeof a.set !== "object" || Array.isArray(a.set)) return bad("headers_set_missing", "headers.set 缺失");
      const entries = Object.entries(a.set);
      if (entries.length === 0 || entries.length > ALLOWED_RULE_HEADERS.size)
        return bad("headers_count_invalid", "headers.set 数量非法");
      const seen = new Set<string>();
      for (const [name, value] of entries) {
        const lower = name.toLowerCase();
        if (!/^[a-z0-9-]{1,64}$/i.test(name)) return bad("header_name_invalid", "header 名非法", name);
        if (!ALLOWED_RULE_HEADERS.has(lower)) return bad("header_not_allowed", `不允许设置 ${name}`, name);
        if (seen.has(lower)) return bad("header_name_invalid", `header 名重复: ${name}`, name);
        seen.add(lower);
        if (typeof value !== "string" || value.length > 1024 || /[\r\n]/.test(value))
          return bad("header_value_invalid", "header 值非法", name);
      }
      // 只靠 pageHost 限定会把头注入该页面发出的所有第三方下载，必须限定目标 host。
      if (!(Array.isArray(rule.match.host) && rule.match.host.length > 0))
        return bad("headers_host_required", "headers 规则必须限定 match.host");
      break;
    }
  }
  return null;
}

/** 被剔除的单条规则：`rule` 为原始输入（可能畸形），供列表展示。 */
export interface RejectedRule {
  id: string;
  /** 稳定 snake_case 代码，UI 映射为本地化文案 */
  code: string;
  /** 出问题的字段 / 取值（可选） */
  param?: string;
  /** 中文原因原文（无本地化映射时回退展示） */
  reason: string;
  rule: unknown;
}

/**
 * 校验规则包。单条不合格剔除（记入 rejected），整包不合格抛 PackRejectedError。
 * `expectedTier` 为存储槽位对应的层级，不能取自包自己声明的 packId。
 */
export function validatePack(
  pack: RulePack,
  expectedTier: RuleTier,
): { pack: RulePack; rejected: RejectedRule[] } {
  if (!pack || typeof pack !== "object") throw new PackRejectedError("不是对象", "pack_not_object");
  if (pack.format !== "fluxdown-sniff-pack") throw new PackRejectedError("format 错误", "pack_format_invalid");
  if (!(expectedTier in TIER_RANK)) throw new PackRejectedError("expectedTier 非法", "pack_tier_invalid");
  if (pack.packId !== expectedTier) throw new PackRejectedError("packId 与预期层级不符", "pack_tier_mismatch");
  if (!Number.isInteger(pack.engine) || pack.engine > RULE_ENGINE_VERSION)
    throw new PackRejectedError("engine 版本过新或非法", "pack_engine_invalid");
  if (!Number.isInteger(pack.sequence) || pack.sequence < 0)
    throw new PackRejectedError("sequence 非法", "pack_sequence_invalid");
  if (!Array.isArray(pack.rules) || pack.rules.length > MAX_RULES_PER_PACK)
    throw new PackRejectedError("rules 非法或超过上限", "pack_rules_invalid");

  const seen = new Set<string>();
  const accepted: SniffRuleV1[] = [];
  const rejected: RejectedRule[] = [];
  for (const rule of pack.rules) {
    const id = typeof rule?.id === "string" ? rule.id : "";
    if (id && seen.has(id)) {
      rejected.push({ id, code: "id_duplicate", reason: "id 重复", rule });
      continue;
    }
    const problem = checkRule(rule, expectedTier);
    if (problem) {
      rejected.push({ id, code: problem.code, ...(problem.param === undefined ? {} : { param: problem.param }), reason: problem.detail, rule });
      continue;
    }
    seen.add(id);
    accepted.push(rule);
  }
  return { pack: { ...pack, rules: accepted }, rejected };
}
