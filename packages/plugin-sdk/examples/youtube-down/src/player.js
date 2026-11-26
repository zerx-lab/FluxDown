// 播放器 JS 与签名 / n 挑战求解。
//
// 求解算法原样复用 yt-dlp-ejs（vendor/yt-dlp-ejs，打包时置于 resolve.js 顶部，全局 `ytSolver`）：
// 它用 meriyah 把整份播放器 JS（约 3 MB）解析成 AST，找出签名 / n 变换函数并执行。
// 首次解析某个播放器很重（QuickJS 里约 2.5 s、峰值约 180 MB 堆，见 manifest memoryLimitMb），
// 所以把 ejs 产出的「预处理播放器」缓存进插件工作区（flux.fs），之后同一播放器只需编译执行
// 预处理代码（约 0.2 s、约 60 MB 堆）。
import { BROWSER_UA } from "./clients.js";
import { ORIGIN } from "./innertube.js";

/** vendor 的 yt-dlp-ejs 版本；升级 vendor 时同步，旧版本的预处理缓存随之失效。 */
export const EJS_VERSION = "0.8.0";

const INDEX_KEY = "ejs.players";
const FILE_PREFIX = "ejs-player-";
/** 最多缓存的播放器数（YouTube 同时在 A/B 多个构建，每份约 4 MB）。 */
const MAX_CACHED = 3;
/**
 * 缓存的播放器在多久内可直接沿用（不管当前推的是哪个构建，与 yt-dlp `player_js_version=STS@ID`
 * 强制旧播放器同理：signatureTimestamp 告诉服务端按哪个播放器版本加密）。过期后重新确定当前播放器。
 */
const REUSE_MS = 24 * 3600 * 1000;

const PLAYER_ID_RE = /\/s\/player\/([0-9a-zA-Z_-]{8,})\//;

/**
 * 求解用的播放器 JS 变体：TV 播放器（yt-dlp 2026-11 起的默认 `player_js_variant=tv`）。同一播放器 id
 * 的各变体签名 / n 算法相同；网页主变体 `player_ias.vflset/en_US/base.js` 自 2026-11 起换成了
 * yt-dlp-ejs 0.8.0 解析不了的构建（求解时抛 `addOnDisposeCallback is not a function`）。
 * 缓存索引按变体区分，换变体时旧变体的预处理缓存随之失效。
 */
const PLAYER_VARIANT = "tv";

export function playerUrl(id) {
  return `${ORIGIN}/s/player/${id}/tv-player-ias.vflset/tv-player-ias.js`;
}

/** 播放器 JS 的 signatureTimestamp（yt-dlp 同一正则）。 */
export function extractSts(code) {
  const m = /(?:signatureTimestamp|sts)\s*:\s*([0-9]{5})/.exec(code || "");
  return m ? Number(m[1]) : 0;
}

function playerIdOf(ytcfg) {
  if (!ytcfg) return "";
  const configs = ytcfg.WEB_PLAYER_CONTEXT_CONFIGS && typeof ytcfg.WEB_PLAYER_CONTEXT_CONFIGS === "object" ? Object.values(ytcfg.WEB_PLAYER_CONTEXT_CONFIGS) : [];
  const url = [ytcfg.PLAYER_JS_URL, ...configs.map((c) => c && c.jsUrl)].find((u) => typeof u === "string" && u);
  const m = url ? PLAYER_ID_RE.exec(url) : null;
  return m ? m[1] : "";
}

/** 当前播放器 id：会话 ytcfg 里有就直接用，否则问 iframe_api（yt-dlp 的回退路径，约 1 KB）。 */
async function currentPlayerId(session) {
  const fromPage = playerIdOf(session.ytcfg);
  if (fromPage) return fromPage;
  const res = await session.request({ url: `${ORIGIN}/iframe_api`, headers: { "User-Agent": BROWSER_UA } });
  const m = res.status === 200 ? /player\\?\/([0-9a-fA-F]{8})\\?\//.exec(res.body) : null;
  if (!m) throw new Error(`无法确定 YouTube 播放器版本（iframe_api HTTP ${res.status}）`);
  return m[1];
}

async function loadIndex() {
  const raw = await flux.storage.get(INDEX_KEY);
  if (!raw) return [];
  try {
    const list = JSON.parse(raw);
    return Array.isArray(list)
      ? list.filter((e) => e && typeof e.id === "string" && Number(e.sts) > 0 && e.ejs === EJS_VERSION && e.variant === PLAYER_VARIANT)
      : [];
  } catch {
    return [];
  }
}

const fileOf = (id) => `${FILE_PREFIX}${id}.js`;

/** 写入预处理播放器并更新索引；淘汰超出上限与索引之外的旧文件。 */
async function savePlayer(id, sts, code, now) {
  await flux.fs.writeFile(fileOf(id), code);
  const index = (await loadIndex()).filter((e) => e.id !== id);
  index.unshift({ id, sts, saved: now, ejs: EJS_VERSION, variant: PLAYER_VARIANT });
  const kept = index.slice(0, MAX_CACHED);
  await flux.storage.set(INDEX_KEY, JSON.stringify(kept));
  const keep = new Set(kept.map((e) => fileOf(e.id)));
  for (const name of await flux.fs.list()) {
    if (name.startsWith(FILE_PREFIX) && !keep.has(name)) await flux.fs.remove(name);
  }
}

