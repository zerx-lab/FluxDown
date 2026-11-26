import { defineMessages } from "../define";

/** 公告列表页(/announcements);公告正文本身在 `src/lib/announcements.ts`。 */
export const announcements = defineMessages("announcements", {
  en: {
    meta: {
      title: "Announcements — FluxDown",
      description: "News, community events and security notices from the FluxDown team.",
    },
    eyebrow: "Announcements",
    title: "News from the <em>team</em>",
    lede: "Community events, votes and security notices, newest first.",
    active: "Active",
    ended: "Ended",
    open: "Open",
    empty: "No announcements yet.",
    count: (n: number) => `${n} announcement${n === 1 ? "" : "s"}`,
  },
  zh: {
    meta: {
      title: "公告 — FluxDown",
      description: "FluxDown 团队的最新动态、社区活动与安全公告。",
    },
    eyebrow: "公告",
    title: "来自<em>团队</em>的消息",
    lede: "社区活动、投票与安全公告,按时间倒序排列。",
    active: "进行中",
    ended: "已结束",
    open: "查看",
    empty: "暂无公告。",
    count: (n: number) => `共 ${n} 条公告`,
  },
});
