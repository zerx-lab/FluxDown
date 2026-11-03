// 播放列表：InnerTube browse（`VL<id>`）+ 续页（对齐 yt-dlp `_tab.py`；2026 版页面条目为
// lockupViewModel，旧版 playlistVideoRenderer 一并识别）。
import { WEB_CLIENT } from "./clients.js";

const VIDEO_ID_RE = /^[A-Za-z0-9_-]{11}$/;
/** 含不可用条目（私享 / 已删除也列出，再由下面的判据剔除），yt-dlp 同一参数。 */
const BROWSE_PARAMS = "wgYCCAA=";
const UNAVAILABLE_TITLES = new Set(["[Private video]", "[Deleted video]", "[Unavailable video]"]);

function textOf(node) {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (typeof node.content === "string") return node.content;
  if (typeof node.simpleText === "string") return node.simpleText;
  if (Array.isArray(node.runs)) return node.runs.map((r) => (r && r.text) || "").join("");
  return "";
}

function firstToken(node) {
  if (Array.isArray(node)) {
    for (const v of node) {
      const t = firstToken(v);
      if (t) return t;
    }
    return "";
  }
  if (!node || typeof node !== "object") return "";
  if (node.continuationCommand && typeof node.continuationCommand.token === "string") return node.continuationCommand.token;
  for (const v of Object.values(node)) {
    const t = firstToken(v);
    if (t) return t;
  }
  return "";
}

/** 遍历一页内容：条目按出现顺序收集，续页 token 取第一个。 */
export function parsePage(root) {
  const entries = [];
  let token = "";
  const walk = (node) => {
    if (Array.isArray(node)) {
      for (const v of node) walk(v);
      return;
    }
    if (!node || typeof node !== "object") return;
    for (const [key, value] of Object.entries(node)) {
      if (!value || typeof value !== "object") continue;
      if (key === "lockupViewModel") {
        if (value.contentType === "LOCKUP_CONTENT_TYPE_VIDEO") {
          const md = (value.metadata && value.metadata.lockupMetadataViewModel) || {};
          entries.push({ id: String(value.contentId || ""), title: textOf(md.title), playable: Boolean(textOf(md.title)) });
        }
      } else if (key === "playlistVideoRenderer") {
        entries.push({ id: String(value.videoId || ""), title: textOf(value.title), playable: value.isPlayable !== false });
      } else if (key === "continuationItemRenderer" || key === "continuationItemViewModel") {
        token = token || firstToken(value);
      } else {
        walk(value);
      }
    }
  };
  walk(root);
  return { entries, token };
}

function alertText(data) {
  const alerts = Array.isArray(data && data.alerts) ? data.alerts : [];
  for (const a of alerts) {
    const r = a && (a.alertRenderer || a.alertWithButtonRenderer);
    if (r && r.type === "ERROR") return textOf(r.text);
  }
  return "";
}

/** 列出播放列表：返回 `{ title, entries: [{id, title}] }`，最多 `limit` 条可播放视频（去重）。 */
export async function fetchPlaylist(session, playlistId, limit) {
  const first = await session.api(WEB_CLIENT, "browse", { browseId: `VL${playlistId}`, params: BROWSE_PARAMS });
  if (!first.contents) {
    const alert = alertText(first);
    throw new Error(alert ? `播放列表不可访问：${alert}` : "播放列表不存在，或需要登录才能查看");
  }
  const metadata = (first.metadata && first.metadata.playlistMetadataRenderer) || {};
  const title = textOf(metadata.title) || playlistId;
  const author = textOf(metadata.author);
  const seen = new Set();
  const entries = [];
  const take = (page) => {
    for (const e of page.entries) {
      if (entries.length >= limit) return;
      if (!VIDEO_ID_RE.test(e.id) || seen.has(e.id) || !e.playable || UNAVAILABLE_TITLES.has(e.title)) continue;
      seen.add(e.id);
      entries.push({ id: e.id, title: e.title });
    }
  };
  let page = parsePage(first.contents);
  take(page);
  const tokens = new Set();
  while (page.token && entries.length < limit && !tokens.has(page.token)) {
    tokens.add(page.token);
    const next = await session.api(WEB_CLIENT, "browse", { continuation: page.token });
    page = parsePage(next.onResponseReceivedActions || next.onResponseReceivedEndpoints || next.continuationContents || []);
    take(page);
  }
  return { title, author, entries };
}
