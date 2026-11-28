import { defineMessages } from "../define";

/**
 * FAQ 页文案。问答按主题分组;`link` 为答案下方的延伸阅读(无语言前缀路径,渲染时本地化)。
 * FAQPage JSON-LD 直接由同一份 `q` / `a` 生成,保证结构化数据与可见文本一致。
 */
export const faq = defineMessages("faq", {
  en: {
    meta: {
      title: "FluxDown FAQ — Free IDM Alternative, Pricing, Platforms & Extension",
      description:
        "Frequently asked questions about FluxDown, the free open-source IDM alternative. Learn about features, pricing, supported platforms, the browser extension, and more.",
    },
    eyebrow: "FAQ",
    title: "Frequently asked <em>questions</em>",
    lede: "Everything you need to know about FluxDown.",
    topics: "Topics",
    count: (n: number) => `${n} questions`,
    more: "Still have questions?",
    contact: "Send us your feedback",
    readMore: "Read more",
    groups: [
      {
        id: "basics",
        title: "Basics",
        items: [
          {
            q: "Is FluxDown free to use?",
            a: "Yes. FluxDown is an open-source project — the app is free to use, with no ads and no hidden fees.",
            link: null,
          },
          {
            q: "How does FluxDown speed up downloads?",
            a: "FluxDown uses multi-threaded downloading with intelligent segmentation. It splits files into multiple parts and downloads them simultaneously, similar to how IDM works. The Rust-powered engine ensures maximum throughput with minimal resource usage.",
            link: null,
          },
          {
            q: "How is FluxDown different from IDM?",
            a: "FluxDown offers similar multi-threaded download acceleration but is completely free and built with modern technology: a Rust download engine and a native, GPU-rendered desktop UI built with GPUI. It supports HTTP, HTTPS, FTP, BitTorrent, and HLS/DASH streaming, and features smart segmentation based on your system specs.",
            link: null,
          },
        ],
      },
      {
        id: "platforms",
        title: "Platforms & setup",
        items: [
          {
            q: "Which platforms are supported?",
            a: "Windows (10 or later, x64/ARM64), macOS (Apple Silicon & Intel), and Linux (x64) are fully supported. Windows 7/8/8.1 are not supported. macOS builds are signed with an Apple Developer ID and notarized, so they open like any other app.",
            link: null,
          },
          {
            q: "Which browsers are supported?",
            a: "The browser extension supports Chrome, Edge, and other Chromium-based browsers. Firefox support is also available. The extension automatically intercepts downloads and sends them to FluxDown for accelerated downloading.",
            link: null,
          },
          {
            q: "How do I install the browser extension?",
            a: "The easiest way is to install directly from the Chrome Web Store, Edge Add-ons or Firefox Add-ons — just search 'FluxDown' and click install. If you can't open the store, the Download page also has the store-signed .crx: drag it onto chrome://extensions with Developer Mode on, and Chrome keeps it updated like a store install.",
            link: "/download",
          },
        ],
      },
      {
        id: "safety",
        title: "Safety & reliability",
        items: [
          {
            q: "Is FluxDown safe to use?",
            a: "Absolutely. FluxDown is built with Rust, which guarantees memory safety. The browser extension communicates with the app via Native Messaging Host (NMH) — a secure, browser-native protocol that uses local IPC (Named Pipe on Windows, Unix socket on macOS/Linux). No data is ever sent to external servers. All your download data stays on your machine.",
            link: "/privacy",
          },
          {
            q: "Does FluxDown support resume after interruption?",
            a: "Yes, FluxDown has full breakpoint resume support. All download progress is persisted to a local SQLite database. You can safely close the app or restart your computer without losing any progress.",
            link: null,
          },
        ],
      },
    ],
  },
  zh: {
    meta: {
      title: "FluxDown 常见问题 — 免费 IDM 替代品、价格、平台与浏览器扩展",
      description:
        "关于 FluxDown 的常见问题：免费开源的 IDM 替代品。了解功能、价格、支持的平台、浏览器扩展等。",
    },
    eyebrow: "常见问题",
    title: "常见<em>问题解答</em>",
    lede: "关于 FluxDown 你需要知道的一切。",
    topics: "主题",
    count: (n: number) => `${n} 个问题`,
    more: "还有其他问题？",
    contact: "给我们发送反馈",
    readMore: "延伸阅读",
    groups: [
      {
        id: "basics",
        title: "基础",
        items: [
          {
            q: "FluxDown 是免费的吗？",
            a: "是的。FluxDown 是开源项目，客户端免费使用，没有广告，也没有隐藏费用。",
            link: null,
          },
          {
            q: "FluxDown 如何加速下载？",
            a: "FluxDown 使用多线程下载和智能分段技术。它将文件拆分为多个部分并同时下载，原理类似 IDM。基于 Rust 的引擎确保了最大吞吐量和最低的资源占用。",
            link: null,
          },
          {
            q: "FluxDown 和 IDM 有什么区别？",
            a: "FluxDown 提供类似的多线程下载加速功能，但完全免费，且使用现代技术构建：Rust 下载引擎，加上基于 GPUI 的原生 GPU 渲染桌面界面。支持 HTTP、HTTPS、FTP、BitTorrent 及 HLS/DASH 流媒体协议，具备基于系统配置的智能分段功能。",
            link: null,
          },
        ],
      },
      {
        id: "platforms",
        title: "平台与安装",
        items: [
          {
            q: "支持哪些操作系统？",
            a: "目前完整支持 Windows（10 及以上，x64/ARM64）、macOS（Apple Silicon 与 Intel）和 Linux（x64）。不支持 Windows 7/8/8.1。macOS 版本已使用 Apple Developer ID 签名并通过公证，可以像其他应用一样直接打开。",
            link: null,
          },
          {
            q: "支持哪些浏览器？",
            a: "浏览器扩展支持 Chrome、Edge 及其他基于 Chromium 的浏览器，同时也支持 Firefox。扩展会自动拦截下载并发送到 FluxDown 进行加速下载。",
            link: null,
          },
          {
            q: "如何安装浏览器扩展？",
            a: "最简单的方式是直接从 Chrome 网上应用店、Edge 附加组件或 Firefox 附加组件商店安装——搜索「FluxDown」点击安装即可。打不开商店时，下载页也提供商店签名的 .crx：开启开发者模式后拖入 chrome://extensions 即可安装，之后会像商店版一样自动更新。",
            link: "/download",
          },
        ],
      },
      {
        id: "safety",
        title: "安全与可靠性",
        items: [
          {
            q: "FluxDown 安全吗？",
            a: "完全安全。FluxDown 使用 Rust 构建，保证内存安全。浏览器扩展通过 Native Messaging Host（NMH）与应用通信——这是一种浏览器原生的安全协议，使用本地 IPC 通道（Windows 上为命名管道，macOS/Linux 上为 Unix socket）。不会向外部服务器发送任何数据，所有下载数据都保留在你的设备上。",
            link: "/privacy",
          },
          {
            q: "FluxDown 支持断点续传吗？",
            a: "支持。FluxDown 具备完整的断点续传功能。所有下载进度都持久化到本地 SQLite 数据库中。你可以安全地关闭应用或重启电脑，不会丢失任何进度。",
            link: null,
          },
        ],
      },
    ],
  },
});
