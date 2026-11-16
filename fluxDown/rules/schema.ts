/**
 * 嗅探规则 schema v1（纯类型 + 常量，零运行时依赖）。
 *
 * 规则包由「内置 / 官方 / 社区 / 本地」四个层级提供，求值器见 engine.ts。
 */
import type { ResourceType } from "../utils/resource-types";

export type { ResourceType };

export const RULE_ENGINE_VERSION = 1;

export type RuleTier = "builtin" | "official" | "plugin" | "community" | "local";

/** 层级优先级：数值越小越先求值（本地最先，内置最后）。 */
export const TIER_RANK: Record<RuleTier, number> = {
  local: 0,
  official: 1,
  plugin: 2,
  community: 3,
  builtin: 4,
};

/**
 * - response：响应分类 / 忽略；normalize：URL 归一（去参、扩展名、镜像域名）；track：音视频轨判定；
 * - body：API 响应 JSON 提取（emit 资源 / 声明清单）；dom：页面元素提取资源；
 * - page：页面级提示（播放元素标题、浮钮排除）；request：下载请求补充的请求头。
 */
export type RuleStage = "response" | "normalize" | "track" | "body" | "dom" | "page" | "request";

export interface RulePack {
  format: "fluxdown-sniff-pack";
  /** 必须等于每条规则的 tier */
  packId: RuleTier;
  /** 非负整数，同 packId 单调递增 */
  sequence: number;
  /** > RULE_ENGINE_VERSION 时整包跳过 */
  engine: number;
  /** ISO 8601 */
  issuedAt: string;
  /** 仅 community 使用 */
  expiresAt?: string;
  rules: SniffRuleV1[];
}

/**
 * 各谓词之间为 AND。数量上限（校验时强制）：url ≤ 64、ext ≤ 256、mime ≤ 64、header ≤ 8；
 * 单条规则序列化后 ≤ 16 KB。
 */
export interface MatchSpec {
  /**
   * "example.com" 匹配本域及子域；"=example.com" 只匹配本域；不支持 "*."。
   * 只比较 hostname：端口被忽略，FQDN 末尾的点被归一，IDN 按 punycode 比较。
   * community / plugin 规则的 host 至少含一个点（不得是 "com" 这类单标签）。
   */
  host?: string[];
  /** 同上，作用于顶层页面 host */
  pageHost?: string[];
  /** glob：语义与引擎 url_glob_match 一致（见 glob.ts）；大小写不敏感，作用于完整 URL（含 query） */
  url?: string[];
  /**
   * 作用于 URL pathname；不区分大小写；受安全子集约束（regex-safety.ts：无嵌套量词 / 歧义选择支 /
   * 相邻重叠量词 / 反向引用 / 环视）。非官方层级执行时输入超过 2048 字符视为不匹配。
   */
  pathRegex?: string;
  /** 小写、不带点；URL 扩展名取末段去掉 `;参数` 后的最后一个 `.ext`（1–10 位字母数字） */
  ext?: string[];
  /** URL 是否带可识别扩展名 */
  hasExt?: boolean;
  /** "video/*" 或精确值；去掉参数后小写比较 */
  mime?: string[];
  /** webRequest ResourceType；MAIN world 观察到的请求记作 "xmlhttprequest" */
  resourceType?: string[];
  /** 闭区间 */
  status?: [number, number];
  /** 字节；大小未知（-1/0）时视为满足 */
  minSize?: number;
  maxSize?: number;
  /** 响应头 Content-Disposition 是否为 attachment */
  attachment?: boolean;
  /** 响应头；name 小写；正则同 pathRegex 的限制（输入上限同样适用） */
  header?: { name: string; regex: string }[];
}

export interface SniffRuleV1 {
  /** /^(builtin|official|plugin|community|local)\.[a-z0-9][a-z0-9._-]{1,62}$/，前缀等于 tier */
  id: string;
  /** 正整数 */
  version: number;
  tier: RuleTier;
  title?: string;
  /** 默认 true */
  enabled?: boolean;
  /** 默认 0，越大越先 */
  priority?: number;
  stage: RuleStage;
  /** 仅在对应设置开启时参与 classifyResponse */
  requires?: "sniffImages";
  /** 只参与 classify()，不参与 classifyResponse() */
  classifyOnly?: boolean;
  match: MatchSpec;
  /** 任一命中即视为本条规则不匹配 */
  exclude?: MatchSpec[];
  action: RuleAction;
}

