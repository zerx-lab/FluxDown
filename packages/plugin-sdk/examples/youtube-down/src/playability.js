// 播放状态与报错文案（yt-dlp `_video.py` 取 reason / subreason 的顺序，换成面向用户的中文提示）。

function textOf(node) {
  if (!node) return "";
  if (typeof node === "string") return node;
  if (typeof node.simpleText === "string") return node.simpleText;
  if (Array.isArray(node.runs)) return node.runs.map((r) => (r && r.text) || "").join("");
  if (typeof node.content === "string") return node.content;
  return "";
}

/** 某个 /player 响应的拒绝原因（OK 返回空串）。 */
export function denialOf(response) {
  const ps = (response && response.playabilityStatus) || {};
  if (ps.status === "OK") return "";
  const screen = ps.errorScreen || {};
  const pem = screen.playerErrorMessageRenderer || {};
  const ivm = (((screen.playerInterstitialRenderer || {}).content || {}).interstitialViewModel || {});
  const reason = textOf(pem.reason) || String(ps.reason || "") || textOf(ivm.title);
  const sub = textOf(pem.subreason) || textOf(ivm.description);
  return [reason, sub && sub !== reason ? sub : ""].filter(Boolean).join(". ") || String(ps.status || "");
}

const AGE_RE = /confirm your age|age-restricted|inappropriate|age_verification_required|age_check_required/i;

export function isAgeGated(response) {
  const ps = (response && response.playabilityStatus) || {};
  return Boolean(ps.desktopLegacyAgeGateReason) || AGE_RE.test(`${ps.status || ""} ${ps.reason || ""}`);
}

/** UNPLAYABLE / ERROR（visionos 对「为儿童打造」视频的表现，可改用 web_embedded）。 */
export function isUnplayable(response) {
  const status = response && response.playabilityStatus && response.playabilityStatus.status;
  return status === "UNPLAYABLE" || status === "ERROR";
}

/** 没有任何可下载格式时，把各客户端的拒绝原因折算成一条提示。 */
export function friendlyDenial(reasons, withCookies) {
  const text = [...new Set(reasons.filter(Boolean))].join(" | ");
  if (/not a bot|Sign in to confirm/i.test(text)) {
    return withCookies
      ? "YouTube 要求验证「不是机器人」，当前 Cookie 未通过。请在浏览器重新登录 YouTube 后再从浏览器发起下载。"
      : "YouTube 要求验证「不是机器人」（当前网络 / IP 被风控）。请登录 YouTube 后从浏览器扩展发起下载（会自动携带 Cookie），或在插件设置里填写备用 Cookie。";
  }
  if (/Private video|members-only|Join this channel|age.?restricted|confirm your age|inappropriate for some users/i.test(text)) {
    return `视频受限（私享 / 会员 / 年龄限制），需要有权限的登录 Cookie。${text}`;
  }
  if (/not made this video available in your country|not available in your country/i.test(text)) {
    return `视频在当前地区不可用（地区限制）。${text}`;
  }
  if (/Video unavailable|This video is not available|has been removed|terminated/i.test(text)) {
    return `视频不可用：${text}`;
  }
  return text ? `YouTube 拒绝播放：${text}` : "YouTube 没有返回可下载的格式";
}

/** 合并各客户端响应里的元数据（取第一个有值的）。 */
export function videoInfo(responses) {
  const details = responses.map((r) => (r.response && r.response.videoDetails) || {});
  const first = (pick) => details.map(pick).find((v) => v !== undefined && v !== null && v !== "");
  const live = (key) => details.some((d) => d[key] === true);
  return {
    title: first((d) => d.title) || "",
    creatorId: first((d) => d.channelId) || "",
    creatorName: first((d) => d.author) || "",
    duration: Number(first((d) => d.lengthSeconds)) || 0,
    liveStatus: live("isPostLiveDvr") ? "post_live" : live("isLive") ? "is_live" : live("isUpcoming") ? "is_upcoming" : "",
  };
}