/** 求解失败。`cached` = 用的是缓存的预处理播放器（可能已失效 / 被并发淘汰），值得换当前播放器重试。 */
export class SolverError extends Error {
  constructor(message, cached) {
    super(message);
    this.cached = cached;
  }
}

function checkResults(kind, challenges, response, cached) {
  const what = kind === "n" ? "n 参数" : "签名";
  if (!response || response.type !== "result") {
    throw new SolverError(`YouTube ${what}求解失败：${(response && response.error) || "无结果"}`, cached);
  }
  const out = {};
  for (const c of challenges) {
    const v = response.data[c];
    // yt-dlp 判据：n 的结果以挑战值结尾说明求解函数抛错后原样返回。
    if (typeof v !== "string" || !v || (kind === "n" && v.endsWith(c))) {
      throw new SolverError(`YouTube ${what}求解结果无效`, cached);
    }
    out[c] = v;
  }
  return out;
}

function runSolver(input, cached) {
  let output;
  try {
    output = ytSolver(input);
  } catch (e) {
    throw new SolverError(`YouTube 播放器求解异常：${e instanceof Error ? e.message : String(e)}`, cached);
  }
  if (!output || output.type !== "result") {
    throw new SolverError(`YouTube 播放器解析失败：${(output && output.error) || "无输出"}`, cached);
  }
  return output;
}

export class Player {
  constructor({ id, sts, raw = null }) {
    this.id = id;
    this.sts = sts;
    /** 冷启动时的原始播放器 JS；求解一次后转为缓存。 */
    this.raw = raw;
  }

  /**
   * 打开播放器。非 `fresh`：会话 ytcfg 指明的当前播放器有缓存就用它，否则直接用 24 h 内最近缓存的
   * 播放器（不必先问 iframe_api 当前是哪个）。`fresh` 或没有可用缓存：确定当前播放器，有缓存用缓存，
   * 没有则下载（冷启动）。
   */
  static async open(session, { fresh = false } = {}) {
    const index = await loadIndex();
    const now = session.now();
    const known = playerIdOf(session.ytcfg);
    const reusable = fresh ? null : (known && index.find((e) => e.id === known)) || index.find((e) => now - Number(e.saved) < REUSE_MS);
    if (reusable) return new Player({ id: reusable.id, sts: Number(reusable.sts) });

    const id = await currentPlayerId(session);
    const cached = index.find((e) => e.id === id);
    if (cached) return new Player({ id: cached.id, sts: Number(cached.sts) });

    const res = await session.request({ url: playerUrl(id), headers: { "User-Agent": BROWSER_UA } });
    if (res.status !== 200 || !res.body) throw new Error(`下载 YouTube 播放器失败（HTTP ${res.status}）`);
    const sts = extractSts(res.body);
    if (!sts) throw new Error(`YouTube 播放器 ${id} 中找不到 signatureTimestamp`);
    return new Player({ id, sts, raw: res.body });
  }

  /** 冷启动的播放器先做预处理并缓存（播放列表借此让后续条目都走缓存）。 */
  async warm(now) {
    if (this.raw !== null) await this.solve([], [], now);
  }

  /** 批量求解：返回 `{ n: {挑战: 结果}, sig: {挑战: 结果} }`。 */
  async solve(nChallenges, sigChallenges, now) {
    try {
      return await this.solveOnce(nChallenges, sigChallenges, now);
    } catch (e) {
      if (e instanceof SolverError) e.playerId = this.id;
      throw e;
    }
  }

  async solveOnce(nChallenges, sigChallenges, now) {
    const requests = [
      { type: "n", challenges: nChallenges },
      { type: "sig", challenges: sigChallenges },
    ];
    const cached = this.raw === null;
    let output;
    if (!cached) {
      output = runSolver({ type: "player", player: this.raw, output_preprocessed: true, requests }, false);
      this.raw = null;
      if (output.preprocessed_player) await savePlayer(this.id, this.sts, output.preprocessed_player, now);
    } else {
      if (nChallenges.length === 0 && sigChallenges.length === 0) return { n: {}, sig: {} };
      const code = await flux.fs.readFile(fileOf(this.id));
      if (code === null) throw new SolverError(`播放器缓存 ${this.id} 已被清理`, true);
      output = runSolver({ type: "preprocessed", preprocessed_player: code, requests }, true);
    }
    return {
      n: nChallenges.length ? checkResults("n", nChallenges, output.responses[0], cached) : {},
      sig: sigChallenges.length ? checkResults("sig", sigChallenges, output.responses[1], cached) : {},
    };
  }

  /** 丢弃某个播放器的缓存（求解失败时，免得重试再次命中它）。 */
  static async forget(id) {
    const index = await loadIndex();
    if (index.some((e) => e.id === id)) await flux.storage.set(INDEX_KEY, JSON.stringify(index.filter((e) => e.id !== id)));
    if ((await flux.fs.list()).includes(fileOf(id))) await flux.fs.remove(fileOf(id));
  }
}
