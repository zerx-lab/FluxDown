// FluxDown 独立 Bilibili 番剧插件。
//
// 两段式 resolver：
//   1. 番剧 season/media 链接 → 调 B 站番剧接口，返回正片/番外分集 manifest；
//   2. manifest 条目启动时 → 用 resolverItem(ep:<id>) 调 B 站播放接口取得 DASH
//      视频/音频短期直链，再交回 FluxDown 下载引擎。
//
// 订阅轮询不放在插件内：当前插件契约没有后台 timer 或主动 createTask API。
// 因此本插件保持独立、无 Rust 修改，先负责稳定的「分集发现 + 单集解析」。

var API_BASE = 'https://api.bilibili.com';
var USER_AGENT = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/131.0 Safari/537.36';
var MAX_EPISODES = 1000;
var VARIANT_HEIGHTS = [2160, 1440, 1080, 720, 480];
var MAX_VARIANTS = 8;
var AUTH_COOKIE_KEY = 'auth.cookie';

function setting(name, fallback) {
  var value = flux.settings[name];
  return value == null ? fallback : value;
}

function sanitizeFileName(name) {
  return (name || 'video')
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 120) || 'video';
}

function sanitizePath(name) {
  var value = sanitizeFileName(name).replace(/[.]/g, ' ');
  return value.slice(0, 80).trim();
}

function cookieHeaderFromNetscape(raw) {
  var lines = String(raw || '').split(/\r?\n/);
  var pairs = [];
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i].trim();
    if (!line) continue;
    // Netscape 导出会用 #HttpOnly_ 前缀标识 HttpOnly cookie；它不是注释。
    if (line.indexOf('#HttpOnly_') === 0) line = line.slice('#HttpOnly_'.length);
    else if (line[0] === '#') continue;
    var fields = line.split('\t');
    if (fields.length >= 7 && fields[5]) {
      pairs.push(fields[5] + '=' + fields.slice(6).join('\t'));
    }
  }
  return pairs.join('; ');
}

function cookieHeader(raw) {
  var value = String(raw || '').trim();
  if (!value) return '';
  if (/^#\s*(Netscape|HTTP Cookie File)/i.test(value) || /\t/.test(value)) {
    return cookieHeaderFromNetscape(value);
  }
  return value.replace(/[\r\n]+/g, ' ').trim();
}

async function effectiveCookie(ctx) {
  var raw = String((ctx && ctx.cookies) || '').trim();
  if (!raw) raw = String(setting('cookies', '') || '').trim();
  var cookie = cookieHeader(raw);

  // 显式输入优先于缓存。首次解析时保存规范化后的 Cookie，后续任务可以复用。
  if (cookie) {
    try {
      var previous = await flux.storage.get(AUTH_COOKIE_KEY);
      if (previous !== cookie) await flux.storage.set(AUTH_COOKIE_KEY, cookie);
    } catch (e) {}
    return cookie;
  }

  if (setting('reuseStoredSession', true)) {
    try {
      var stored = await flux.storage.get(AUTH_COOKIE_KEY);
      if (stored) return String(stored);
    } catch (e) {}
  }
  return '';
}

function requestHeaders(cookie) {
  var headers = {
    Accept: 'application/json, text/plain, */*',
    Referer: 'https://www.bilibili.com/',
    'User-Agent': USER_AGENT,
  };
  if (cookie) headers.Cookie = cookie;
  return headers;
}

async function apiGet(path, ctx, cookie) {
  var response;
  try {
    response = await flux.fetch({
      method: 'GET',
      url: API_BASE + path,
      headers: requestHeaders(cookie || await effectiveCookie(ctx)),
    });
  } catch (e) {
    throw new Error('Bilibili 接口请求失败: ' + String(e));
  }

  if (!response || response.status < 200 || response.status >= 300) {
    throw new Error('Bilibili 接口 HTTP 状态异常: ' + String(response && response.status));
  }

  var payload;
  try {
    payload = JSON.parse(response.body || '');
  } catch (e) {
    throw new Error('Bilibili 接口返回非法 JSON: ' + String(e));
  }
  if (!payload || Number(payload.code) !== 0) {
    throw new Error(
      'Bilibili 接口错误 code=' + String(payload && payload.code) +
      ': ' + String((payload && payload.message) || 'unknown')
    );
  }
  return payload.result || {};
}

function firstMatch(url, pattern) {
  var match = pattern.exec(url || '');
  return match && match[1] ? match[1] : '';
}

