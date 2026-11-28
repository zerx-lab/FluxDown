import { defineMessages } from "../define";

/** /pricing/why/: the paid-plans explainer. */
export const pricingWhy = defineMessages("pricingWhy", {
  en: {
    meta: {
      title: "Why FluxDown Is Introducing Paid Plans — FluxDown",
      description:
        "Why FluxDown is introducing paid plans, what happens to the original free promise, and exactly what changes (and what never will) for free users.",
    },
    eyebrow: "Pricing explained",
    title: "Why FluxDown is introducing <em>paid plans</em>",
    intro:
      "FluxDown started as — and remains — a free, open-source downloader. This page explains honestly why paid plans are coming, what happens to the original promise, and exactly what changes for free users.",
    back: "Back to pricing",
    vote: "Vote on the price",
    contents: "Contents",
    draftLabel: "Draft",
    draftNote:
      "Prices and tiers are a draft — vote and comment to shape the final plan. One thing is not a draft: the local downloader stays fully free, forever.",
    sections: [
      {
        id: "promise",
        title: "The original promise",
        body: [
          "When FluxDown launched we promised a free, open-source download manager. That promise stands: the app is AGPL-3.0 licensed, the full source code stays public, and the downloader you use today keeps working without paying anything.",
          "Introducing paid plans does not mean putting the downloader behind a paywall. It means giving the project a sustainable way to keep shipping.",
        ],
      },
      {
        id: "why-charge",
        title: "Why charge at all?",
        body: [
          "Some upcoming features cannot run on your machine alone: account sync, cross-device task dispatch and CDN-backed delivery all need servers that cost real money every month. Development time is also finite — sustainable funding keeps updates coming.",
          "Paid plans cover exactly those server-backed capabilities. Donations and sponsorships helped us get here, but they don't scale with server bills.",
        ],
      },
      {
        id: "free-users",
        title: "Will free users lose anything?",
        body: [
          "No. The principle is simple: everything that runs locally stays fully free — every protocol, unlimited threads, the browser extension, themes, plugins and the local API. There will never be artificial limits like capping free users to 8 threads — not now, not ever. Paid plans only add cloud-backed services on top.",
        ],
      },
      {
        id: "decide",
        title: "Help decide",
        body: [
          "Price points are still open. The poll and discussion feed directly into the final decision — every vote and comment is recorded publicly in a GitHub issue.",
        ],
      },
    ],
    afterTable:
      "In other words, free and premium download exactly the same way. The differences are only services that run on our servers — cloud sync, cross-device dispatch, priority support — plus more premium features we'll keep adding over time. And FluxDown stays open source: nothing about the license changes.",
    selfHostTitle: "One more promise: a self-hosted path, wherever we can",
    selfHostBody:
      "You'll never be forced onto our servers. Wherever a feature depends on the cloud, we'll do our best to keep a self-hosted alternative open: webhook notifications already work with your own endpoint on any plan, and multi-device connectivity can be built on your own tunnel or private network (e.g. frp or Tailscale) pointing at your own headless server. What paid plans buy is convenience — it works out of the box and we run the servers for you — never a lock-in.",
    cta: "Vote on the price",
    ctaSecondary: "See plans",
  },
  zh: {
    meta: {
      title: "为什么 FluxDown 要开始收费 — FluxDown",
      description: "FluxDown 为什么引入付费计划、最初的免费承诺怎么办，以及免费用户究竟会有哪些变化（以及哪些永远不会变）。",
    },
    eyebrow: "收费说明",
    title: "为什么 FluxDown <em>要开始收费</em>",
    intro:
      "FluxDown 从一开始就是——并且将继续是——一款免费开源的下载器。这篇文章坦诚地说明：为什么会引入付费计划、最初的承诺怎么办、免费用户的体验究竟会有什么变化。",
    back: "返回定价页",
    vote: "为价格投票",
    contents: "目录",
    draftLabel: "草案",
    draftNote: "价格与档位均为草案，欢迎投票和留言——最终方案将由这些反馈共同决定。但有一点不是草案：下载器的本地功能完整免费，永远如此。",
    sections: [
      {
        id: "promise",
        title: "最初的承诺",
        body: [
          "FluxDown 发布时承诺做一款免费开源的下载管理器。这个承诺不变：应用采用 AGPL-3.0 协议、完整源码持续公开，你今天在用的下载器不付一分钱也会一直可用。",
          "引入付费计划不是把下载器关进付费墙，而是给项目一条可持续走下去的路。",
        ],
      },
      {
        id: "why-charge",
        title: "为什么要收费？",
        body: [
          "一些即将到来的能力没法只跑在你的电脑上：账号同步、跨设备任务下发、CDN 分发，背后都是每个月实打实的服务器开销。开发时间同样是有限资源——可持续的收入才能支撑持续更新。",
          "付费计划覆盖的正是这些依赖服务器的能力。捐赠和赞助帮我们走到了今天，但它们无法随服务器账单一起增长。",
        ],
      },
      {
        id: "free-users",
        title: "免费用户会失去什么吗？",
        body: [
          "不会。原则很简单：跑在本地的能力全部永久免费——所有协议、不限线程、浏览器扩展、主题、插件、本机 API。永远不会出现「免费版限 8 线程」这类人为限制——现在不会，将来也不会。付费计划只是在此之上叠加云端服务。",
        ],
      },
      {
        id: "decide",
        title: "一起来决定",
        body: ["价位仍未定死。投票与讨论会直接影响最终决定——每一票、每条留言都公开记录在 GitHub issue 中。"],
      },
    ],
    afterTable:
      "换句话说，免费版和高级版的下载能力完全一致，差别只在跑在我们服务器上的服务——云同步、跨设备任务下发、优先支持，以及未来持续加入的更多高级功能。并且 FluxDown 依然开源：许可协议不会有任何变化。",
    selfHostTitle: "再多一个承诺：能自建的，都给你留自建的路",
    selfHostBody:
      "你永远不会被绑死在我们的服务器上。凡是依赖云端的能力，我们都会尽力保留自建替代方案：Webhook 通知现在就支持填你自己的回调地址，任何套餐都能用；多设备互联同样可以用自建内网穿透或组网（如 frp、Tailscale）连接你自己的服务端。付费买到的是省心——开箱即用、服务器有人替你运维——而不是锁定。",
    cta: "去为价格投票",
    ctaSecondary: "查看套餐",
  },
});
