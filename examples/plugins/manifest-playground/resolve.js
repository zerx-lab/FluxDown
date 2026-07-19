// FluxDown 测试插件：多嵌套文件夹/多文件清单（两段式 resolver，classic script）。
//
// 初段（ctx.resolverItem 为空）：返回 { manifest: { name, items } } 清单，
//   数据集镜像 design/desktop-task-views/manifest.js 的 mock（选择弹窗设计原型），
//   逐项覆盖边界：
//   - 剧集三件套 ×24（正片 mkv / 双语 ass / 纯英 srt，路径「正片」「字幕/中英双语」「字幕/纯英文」）
//   - 8 级深链（制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL——
//     单链目录在弹窗中应合并为一行并一次跳级进入）
//   - 每级都有直属文件的分叉目录（制作资料/字幕工程/…——不可合并，逐层下钻）
//   - 根级散件（path=""）、大小未知（省略 size）、超长文件名（省略号渲染）
//   - 规格变体（花絮 mp4 带 1080p/720p variants——v1.6 弹窗不展示，
//     供 REST 预解析面与引擎自动裂变「取首个规格」路径验证）
//   - stress 数据集：40 卷 × 25 文件 = 1000 项（契约上限，虚拟化压测）
//
// 二段（ctx.resolverItem 非空）：返回 { url: target + "?item=<resolverItem>" }。
//   target 留空则返回 null 放行原始链接（manifest.test 无法解析，下载会失败，
//   仅用于验证 UI/建组/稍后下载流程；要端到端下载成功请在插件设置里
//   把「二段直链」指向任意可下载的 http(s) 文件）。
//
// 使用方法：设置 → 插件 → 开发者模式安装本目录；新建下载输入
//   https://manifest.test/demo 即可唤起清单选择弹窗。

const KB = 1024, MB = 1024 * KB, GB = 1024 * MB;

/** 构造一个清单条目；size 传 null/undefined 表示大小未知（省略字段）。 */
function item(id, name, path, size, variants) {
  const it = { id, name, path };
  if (size != null) it.size = Math.round(size);
  if (variants) it.variants = variants;
  return it;
}

/** 嵌套边界样例（98 项）。 */
function normalDataset() {
  const items = [];
  let n = 0;
  const next = () => "mi" + (++n);

  for (let i = 1; i <= 24; i++) {
    const ep = "S02E" + String(i).padStart(2, "0");
    items.push(item(next(), `沙丘·第二季.${ep}.2160p.BluRay.mkv`, "正片", (5.4 + i * 0.04) * GB));
    items.push(item(next(), `沙丘·第二季.${ep}.简繁英双语.ass`, "字幕/中英双语", 96 * KB + i * 3 * KB));
    items.push(item(next(), `Dune.Part.Two.${ep}.eng.srt`, "字幕/纯英文", 64 * KB + i * 2 * KB));
  }
  ["幕后制作特辑.mp4", "沙漠实景拍摄花絮.mp4", "主创访谈_导演篇.mp4", "删减片段合集.mp4"].forEach((name, i) => {
    const size = (620 + i * 90) * MB;
    items.push(item(next(), name, "花絮", size, [
      { id: "1080p", label: "1080P 原画", size: Math.round(size) },
      { id: "720p", label: "720P", size: Math.round(size * 0.45) },
    ]));
  });
  for (let i = 1; i <= 3; i++) {
    items.push(item(next(), `沙丘S02E0${i}_TrueHD_Atmos_7.1.mka`, "音轨/杜比全景声 TrueHD", (1.1 + i * 0.1) * GB));
  }
  for (let i = 1; i <= 8; i++) {
    items.push(item(next(), `剧照_${String(i).padStart(2, "0")}.jpg`, "海报与剧照", (3 + i) * MB));
  }
  // 8 级深链（无中间文件 → 弹窗单链合并为一行、一次跳级进入）
  items.push(item(next(), "00055.m2ts", "制作资料/原盘结构样例/BDMV/STREAM", 890 * MB));
  items.push(item(next(), "00001.clpi", "制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL", 12 * KB));
  items.push(item(next(), "index.bdmv", "制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL", 4 * KB));
  // 每级都有直属文件的深层目录（不可合并，深度只体现在面包屑）
  items.push(item(next(), "字幕工程说明.txt", "制作资料/字幕工程", 3 * KB));
  items.push(item(next(), "分轨命名规范.txt", "制作资料/字幕工程/分轨", 2 * KB));
  items.push(item(next(), "EP01_打轴笔记.txt", "制作资料/字幕工程/分轨/EP01", 5 * KB));
  items.push(item(next(), "中文样式表.ass", "制作资料/字幕工程/分轨/EP01/中文", 18 * KB));
  items.push(item(next(), "draft_v2.ass", "制作资料/字幕工程/分轨/EP01/中文/草稿", 84 * KB));
  items.push(item(next(), "draft_v0_初版.ass", "制作资料/字幕工程/分轨/EP01/中文/草稿/历史归档", 61 * KB));
  // 根级散件：大小未知 + 超长文件名
  items.push(item(next(), "沙丘S02.nfo", "", null));
  items.push(item(next(), "下载必读_解压密码与字幕挂载说明_请务必先阅读本文件再提问_v3_final_最终版.txt", "", 2 * KB));

  return { name: "沙丘·第二季 4K 蓝光原盘 REMUX 全 24 集", items };
}

/** 千项压测：40 卷 × 25 文件 = 1000 项（清单契约上限）。 */
function stressDataset() {
  const items = [];
  let id = 0;
  for (let v = 1; v <= 40; v++) {
    for (let f = 1; f <= 25; f++) {
      const n = (v * 31 + f * 7) % 100;
      const name =
        n < 55 ? `素材_${v}_${String(f).padStart(2, "0")}.mp4`
        : n < 75 ? `素材_${v}_${String(f).padStart(2, "0")}.jpg`
        : n < 90 ? `工程_${v}_${String(f).padStart(2, "0")}.zip`
        : `说明_${v}_${String(f).padStart(2, "0")}.txt`;
      items.push(item("st" + (++id), name, `批量素材包/卷${String(v).padStart(2, "0")}`, (2 + n * 3) * MB));
    }
  }
  return { name: "千项压测 · 批量素材归档", items };
}

globalThis.resolve = async (ctx) => {
  // 二段：清单条目 → 真实直链（惰性续期——每次 start/resume 都会重跑到这里）。
  if (ctx.resolverItem) {
    const target = flux.settings.target;
    if (!target) {
      flux.logger.warn("[manifest-playground] 二段直链未配置，放行原始链接（预期下载失败）");
      return null;
    }
    const sep = target.includes("?") ? "&" : "?";
    return { url: target + sep + "item=" + encodeURIComponent(ctx.resolverItem) };
  }

  // 初段：分享链接 → 多文件清单。
  const manifest = flux.settings.dataset === "stress" ? stressDataset() : normalDataset();
  flux.logger.info(`[manifest-playground] 返回清单「${manifest.name}」· ${manifest.items.length} 项`);
  return { manifest };
};
