// YouTube 下载插件的 resolver（打包成 classic script，入口挂 globalThis.resolve；`flux` 由引擎注入）。
//
// 自研解析，不依赖外部程序：
//   1. 链接归一成视频 / 播放列表；
//   2. 按 yt-dlp 的客户端策略调 InnerTube /player（无 PO Token 即可取直链的 TV / visionOS /
//      嵌入式客户端），签名与 n 挑战交给打包进来的 yt-dlp-ejs 求解；
//   3. 挑出「视频轨 + 同容器音频轨」交回引擎多段下载、合并；播放列表返回清单，引擎按条目
//      裂变子任务，每个子任务再走一次二段解析。
import { CLIENTS, DEFAULT_AUTHED_CLIENTS, DEFAULT_CLIENTS, parseClientSetting } from "./clients.js";
import { buildOptions, collectStreams, finalizeStreams, pickOption, toVariant } from "./formats.js";
import { Session } from "./innertube.js";
import { denialOf, friendlyDenial, isAgeGated, isUnplayable, videoInfo } from "./playability.js";
import { Player, SolverError } from "./player.js";
import { fetchPlaylist } from "./playlist.js";
import { PRESETS, parseItem, parseTarget } from "./target.js";

/** 播放列表条目的可选规格；首项为引擎自动全选时使用的默认规格。 */
const ITEM_PRESETS = [
  { id: "best", label: "最高画质" },
  { id: "1080", label: "≤ 1080p" },
  { id: "720", label: "≤ 720p" },
  { id: "480", label: "≤ 480p" },
  { id: "audio", label: "仅音频" },
];
/** 列出播放列表后还剩这么多墙钟时间才顺手预热播放器缓存（resolver 墙钟顶 30 s）。 */
const WARM_DEADLINE_MS = 12000;

function sanitizeFileName(name, fallback) {
  const clean = String(name || "")
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+/, "")
    .slice(0, 120)
    .trim();
  return clean || fallback;
}

/** 任务携带的 Cookie（浏览器扩展发起的下载）优先，其次是设置里的备用 Cookie。 */
function cookieText(ctx) {
  const fromTask = flux.settings.useTaskCookies !== false ? String(ctx.cookies || "").trim() : "";
  return fromTask || String(flux.settings.cookies || "").trim();
}

async function playerResponses(session, videoId, clients, sts) {
  return Promise.all(
    clients.map(async (client) => {
      try {
        const response = await session.playerResponse(CLIENTS[client], videoId, sts);
        const got = response.videoDetails && response.videoDetails.videoId;
        if (got && got !== videoId) return { client, response: null, error: `${client} 返回了另一个视频（IP 可能被 YouTube 限制）` };
        return { client, response, error: "" };
      } catch (e) {
        return { client, response: null, error: `${client}: ${e.message}` };
      }
    }),
  );
}

/** 取一个视频的元数据与可下载格式。`fresh` 跳过「沿用其他缓存播放器」。 */
async function extract(ctx, videoId, fresh) {
  const cookies = cookieText(ctx);
  const session = new Session(cookies);
  // 分段耗时（毫秒），随结果日志输出，排查「解析慢」时看是网络还是求解。
  const timings = [];
  let mark = Date.now();
  const lap = (name) => {
    const now = Date.now();
    timings.push(`${name}=${now - mark}`);
    mark = now;
  };
  if (session.authenticated) {
    try {
      await session.loadSessionConfig(videoId);
    } catch (e) {
      flux.logger.warn("[youtube-down] 会话配置获取失败，按无会话信息继续:", e.message);
    }
    lap("session");
  }
  const player = await Player.open(session, { fresh });
  lap(player.raw === null ? "player" : "player(download)");
  const clients = parseClientSetting(flux.settings.playerClients) || (session.authenticated ? DEFAULT_AUTHED_CLIENTS : DEFAULT_CLIENTS);
  const results = await playerResponses(session, videoId, clients, player.sts);
  // 年龄限制 / 「为儿童打造」（visionOS 不可播）：可嵌入的视频改用嵌入式播放器（yt-dlp 同一回退）。
  const fallback = results.some(
    (r) => r.response && (isAgeGated(r.response) || (r.client === "visionos" && isUnplayable(r.response))),
  );
  if (fallback && !clients.includes("web_embedded")) {
    results.push(...(await playerResponses(session, videoId, ["web_embedded"], player.sts)));
  }

  const responses = results.filter((r) => r.response);
  const info = videoInfo(responses);
  if (info.liveStatus === "is_live" || info.liveStatus === "is_upcoming") {
    throw new Error("直播尚未结束，结束后可下载回放");
  }
  const { streams, nChallenges, sigChallenges } = collectStreams(responses);
  if (streams.length === 0) {
    const reasons = [...responses.map((r) => denialOf(r.response)), ...results.map((r) => r.error)].filter(Boolean);
    if (reasons.length === 0 && info.liveStatus === "post_live") throw new Error("直播回放仍在处理中，稍后再试");
    throw new Error(friendlyDenial(reasons, cookies !== ""));
  }
  lap("api");
  const cold = player.raw !== null;
  const solved = await player.solve(nChallenges, sigChallenges, session.now());
  lap(cold ? "solve(cold)" : "solve");
  flux.logger.info("[youtube-down]", videoId, "player", player.id, "clients", responses.map((r) => r.client).join(","), "streams", streams.length, "ms", timings.join(" "));
  return { info, formats: finalizeStreams(streams, solved) };
}

