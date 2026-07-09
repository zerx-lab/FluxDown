#!/usr/bin/env node
/**
 * indexnow-ping.mjs — 向 IndexNow 提交本站 URL(零运行时依赖)。
 *
 * IndexNow(Bing / Yandex / Seznam 等共享)可在 1-3 天内收录,远快于纯 sitemap 的
 * 数周;占 Bing 新 URL 发现约 17%。Google 不支持 IndexNow —— 其收录靠 sitemap +
 * 结构化数据,本脚本不影响 Google 路径。
 *
 * 从**线上** sitemap 抓取 URL(而非本地 dist/):
 *  - 部署方式为 Docker,runtime 容器仅含 dist/,无 scripts/;从线上抓取让本脚本
 *    可在宿主机 website/ 目录直接跑,不依赖容器文件布局。
 *  - 提交前 sitemap 已线上可访问,天然保证 key 文件(public/<KEY>.txt)也已上线,
 *    避免 IndexNow 422(key 校验失败)。
 *
 * 用法:部署完成、站点起来后 `node scripts/indexnow-ping.mjs`(见 deploy.sh 第 5 步)。
 *   跳过:设 INDEXNOW_SKIP=1。
 */

const HOST = "fluxdown.zerx.dev";
const KEY = "1f461e91dce4402097da1e673bb048de";
const ORIGIN = `https://${HOST}`;
const SITEMAP_INDEX = `${ORIGIN}/sitemap-index.xml`;
const ENDPOINT = "https://api.indexnow.org/indexnow";

if (process.env.INDEXNOW_SKIP === "1") {
  console.log("[indexnow] INDEXNOW_SKIP=1 — 跳过提交");
  process.exit(0);
}

/** 从 sitemap XML 文本提取所有 <loc> 值。 */
function extractLocs(xml) {
  const out = [];
  const re = /<loc>\s*([^<\s]+)\s*<\/loc>/g;
  let m;
  while ((m = re.exec(xml)) !== null) out.push(m[1].trim());
  return out;
}

async function fetchText(url) {
  const res = await fetch(url, { headers: { "User-Agent": "FluxDown-IndexNow/1" } });
  if (!res.ok) throw new Error(`HTTP ${res.status} @ ${url}`);
  return res.text();
}

try {
  // sitemap-index.xml 的 <loc> 指向各子 sitemap;逐个抓取取页面 URL。
  const indexXml = await fetchText(SITEMAP_INDEX);
  const childSitemaps = extractLocs(indexXml).filter((u) => /\.xml$/i.test(u));

  const urlSet = new Set();
  for (const sm of childSitemaps) {
    for (const loc of extractLocs(await fetchText(sm))) {
      if (!/\.xml$/i.test(loc) && loc.includes(HOST)) urlSet.add(loc);
    }
  }

  const urlList = [...urlSet];
  if (urlList.length === 0) {
    console.error("[indexnow] 线上 sitemap 无可提交 URL");
    process.exit(1);
  }

  const body = {
    host: HOST,
    key: KEY,
    keyLocation: `${ORIGIN}/${KEY}.txt`,
    urlList,
  };

  console.log(`[indexnow] 提交 ${urlList.length} 个 URL 到 ${ENDPOINT}`);

  const res = await fetch(ENDPOINT, {
    method: "POST",
    headers: { "Content-Type": "application/json; charset=utf-8" },
    body: JSON.stringify(body),
  });
  // IndexNow: 200/202 成功;422 = key 校验失败;429 = 频率限制。
  if (res.ok || res.status === 202) {
    console.log(`[indexnow] 成功 (HTTP ${res.status})`);
  } else {
    const text = await res.text().catch(() => "");
    console.error(`[indexnow] 失败 HTTP ${res.status}: ${text}`);
    process.exit(0); // 收录提交失败不应中断部署
  }
} catch (err) {
  console.error(`[indexnow] 错误: ${err?.message ?? err}`);
  process.exit(0);
}