/** 只支持 ${field} 替换，没有表达式 */
export type Template = string;

export type RuleAction =
  | {
      kind: "resource";
      type: ResourceType;
      confidence?: "high" | "medium" | "low";
    }
  | { kind: "ignore" }
  | { kind: "strip-params"; params: string[] }
  | { kind: "filename"; extFromQuery: string[] }
  | {
      kind: "track";
      source: "filenameOrUrl";
      pattern: string;
      group: number;
      audio: string[];
      video?: string[];
      fallback: "video" | "audio" | "none";
    }
  | {
      kind: "extract";
      when?: { path: string; equals: string | number | boolean }[];
      each?: string;
      fields: Record<string, string>;
      emit: EmitSpec;
    }
  /**
   * 页面元素提取资源。模板字段：`value`（attr 取值；URL 型属性取解析后的绝对地址）、`text`（元素文本）、
   * `page`（规则匹配所用的页面 URL）。命中元素是（或包含）页面媒体元素时，产出的资源归属该元素。
   */
  | { kind: "dom"; selector: string; attr: string; emit: EmitSpec }
  /** JSON 中 `path`（缺省为根）处是 DASH 结构（video[] / audio[] 带 baseUrl），按清单处理。 */
  | { kind: "manifest"; path?: string }
  /** 同一媒体的镜像域名组：组内域名上路径相同的文件视为同一文件（清单合并、播放档识别）。 */
  | { kind: "mirror"; hosts: string[] }
  /** 播放元素的标题来源：取离该元素最近的匹配节点（`attr` 缺省取文本）。用于候选名与下载文件名。 */
  | { kind: "title"; selector: string; attr?: string }
  /**
   * 匹配 `selector`（或位于其中）的媒体元素的浮钮策略：`hide` 不显示浮钮（背景 / 广告 / 预览视频）；
   * `inset` 让贴顶部的浮钮离元素顶部至少 `top` px（整数 1–200），避开播放器自带的顶部控件条
   * （如迷你播放器的展开 / 关闭按钮），元素剩余高度放不下浮钮时隐藏。
   */
  | { kind: "player"; selector: string; float: "hide" }
  | { kind: "player"; selector: string; float: "inset"; top: number }
  /**
   * 下载这些 URL 时补充的请求头。名字只允许 {@link ALLOWED_RULE_HEADERS}；规则必须用 `match.host`
   * 限定目标（只写 pageHost 会把头注入该页面发出的所有第三方请求）。plugin 层不可用。
   */
  | { kind: "headers"; set: Record<string, string> };

export interface EmitSpec {
  mode: "resource" | "preempt";
  url: Template | { resolve: { base: Template; ref: Template; bareQueryPrefix?: string } };
  filename?: Template;
  /** 缺省走 classify(url) */
  type?: ResourceType;
  quality?: Template;
}

export interface RevocationDoc {
  format: "fluxdown-sniff-revocations";
  sequence: number;
  issuedAt: string;
  expiresAt: string;
  entries: {
    ruleId: string;
    versions: "*" | { max: number };
    action: "soft" | "hard";
    reason: string;
    ref?: string;
  }[];
}

export type RuleOverrides = Record<
  string,
  { enabled: boolean; ackRevocation?: number }
>;

/** 各 stage 允许的 action.kind。 */
export const STAGE_ACTIONS: Record<RuleStage, RuleAction["kind"][]> = {
  response: ["resource", "ignore"],
  normalize: ["strip-params", "filename", "mirror"],
  track: ["track"],
  body: ["extract", "manifest"],
  dom: ["dom"],
  page: ["title", "player"],
  request: ["headers"],
};

/**
 * headers 动作允许设置的请求头（白名单）：只有这些与「站点防盗链 / 内容协商」相关的头可以注入。
 * 不含 range（分片由下载引擎管理）、cookie / authorization（凭据）、host / content-length / connection
 * 等连接控制头，以及 transfer-encoding / sec-* / proxy-* 等浏览器保留头。
 */
export const ALLOWED_RULE_HEADERS: ReadonlySet<string> = new Set([
  "referer",
  "origin",
  "user-agent",
  "accept",
  "accept-language",
  "x-requested-with",
]);
