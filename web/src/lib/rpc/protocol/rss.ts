// RSS 订阅 DTO。

/** `RssItemDto.status` 取值。 */
export const RSS_ITEM_STATUS = {
  new: 0,
  downloaded: 1,
  ignored: 2,
  /** 规则未命中。 */
  filtered: 3,
  /** 重复剧集。 */
  duplicateEpisode: 4,
  /** 首轮抓取的历史条目。 */
  seeded: 5,
} as const;

/** 一个 RSS 订阅（服务端输出，所有字段必带）。 */
export interface RssSourceDto {
  sourceId: string;
  /** 来源 provider 稳定 ID；内置 RSS 为 `rss`。 */
  providerId: string;
  /** provider 专属配置 JSON 字符串。 */
  providerConfig: string;
  url: string;
  /** 空 = 用 feed 标题回填。 */
  name: string;
  enabled: boolean;
  /** false = 收集模式（只收集条目供手动挑选）。 */
  autoDownload: boolean;
  /** 自动创建的任务以 paused 落库。 */
  startPaused: boolean;
  /** 空 = 内置主队列。 */
  queueId: string;
  /** 空 = 队列目录 → 全局目录。 */
  saveDir: string;
  /** 抓取间隔（分钟）；0 = 引擎默认 30。 */
  intervalMinutes: number;
  /** 包含关键词（`|` = 或，空格 = 且；空 = 不过滤）。 */
  includePattern: string;
  excludePattern: string;
  useRegex: boolean;
  smartEpisode: boolean;
  /** 体积下限（字节，0 = 不限）。 */
  sizeMinBytes: number;
  /** 体积上限（字节，0 = 不限）。 */
  sizeMaxBytes: number;
  sendReferer: boolean;
  notifyOnDownload: boolean;
  /** 每轮最多新建任务数（1..=100）；0 = 引擎默认 20。 */
  maxPerFetch: number;
  cookies: string;
  userAgent: string;
  proxyUrl: string;
  /** 只读：上次发起抓取的 Unix 秒（0 = 从未）。 */
  lastFetchAt: number;
  /** 只读：上次成功抓取的 Unix 秒（0 = 从未）。 */
  lastSuccessAt: number;
  /** 只读：上次失败原因（空 = 健康）。 */
  lastError: string;
  /** 只读：连续失败次数。 */
  failCount: number;
  /** 只读：首轮抓取是否已完成。 */
  seeded: boolean;
  position: number;
  /** 只读：未处理条目数。 */
  unreadCount: number;
}

/**
 * 创建/更新订阅时提交的字段：仅 `url` 必填，其余缺省取服务端默认
 * （`providerId` 默认 `rss`，`enabled`/`autoDownload`/`sendReferer`/`notifyOnDownload` 默认 true）。
 * 只读运行态字段（lastFetchAt 等）会被引擎忽略。更新时必须带非空 `sourceId`。
 */
export type RssSourceInput = Partial<RssSourceDto> & { url: string };

/** 订阅流条目。 */
export interface RssItemDto {
  sourceId: string;
  /** 去重主键（常常是整条 URL）。 */
  guid: string;
  title: string;
  link: string;
  /** enclosure 直链（空 = 回退 `link`）。 */
  enclosureUrl: string;
  /** enclosure 声明大小（字节，0 = 未知）。 */
  enclosureLength: number;
  /** 发布时间（Unix 秒，0 = 未知）。 */
  pubDate: number;
  fetchedAt: number;
  /** 见 {@link RSS_ITEM_STATUS}。 */
  status: number;
  /** `status == 1` 时回链的任务 ID。 */
  taskId: string;
  /** 智能剧集归一键（空 = 未识别）。 */
  episodeKey: string;
  /** 稳定原因码（`excluded` / `too_large` / `dup_episode` / `seed_skipped` …，空 = 无），客户端负责本地化。 */
  reason: string;
  /** 可查看的图片数（0 时省略），不传输图片 URL。 */
  imageCount?: number;
}

/** 订阅地址验证请求（只读，不落库）；`providerId` 缺省为内置 RSS。 */
export interface RssValidateRequest {
  /** 订阅来源 provider；缺省 / 空 = 内置 `rss`。 */
  providerId?: string;
  /** provider 专属配置 JSON 字符串；缺省为空。 */
  providerConfig?: string;
  url: string;
  cookies?: string;
  userAgent?: string;
  proxyUrl?: string;
}

/** `error` 非空即验证失败——这是诊断载荷而非传输错误。 */
export interface RssValidateResponse {
  url: string;
  feedTitle: string;
  items: RssItemDto[];
  error: string;
}