async function seasonIdFromUrl(url, ctx, cookie) {
  var seasonId = firstMatch(url, /\/bangumi\/play\/ss(\d+)/i);
  if (seasonId) return seasonId;

  var mediaId = firstMatch(url, /\/bangumi\/media\/md(\d+)/i);
  if (mediaId) {
    var media = await apiGet('/pgc/review/user?media_id=' + encodeURIComponent(mediaId), ctx, cookie);
    var mediaInfo = media.media || {};
    if (mediaInfo.season_id) return String(mediaInfo.season_id);
  }

  var query = /[?&]season_id=(\d+)/i.exec(url || '');
  return query && query[1] ? query[1] : '';
}

function episodeLabel(ep) {
  var index = ep.index_show || ep.title || ('EP' + String(ep.id || ''));
  var longTitle = ep.long_title || '';
  return longTitle && longTitle !== index ? index + ' ' + longTitle : index;
}

function collectEpisodes(result, includeExtras) {
  var groups = [];
  if (Array.isArray(result.episodes)) {
    groups.push({ title: '正片', episodes: result.episodes });
  }
  if (includeExtras && Array.isArray(result.section)) {
    for (var i = 0; i < result.section.length; i++) {
      var section = result.section[i];
      if (section && Array.isArray(section.episodes)) {
        groups.push({
          title: section.title || '番外',
          episodes: section.episodes,
        });
      }
    }
  }

  var seen = {};
  var items = [];
  for (var gi = 0; gi < groups.length; gi++) {
    var group = groups[gi];
    for (var ei = 0; ei < group.episodes.length; ei++) {
      var ep = group.episodes[ei] || {};
      var id = String(ep.id || ep.ep_id || '');
      if (!id || seen[id]) continue;
      seen[id] = true;
      items.push({
        id: 'ep:' + id,
        name: sanitizeFileName(episodeLabel(ep)),
        path: group.title === '正片' ? '' : sanitizePath(group.title),
        size: 0,
        kind: 'file',
      });
      if (items.length >= MAX_EPISODES) return items;
    }
  }
  return items;
}

async function resolveManifest(ctx) {
  var cookie = await effectiveCookie(ctx);
  var seasonId = await seasonIdFromUrl(ctx.url, ctx, cookie);
  if (!seasonId) {
    throw new Error('无法从 Bilibili 番剧链接识别 season_id 或 media_id');
  }

  var result = await apiGet(
    '/pgc/view/web/season?season_id=' + encodeURIComponent(seasonId),
    ctx,
    cookie
  );
  var title = sanitizeFileName(result.title || result.season_title || ('Bilibili ' + seasonId));
  var items = collectEpisodes(result, Boolean(setting('includeExtras', false)));
  if (!items.length) throw new Error('Bilibili 未返回可下载分集');

  return { manifest: { name: title, items: items } };
}

async function resolveEpisode(ctx) {
  var match = /^ep:(\d+)$/.exec(String(ctx.resolverItem || ''));
  if (!match) throw new Error('Bilibili 分集标识非法: ' + String(ctx.resolverItem || ''));
  var epId = match[1];
  var cookie = await effectiveCookie(ctx);
  var season = await apiGet('/pgc/view/web/season?ep_id=' + encodeURIComponent(epId), ctx, cookie);
  var episode = findEpisode(season, epId);
  var play = await apiGet(
    '/pgc/player/web/playurl?ep_id=' + encodeURIComponent(epId) +
    '&qn=127&fnval=4048&fourk=1&fnver=0&otype=json&platform=web',
    ctx,
    cookie
  );
  if (play.code != null && Number(play.code) !== 0) {
    throw new Error('Bilibili 播放接口错误 code=' + String(play.code));
  }
  var title = sanitizeFileName(
    (season.title || season.season_title || 'Bilibili') + ' ' +
    (episode ? episodeLabel(episode) : 'EP' + epId)
  );
  // 播放地址可能要求登录态；下载阶段也要带上同一份 Cookie。
  var headers = requestHeaders(cookie);
  var dash = play.dash || {};
  var videos = Array.isArray(dash.video) ? dash.video : [];
  var audios = Array.isArray(dash.audio) ? dash.audio : [];
  var audio = pickAudioTrack(audios);
  var quality = String(setting('quality', 'best'));
  var variants = buildDashVariants(videos, audio, title);
  var chosenIndex = chooseDashIndex(variants, quality);

  if (quality === 'audio' && audio) {
    return {
      url: audioUrl(audio),
      fileName: title + '.m4a',
      totalBytes: Number(audio.bandwidth) > 0 ? 0 : 0,
      extraHeaders: headers,
      ephemeral: true,
      rangeSupported: true,
      variants: variants,
      defaultVariantIndex: chosenIndex,
    };
  }

  if (variants.length) {
    var chosen = variants[chosenIndex] || variants[0];
    if (!chosen.url) throw new Error('Bilibili 播放接口未返回视频地址');
    return {
      url: chosen.url,
      audioUrl: chosen.audioUrl,
      fileName: chosen.fileName,
      totalBytes: chosen.totalBytes,
      extraHeaders: headers,
      ephemeral: true,
      rangeSupported: true,
      variants: variants,
      defaultVariantIndex: chosenIndex,
    };
  }

  var durl = Array.isArray(play.durl) ? play.durl : [];
  if (durl.length && durl[0] && durl[0].url) {
    return {
      url: durl[0].url,
      fileName: title + '.flv',
      totalBytes: Number(durl[0].size) > 0 ? Number(durl[0].size) : 0,
      extraHeaders: headers,
      ephemeral: true,
      rangeSupported: true,
    };
  }
  throw new Error('Bilibili 播放接口未返回 DASH 或 FLV 地址');
}

