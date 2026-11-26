import { defineMessages } from "../define";

/** 社区平台投票(`/vote/`)与 Logo 投票(`/logo-vote/`)文案。 */
export const polls = defineMessages("polls", {
  en: {
    platform: {
      meta: {
        title: "Community Platform Vote — FluxDown",
        description:
          "Vote on which community platform FluxDown should create next: WeChat group, QQ group or Official Account.",
      },
      eyebrow: "Community vote",
      title: "Choose our <em>community platform</em>",
      lede: "Help us decide which community platform to create next. Your vote shapes where FluxDown's community lives.",
      options: {
        wechat: {
          name: "WeChat Group",
          desc: "Convenient for daily chat, voice messages and real-time discussion.",
        },
        qq: {
          name: "QQ Group",
          desc: "File sharing, screen sharing and persistent chat history.",
        },
        officialAccount: {
          name: "Official Account",
          desc: "Curated updates, feature announcements, tutorials and articles from the team.",
        },
      },
      votes: (n: number) => (n === 1 ? "1 vote" : `${n} votes`),
      totalVotes: (n: number) => `${n} votes total`,
      submitVote: "Vote",
      submitting: "Voting…",
      yourVote: "Your vote",
      success: "Thanks for voting!",
      alreadyVoted: "You have already voted.",
      error: "Vote failed. Please try again.",
      rateLimited: "Too many requests. Please wait.",
      loading: "Loading results…",
      loadError: "Failed to load results.",
    },
    logo: {
      meta: {
        title: "Logo Vote — FluxDown",
        description:
          "Community vote on the FluxDown logo. Voting has ended — see the final ranking of every submitted design.",
      },
      eyebrow: "Voting ended",
      title: "Vote for the <em>FluxDown logo</em>",
      lede: "The logo vote has ended. Thanks to everyone who took part — the final results are below.",
      loading: "Loading logos…",
      loadError: "Failed to load logos. Please refresh.",
      votes: (n: number) => (n === 1 ? "1 vote" : `${n} votes`),
      rank: (n: number) => `#${String(n).padStart(2, "0")}`,
      builtin: "Official",
      community: "Community",
      anonymous: "Anonymous",
      noLogos: "No logos yet.",
      uploadedBy: (name: string) => `by ${name}`,
      alt: (n: number) => `Logo candidate ${n}`,
      total: (n: number) => `${n} designs`,
      totalVotes: (n: number) => `${n} votes cast`,
    },
  },
  zh: {
    platform: {
      meta: {
        title: "社区平台投票 — FluxDown",
        description: "投票决定 FluxDown 接下来创建哪个社区平台：微信群、QQ 群或微信公众号。",
      },
      eyebrow: "社区投票",
      title: "选择我们的<em>社区平台</em>",
      lede: "帮助我们决定下一个创建哪个社区平台。你的投票将决定 FluxDown 社区的去处。",
      options: {
        wechat: { name: "微信群", desc: "日常沟通方便，支持语音消息，实时交流讨论。" },
        qq: { name: "QQ 群", desc: "支持文件共享、屏幕分享，聊天记录持久保存。" },
        officialAccount: { name: "微信公众号", desc: "精选更新推送、功能预告、使用教程和团队文章。" },
      },
      votes: (n: number) => `${n} 票`,
      totalVotes: (n: number) => `共 ${n} 票`,
      submitVote: "投票",
      submitting: "投票中…",
      yourVote: "你的选择",
      success: "感谢投票！",
      alreadyVoted: "你已经投过票了。",
      error: "投票失败，请重试。",
      rateLimited: "请求过于频繁，请稍候。",
      loading: "正在加载结果…",
      loadError: "加载结果失败。",
    },
    logo: {
      meta: {
        title: "Logo 投票 — FluxDown",
        description: "FluxDown Logo 社区投票。投票已结束——查看所有参选设计的最终排名。",
      },
      eyebrow: "投票已结束",
      title: "为 FluxDown <em>Logo 投票</em>",
      lede: "Logo 投票已结束，感谢所有参与者！以下是最终投票结果。",
      loading: "正在加载 Logo…",
      loadError: "加载失败，请刷新重试。",
      votes: (n: number) => `${n} 票`,
      rank: (n: number) => `第 ${n} 名`,
      builtin: "官方",
      community: "社区",
      anonymous: "匿名",
      noLogos: "暂无 Logo。",
      uploadedBy: (name: string) => `by ${name}`,
      alt: (n: number) => `候选 Logo ${n}`,
      total: (n: number) => `${n} 个方案`,
      totalVotes: (n: number) => `共 ${n} 票`,
    },
  },
});
