import { defineMessages } from "../define";

/** /film/ 短片舞台的字幕与片头片尾文案。 */
export const film = defineMessages("film", {
  en: {
    title: "FluxDown — the film",
    tagline: "The download manager, rebuilt in Rust.",
    captions: [
      { k: "01", v: "Paste any link." },
      { k: "02", v: "It splits itself — 1 → 16 connections." },
      { k: "03", v: "Close the window. It keeps going." },
    ],
    endTitle: "Download anything.",
    endAccent: "Natively fast.",
    endMeta: "macOS · Windows · Linux — free & open source",
  },
  zh: {
    title: "FluxDown — 产品短片",
    tagline: "用 Rust 重写的下载管理器。",
    captions: [
      { k: "01", v: "粘贴任意链接。" },
      { k: "02", v: "自动拆分——1 → 16 条连接。" },
      { k: "03", v: "关掉窗口，下载照旧。" },
    ],
    endTitle: "下载一切，",
    endAccent: "原生极速。",
    endMeta: "macOS · Windows · Linux — 免费开源",
  },
});