async function resolveVideo(ctx, videoId, preset, interactive) {
  let extracted;
  try {
    extracted = await extract(ctx, videoId, false);
  } catch (e) {
    // 缓存的播放器失效 / 被并发淘汰：丢掉这份缓存，换当前播放器冷启动重来一次。
    if (!(e instanceof SolverError) || !e.cached) throw e;
    flux.logger.warn("[youtube-down] 缓存播放器求解失败，改用当前播放器重试:", e.message);
    await Player.forget(e.playerId);
    extracted = await extract(ctx, videoId, true);
  }
  const { info, formats } = extracted;
  const base = sanitizeFileName(info.title, videoId);
  const archiveMetadata = { platform: "youtube", mediaKind: "post" };
  if (info.creatorId) archiveMetadata.creatorId = String(info.creatorId);
  if (info.creatorName) archiveMetadata.creatorName = String(info.creatorName);
  const options = buildOptions(formats, String(flux.settings.codec || "compat"), base);
  if (options.length === 0) throw new Error("没有可直接下载的格式（可能被风控或需要登录 Cookie）");

  const chosen = pickOption(options, preset);
  const top = toVariant(chosen);
  flux.logger.info("[youtube-down]", videoId, preset, "→", chosen.label, `(${chosen.video.client})`);
  const result = {
    url: top.url,
    audioUrl: top.audioUrl,
    fileName: top.fileName,
    totalBytes: top.totalBytes || undefined,
    // googlevideo 直链带签名与过期时间：跳过探测（探测会浪费一次性直链），并担保支持 Range。
    ephemeral: true,
    rangeSupported: true,
    archiveMetadata,
  };
  if (interactive && preset === "ask" && options.length > 1) {
    // variants[0] 即最高画质，弹框超时 / 免打扰 / headless 时按它下载。
    result.variants = options.map(toVariant);
    result.defaultVariantIndex = 0;
  }
  return result;
}

async function listPlaylist(ctx, playlistId) {
  const started = Date.now();
  const session = new Session(cookieText(ctx));
  const limit = Math.min(1000, Math.max(1, Math.floor(Number(flux.settings.playlistLimit) || 200)));
  const { title, author, entries } = await fetchPlaylist(session, playlistId, limit);
  if (entries.length === 0) throw new Error("播放列表为空，或其中的视频都不可用");
  const quality = String(flux.settings.quality || "ask");
  const first = ITEM_PRESETS.find((p) => p.id === quality) || ITEM_PRESETS[0];
  const variants = [first, ...ITEM_PRESETS.filter((p) => p !== first)];
  const items = entries.map((e) => ({
    id: e.id,
    name: `${sanitizeFileName(e.title, e.id)}.${first.id === "audio" ? "m4a" : "mp4"}`,
    kind: "file",
    variants,
  }));
  flux.logger.info("[youtube-down] playlist", playlistId, items.length, "items");

  // 建任务那次解析（随后裂变出的子任务会并发解析）：先把播放器预处理进缓存，免得每个子任务各自冷启动。
  if (ctx.taskId && Date.now() - started < WARM_DEADLINE_MS) {
    try {
      await (await Player.open(session)).warm(session.now());
    } catch (e) {
      flux.logger.warn("[youtube-down] 播放器预热失败（子任务会各自重试）:", e.message);
    }
  }
  return {
    manifest: { name: sanitizeFileName(title, playlistId), items, archiveMetadata: { platform: "youtube", mediaKind: "post", collectionId: playlistId, collectionTitle: title, ...(author ? { creatorName: author } : {}) } },
  };
}

globalThis.resolve = async (ctx) => {
  const item = parseItem(ctx.resolverItem);
  if (item) return resolveVideo(ctx, item.videoId, item.preset, false);

  const target = parseTarget(ctx.url);
  if (!target) throw new Error(`不是可下载的 YouTube 视频或播放列表链接：${ctx.url}`);
  if (target.playlistId) return listPlaylist(ctx, target.playlistId);
  // 新建下载对话框的前置预解析（还没有任务）只关心播放列表清单；单个视频不必白跑一次解析。
  if (!ctx.taskId) return null;
  const quality = String(flux.settings.quality || "ask");
  return resolveVideo(ctx, target.videoId, PRESETS.includes(quality) || quality === "ask" ? quality : "ask", true);
};
