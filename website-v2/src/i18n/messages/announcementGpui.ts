import { defineMessages } from "../define";

/** /announcements/gpui-client/: the author's note on the GPUI rewrite (v0.5.0). */
export const announcementGpui = defineMessages("announcementGpui", {
  en: {
    meta: {
      title: "Why I rebuilt FluxDown's interface from scratch — FluxDown",
      description:
        "FluxDown 0.5.0 ships a native GPUI desktop client. A note from the author on why the Flutter UI was retired, what the new three-process architecture changes, and what you get from it.",
    },
    eyebrow: "Announcement · v0.5.0",
    title: "I rebuilt FluxDown's interface <em>from scratch</em>",
    intro:
      "This isn't a reskin. For a long stretch I put almost everything into one goal: make FluxDown a truly native app — there the moment you open it, still working when you close it, and quick to respond in every frame. Here's why I did it, and what it means for you.",
    byline: "From ZerxLab, the author of FluxDown",
    date: "September 30, 2026",
    download: "Download 0.5.0",
    changelog: "Full changelog",
    contents: "Contents",
    statsLabel: "The rewrite, in numbers",
    stats: [
      { v: "3", k: "processes, each with one job" },
      { v: "11", k: "independent UI modules" },
      { v: "~68k", k: "lines of Rust UI code" },
      { v: "~30k", k: "lines in the rebuilt Web console" },
    ],
    sections: [
      {
        id: "why",
        title: "Why tear it down",
        body: [
          "Let me be honest first: the Flutter version of FluxDown worked, and it finished countless downloads for a lot of people. But every time I opened it, I knew it was still one step short of the downloader I had in mind.",
          "Its interface and download engine were squeezed into a single process, talking through an FFI signal bridge. If the interface went wrong, the downloads in flight went down with it.",
          "Multiple windows were another wall. Flutter on the desktop has no real multi-window: settings, the queue manager and task details all had to be crammed into the main window as dialogs. Just for one small quick-download window, I had to write a separate native host on each platform — C++ for Windows, Swift for macOS, C for Linux — embed a second, complete Flutter engine in it, and keep it in sync with the main window over message channels.",
          "Memory is what I felt worst about. Once that small window's engine was created, it could never be torn down — creating and destroying it repeatedly crashed — so it stayed in memory until the app quit. And to keep downloads running in the background, the entire Flutter interface had to stay resident too, even on days you never opened it once.",
          "Then there was the pace of work. Adding one button meant writing a Rust signal, generating Dart bindings, then writing the Dart model again — the same thing, twice, in two languages. Time went into shuttling data around instead of polishing the window you look at every day.",
          "So I stopped patching the old interface and started over: the entire desktop client, rewritten on a pure-Rust native UI.",
        ],
      },
      {
        id: "gpui",
        title: "Why GPUI",
        body: [
          "GPUI is the Rust UI framework behind the Zed editor; it draws the interface directly on the GPU. No WebView, no Electron, no second language — from the download engine to every pixel on screen, it's now one Rust codebase.",
          "That means fewer layers, a shorter path from click to result, and native windows on macOS, Windows and Linux. It isn't a web page stuffed into a shell. It's an app that belongs on your desktop.",
          "Multiple windows are first-class here. Settings, the queue manager, task details and progress windows are all real native windows sharing one process and one live state — open as many as you like, and closing one actually frees it.",
        ],
      },
      {
        id: "architecture",
        title: "The new architecture: downloads and UI, fully separated",
        body: [
          "The most important decision in this rewrite is one you can't see: FluxDown is now three processes, each with a single job.",
        ],
      },
      {
        id: "interface",
        title: "The new interface: every corner rethought",
        body: [
          "Beyond the architecture, I rebuilt the interface itself pixel by pixel. A few of the changes I love most:",
        ],
      },
      {
        id: "beyond",
        title: "Not just the desktop",
        body: [
          "With a new foundation in place, I brought the rest of the family up to the same starting line.",
        ],
      },
      {
        id: "upgrade",
        title: "Before you upgrade",
        body: [
          "Your tasks, history and settings live in the same local database, so they're all still there after the upgrade. On Windows the installer upgrades the old Flutter version in place, and file associations plus magnet / ed2k links are pointed at the new client automatically.",
          "A few things are worth knowing:",
        ],
      },
      {
        id: "thanks",
        title: "Finally, thank you",
        body: [
          "Rewriting software that already works is a risky and lonely thing to do. What carried me through was every issue, every vote, and every \u201cthis is great\u201d.",
          "FluxDown is still free, open source and ad-free. If the new interface makes you smile, tell me. If something feels off, tell me even more — you get to steer the next release.",
        ],
      },
    ],
    quote:
      "The downloader I want is one you barely notice: there when you open it, still going when you close it, never in your way.",
    layers: [
      {
        id: "fluxdown-desktop",
        role: "Interface",
        desc: "Only draws the picture. Starts on demand and quits when you close it — nothing left behind in the background.",
      },
      {
        id: "fluxdown-agent",
        role: "Gateway",
        desc: "Stays resident for the tray, browser capture, clipboard, cloud sync and the local API.",
      },
      {
        id: "fluxdownd",
        role: "Download core",
        desc: "Owns the engine and the database: tasks, queues, schedules, RSS, plugins and webhooks.",
      },
    ],
    archAfter:
      "The difference is immediate. Close the window and downloads keep going; open it again and it picks up the live state instantly. Close the last window and the interface process exits completely, handing all of its memory back to the system — what stays resident is only the agent and the download core, with no UI runtime at all. Whatever happens to the interface process, it can't touch the files being downloaded. And because the core and the interface speak one protocol, the Web console on your NAS finally shares the same capabilities as the desktop client.",
    features: [
      {
        k: "Command palette",
        v: "Press ⌘K / Ctrl+K and every action and setting is one search away. Fuzzy matching, and the things you use most float to the top.",
      },
      {
        k: "Real multi-window",
        v: "Settings, queue manager, task details and a per-task progress window each live on their own — with their position and size remembered.",
      },
      {
        k: "Files you can see",
        v: "The task list shows system file icons. Double-click to open, drag a finished file out of the window (macOS / Linux Wayland), and moved or deleted files are flagged right away.",
      },
      {
        k: "Speed you can read",
        v: "Task details gain \u201cSource composition\u201d and \u201cSpeed\u201d pages, showing how much came from CDN, proxy or each network card.",
      },
      {
        k: "A list that holds still",
        v: "Rows keep a stable order while progress updates, and the page stays smooth even with a very long task list.",
      },
      {
        k: "Themes, rebuilt",
        v: "A declarative token format with contrast that stays readable by design — craft your own palette in the theme builder on this site.",
      },
    ],
    beyondItems: [
      {
        k: "A new Web console",
        v: "The NAS / server Web console was rewritten from scratch, with layout and actions matching the desktop client one to one.",
      },
      {
        k: "Multi-device",
        v: "Send tasks to your cloud devices or to paired devices on your LAN; direct LAN pairing is confirmed with a short security code.",
      },
      {
        k: "Multi-NIC downloads",
        v: "Aggregate several network cards for one download; Auto proxy mode now schedules multiple paths in parallel.",
      },
      {
        k: "Old bugs, gone",
        v: "A long list of fixes: HTTP resume checks, silent HLS / DASH output, ED2K files over 4 GB, BT pause races, queue schedules and more.",
      },
      {
        k: "Quieter, faster",
        v: "Idle FluxDown no longer wakes sleeping NAS disks, and cold starts are faster with shorter waits for the window.",
      },
    ],
    upgradeItems: [
      "The LAN pairing protocol is now v2 — upgrade both devices together.",
      "Saved site credentials need to be entered once more.",
      "Update the browser extension along with the desktop app, otherwise it may show \u201cNot connected\u201d.",
      "Windows builds are unsigned for now, so the system may warn you on first launch.",
      "The Android app is still built on Flutter and keeps receiving engine updates.",
    ],
    ctaDownload: "Get the new version",
    ctaFeedback: "Send feedback",
    ctaSponsor: "Sponsor FluxDown",
    signature: "— author of FluxDown",
  },
  zh: {
    meta: {
      title: "为什么我把 FluxDown 的界面从头重写了一遍 — FluxDown",
      description:
        "FluxDown 0.5.0 正式发布原生 GPUI 桌面客户端。作者来信：为什么告别 Flutter 界面、三进程新架构改变了什么、你能从中得到什么。",
    },
    eyebrow: "公告 · v0.5.0",
    title: "我把 FluxDown 的界面，<em>从头重写了一遍</em>",
    intro:
      "这不是一次换皮。过去很长一段时间，我几乎把全部精力都投在了一件事上：让 FluxDown 成为真正的原生应用——点开即在，关掉不停，每一帧都跟手。下面是我为什么这么做，以及它会给你带来什么。",
    byline: "来自 FluxDown 作者 ZerxLab",
    date: "2026 年 9 月 30 日",
    download: "下载 0.5.0",
    changelog: "完整更新日志",
    contents: "目录",
    statsLabel: "这次重写，用数字说",
    stats: [
      { v: "3", k: "个进程，各司其职" },
      { v: "11", k: "个独立界面模块" },
      { v: "~6.8 万", k: "行 Rust 界面代码" },
      { v: "~3 万", k: "行重写的 Web 控制台" },
    ],
    sections: [
      {
        id: "why",
        title: "为什么要推倒重来",
        body: [
          "先说实话：Flutter 版的 FluxDown 能用，也陪很多人下完了数不清的文件。但每次打开它，我都知道，它离我心里那个下载器还差一口气。",
          "它的界面和下载引擎挤在同一个进程里，中间靠一层 FFI 信号桥来回传话。界面一旦出问题，正在跑的下载也跟着遭殃。",
          "多窗口是另一道坎。Flutter 桌面端没有真正的多窗口：设置、队列管理、任务详情，都只能塞进主窗口里当对话框。光是一个快速下载小窗，我就得在三个平台各写一套原生宿主——Windows 用 C++、macOS 用 Swift、Linux 用 C——再往里塞进第二个完整的 Flutter 引擎，靠消息通道和主窗口来回同步。",
          "内存，则是最让我过意不去的地方。那个小窗的引擎一旦创建就不敢销毁——反复创建销毁会崩溃——只能一直占着内存直到程序退出；而为了让下载在后台继续，整个 Flutter 界面也得跟着常驻内存，哪怕你一整天都没打开过它。",
          "还有开发节奏：加一个按钮，要先写 Rust 信号、再生成 Dart 绑定、再写一遍 Dart 模型——同一件事，用两种语言各写一遍。时间都花在了来回搬运上，而不是打磨你每天盯着看的那个窗口。",
          "所以我决定停下来，不再给旧界面打补丁，而是从零开始，用一套纯 Rust 的原生 UI 重写整个桌面客户端。",
        ],
      },
      {
        id: "gpui",
        title: "为什么是 GPUI",
        body: [
          "GPUI 是 Zed 编辑器背后的 Rust UI 框架，界面由 GPU 直接绘制。没有 WebView，没有 Electron，也没有第二种语言——从下载引擎到屏幕上的每一个像素，现在都是同一套 Rust 代码。",
          "这意味着更少的中间层、更短的链路，以及 macOS、Windows、Linux 上各自原生的窗口。它不是把网页塞进一个壳里，而是真正属于桌面的应用。",
          "多窗口在这里是一等公民：设置、队列管理、任务详情、进度窗口都是真正的原生窗口，共享同一个进程和同一份实时状态——想开几个开几个，关掉就真正释放。",
        ],
      },
      {
        id: "architecture",
        title: "新的架构：把下载和界面彻底分开",
        body: ["这次重构里最重要的决定，其实你看不见：FluxDown 被拆成了三个各司其职的进程。"],
      },
      {
        id: "interface",
        title: "新的界面：每一处都重新想过",
        body: ["架构之外，界面本身我也一个像素一个像素地重做了。挑几个我自己最喜欢的变化："],
      },
      {
        id: "beyond",
        title: "不只是桌面",
        body: ["既然底座换了，我顺手把整个家族都拉到了同一条起跑线上。"],
      },
      {
        id: "upgrade",
        title: "升级前你需要知道",
        body: [
          "你的任务、历史和设置沿用同一个本地数据库，升级后原样都在。Windows 安装包会直接覆盖升级旧的 Flutter 版，文件关联和 magnet / ed2k 链接也会自动改指向新客户端。",
          "有几件事值得留意：",
        ],
      },
      {
        id: "thanks",
        title: "最后，谢谢你",
        body: [
          "重写一个已经能用的软件，是一件很冒险、也很孤独的事。支撑我走完这段路的，是每一条 issue、每一次投票、每一句「好用」。",
          "FluxDown 依然免费、开源、没有广告。如果新界面让你眼前一亮，告诉我；如果哪里不顺手，更要告诉我——下一个版本往哪走，由你来决定。",
        ],
      },
    ],
    quote: "我想要的下载器，是你几乎感觉不到它的存在：点开就在，关掉也不停，从不拖你后腿。",
    layers: [
      {
        id: "fluxdown-desktop",
        role: "界面",
        desc: "只负责把画面画好。按需启动，关掉即退出，不在后台留下任何东西。",
      },
      {
        id: "fluxdown-agent",
        role: "网关",
        desc: "常驻后台，负责托盘、浏览器接管、剪贴板、云同步与本机 API。",
      },
      {
        id: "fluxdownd",
        role: "下载核心",
        desc: "独占下载引擎与数据库，掌管任务、队列、定时、RSS、插件与 Webhook。",
      },
    ],
    archAfter:
      "带来的变化很直接：关掉窗口，下载照旧；再次打开，界面立刻接上实时状态。关掉最后一个窗口，界面进程随之完全退出，占用的内存如数还给系统——后台常驻的只剩不含任何界面运行时的 agent 与下载核心。界面进程无论出什么状况，都碰不到正在下载的文件。而且下载核心与界面之间只说一种协议，NAS 上的 Web 控制台终于和桌面客户端共用同一套能力。",
    features: [
      {
        k: "命令面板",
        v: "⌘K / Ctrl+K 一键唤出，所有操作与设置一搜即达；支持拼音、首字母与混拼，越常用的越靠前。",
      },
      {
        k: "真正的多窗口",
        v: "设置、队列管理、任务详情、单任务进度窗口各自独立，位置和尺寸都会被记住。",
      },
      {
        k: "看得见的文件",
        v: "任务列表直接显示系统文件图标；双击打开，拖出窗口就是文件（macOS / Linux Wayland），文件被移走或删除会立刻标出来。",
      },
      {
        k: "看得懂的速度",
        v: "任务详情新增「来源构成」与「速度」页，CDN、代理、每张网卡各贡献了多少，一目了然。",
      },
      {
        k: "不乱跳的列表",
        v: "进度刷新时行序保持稳定；任务再多，页面依旧顺滑。",
      },
      {
        k: "重做的主题",
        v: "声明式 token 格式，对比度自动保证可读；还能在官网的主题编辑器里调出属于你的配色。",
      },
    ],
    beyondItems: [
      {
        k: "全新 Web 控制台",
        v: "NAS / 服务器端的 Web 控制台从头重写，布局与操作和桌面客户端一一对齐。",
      },
      {
        k: "多设备协同",
        v: "把任务下发到云设备或局域网内已配对的设备；局域网直连配对用一串安全码确认。",
      },
      {
        k: "多网卡聚合",
        v: "多张网卡合力下载同一个文件；自动代理改为多路径并行调度。",
      },
      {
        k: "告别老问题",
        v: "一长串修复：HTTP 续传校验、HLS / DASH 无声、ED2K 超过 4 GB、BT 暂停竞态、队列定时……",
      },
      {
        k: "更安静，更快",
        v: "空闲时不再唤醒 NAS 休眠的硬盘；冷启动更快，等窗口的时间更短。",
      },
    ],
    upgradeItems: [
      "局域网配对协议升级到 v2，两台设备需要一起升级。",
      "已保存的站点凭据需要重新输入一次。",
      "浏览器扩展请与桌面端一起更新，否则可能显示「未连接」。",
      "Windows 安装包暂未签名，首次运行时系统可能会提示风险。",
      "Android 版仍基于 Flutter，继续随下载引擎一起更新。",
    ],
    ctaDownload: "下载新版本",
    ctaFeedback: "反馈问题",
    ctaSponsor: "赞助 FluxDown",
    signature: "—— FluxDown 作者",
  },
});