function findEpisode(result, epId) {
  var groups = [];
  if (Array.isArray(result.episodes)) groups.push(result.episodes);
  if (Array.isArray(result.section)) {
    for (var i = 0; i < result.section.length; i++) {
      if (result.section[i] && Array.isArray(result.section[i].episodes)) {
        groups.push(result.section[i].episodes);
      }
    }
  }
  for (var gi = 0; gi < groups.length; gi++) {
    for (var ei = 0; ei < groups[gi].length; ei++) {
      var episode = groups[gi][ei];
      if (String(episode.id || episode.ep_id || '') === String(epId)) return episode;
    }
  }
  return null;
}

function streamUrl(stream) {
  return String(stream.base_url || stream.baseUrl || stream.url || '');
}

function audioUrl(stream) {
  return streamUrl(stream);
}

function pickAudioTrack(audios) {
  var best = null;
  var score = -1;
  for (var i = 0; i < audios.length; i++) {
    var track = audios[i];
    if (!track || !streamUrl(track)) continue;
    var current = Number(track.bandwidth) || Number(track.id) || 0;
    if (current > score) {
      score = current;
      best = track;
    }
  }
  return best;
}

function videoHeight(video) {
  return Number(video.height) || Number(video.height_cm) || 0;
}

function buildDashVariants(videos, audio, title) {
  var candidates = [];
  var seen = {};
  for (var i = 0; i < videos.length; i++) {
    var video = videos[i];
    var url = streamUrl(video);
    var height = videoHeight(video);
    if (!url || !height || seen[height]) continue;
    seen[height] = true;
    candidates.push({ video: video, height: height });
  }
  candidates.sort(function(a, b) {
    if (b.height !== a.height) return b.height - a.height;
    return (Number(b.video.bandwidth) || 0) - (Number(a.video.bandwidth) || 0);
  });

  var variants = [];
  for (var ci = 0; ci < candidates.length && variants.length < MAX_VARIANTS; ci++) {
    var item = candidates[ci];
    var video = item.video;
    variants.push({
      label: item.height + 'p' + (video.codecs ? ' ' + String(video.codecs) : ''),
      url: streamUrl(video),
      audioUrl: audio ? audioUrl(audio) : '',
      fileName: title + '.mp4',
      totalBytes: 0,
      bandwidth: Number(video.bandwidth) || 0,
      width: Number(video.width) || 0,
      height: item.height,
      container: 'mp4',
    });
  }
  if (audio && variants.length < MAX_VARIANTS) {
    variants.push({
      label: 'Audio only (m4a)',
      url: audioUrl(audio),
      audioUrl: '',
      fileName: title + '.m4a',
      totalBytes: 0,
      bandwidth: Number(audio.bandwidth) || 0,
      width: 0,
      height: 0,
      container: 'm4a',
    });
  }
  return variants;
}

function chooseDashIndex(variants, quality) {
  if (!variants.length) return 0;
  if (quality === 'audio') {
    for (var i = 0; i < variants.length; i++) {
      if (variants[i].height === 0) return i;
    }
    return 0;
  }
  if (quality === 'best') return 0;
  var target = Number(quality) || 1080;
  var best = 0;
  var bestHeight = 0;
  for (var j = 0; j < variants.length; j++) {
    var height = Number(variants[j].height) || 0;
    if (height && height <= target && height >= bestHeight) {
      best = j;
      bestHeight = height;
    }
  }
  return best;
}

globalThis.resolve = async (ctx) => {
  if (ctx.resolverItem) return await resolveEpisode(ctx);
  return await resolveManifest(ctx);
};
