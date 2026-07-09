import type { Messages } from "./en";

const zhCN: Messages = {
  // Announcement
  "announcement.1": "投票进行中：建微信群、QQ群还是公众号？快来参与投票吧！",
  "announcement.2":
    "QQ 群已创建！群号：832143651，欢迎加入 FluxDown 社区交流群！",
  "announcement.3": "Telegram 群已创建！欢迎加入 FluxDown 国际社区交流群！",
  "announcement.4":
    "🎨 Logo 投票开启！为 FluxDown 新 Logo 投票，或提交你的原创设计，快来参与！",
  "announcement.5":
    "⚠️ 安全警告：发现仿冒网站 fluxdown.com.cn 传播「银狐」木马病毒，切勿下载！",
  "announcementModal.title": "安全警告",
  "announcementModal.body":
    "我们发现有非官方网站冒充 FluxDown，传播携带「银狐」远控木马的恶意安装包。请勿在该网站下载或运行任何文件，谨防资金与隐私损失。",
  "announcementModal.fakeLabel": "仿冒网站（请勿访问）",
  "announcementModal.fakeSite": "fluxdown.com.cn",
  "announcementModal.officialLabel": "官方网址（认准此域名）",
  "announcementModal.officialSite": "https://fluxdown.zerx.dev",
  "announcementModal.confirm": "我已知晓",
  "announcementModal.goOfficial": "前往官网",
  "announcement.close": "关闭",
  "announcement.badge": "公告",
  "announcement.title": "最新",
  "announcement.titleHighlight": "公告",
  "announcement.subtitle": "了解 FluxDown 团队的最新动态、活动与更新。",
  "announcement.active": "进行中",
  "announcement.ended": "已结束",
  "announcement.viewDetail": "查看详情",
  "announcement.empty": "暂无公告。",

  // Telegram Group
  "telegramGroup.badge": "Telegram 社区群",
  "telegramGroup.title": "加入",
  "telegramGroup.titleHighlight": "Telegram 群",
  "telegramGroup.subtitle":
    "点击下方按钮即可加入 FluxDown 国际社区，与全球开发者和用户一起交流讨论。",
  "telegramGroup.joinBtn": "加入 Telegram 群",
  "telegramGroup.howToJoin": "如何加入",
  "telegramGroup.step1": "点击上方按钮，打开 Telegram 邀请链接",
  "telegramGroup.step2": "在 Telegram 应用或网页端打开，无需搜索群组",
  "telegramGroup.step3": "点击「加入群组」即可成为社区成员",

  // Community Float
  "community.floatLabel": "加入社区",
  "community.telegram": "Telegram 群",
  "community.qq": "QQ 群",

  // QQ Group
  "qqGroup.badge": "QQ 社区群",
  "qqGroup.title": "加入",
  "qqGroup.titleHighlight": "社区群",
  "qqGroup.subtitle":
    "扫描二维码或搜索群号加入 FluxDown QQ 交流群，与开发者和其他用户一起讨论。",
  "qqGroup.groupNumber": "群号",
  "qqGroup.copy": "复制",
  "qqGroup.copied": "已复制",
  "qqGroup.howToJoin": "如何加入",
  "qqGroup.step1": "打开 QQ，扫描上方二维码直接加群",
  "qqGroup.step2": "或在 QQ 中搜索群号 832143651",
  "qqGroup.step3": "点击申请加入，等待审核通过即可",

  // Vote
  "vote.badge": "社区投票",
  "vote.title": "选择我们的",
  "vote.titleHighlight": "社区平台",
  "vote.subtitle":
    "帮助我们决定创建哪个社区平台。你的投票将塑造 FluxDown 社区的未来！",
  "vote.wechat": "微信群",
  "vote.wechatDesc": "日常沟通方便，支持语音消息，实时交流讨论。",
  "vote.qq": "QQ 群",
  "vote.qqDesc": "功能丰富，支持文件共享、屏幕分享，聊天记录持久保存。",
  "vote.officialAccount": "微信公众号",
  "vote.officialAccountDesc": "精选更新推送、功能预告、使用教程和团队文章。",
  "vote.votes": "{n} 票",
  "vote.totalVotes": "共 {n} 票",
  "vote.submitVote": "投票",
  "vote.submitting": "投票中...",
  "vote.success": "感谢投票！",
  "vote.alreadyVoted": "你已经投过票了",
  "vote.error": "投票失败，请重试",
  "vote.rateLimited": "请求过于频繁，请稍候",
  "vote.loading": "加载中...",
  "vote.loadError": "加载结果失败",

  // Logo Vote
  "logoVote.badge": "社区投票",
  "logoVote.endedBadge": "投票已结束",
  "logoVote.title": "为 FluxDown ",
  "logoVote.titleHighlight": "Logo 投票",
  "logoVote.subtitle":
    "为你最喜欢的 Logo 方案投票，或提交你的原创设计。FluxDown 的新面貌，由社区共同决定！",
  "logoVote.endedSubtitle":
    "Logo 投票已结束，感谢所有参与者！以下是最终投票结果。",
  "logoVote.loading": "正在加载 Logo...",
  "logoVote.loadError": "加载失败，请刷新重试",
  "logoVote.votes": "{n} 票",
  "logoVote.vote": "投票",
  "logoVote.unvote": "取消投票",
  "logoVote.voting": "投票中...",
  "logoVote.voteSuccess": "投票成功！",
  "logoVote.unvoteSuccess": "已取消投票",
  "logoVote.voteError": "投票失败，请重试",
  "logoVote.rateLimited": "操作太频繁，请稍候",
  "logoVote.rank": "第 {n} 名",
  "logoVote.topBadge": "前10名",
  "logoVote.uploadTitle": "提交你的 Logo",
  "logoVote.uploadDesc": "有好的设计创意？上传你的 Logo 设计，让社区来评选！",
  "logoVote.dropHint": "拖放图片到此处，或点击选择文件",
  "logoVote.dropHintActive": "松开以上传",
  "logoVote.fileLimit": "支持 PNG / JPG / SVG / WebP · 最大 2MB",
  "logoVote.nameLabel": "你的名字（可选）",
  "logoVote.namePlaceholder": "匿名",
  "logoVote.descLabel": "设计说明（可选）",
  "logoVote.descPlaceholder": "描述你的设计理念...",
  "logoVote.submit": "提交 Logo",
  "logoVote.submitting": "上传中...",
  "logoVote.submitSuccess": "提交成功！稍后将出现在列表中。",
  "logoVote.submitError": "上传失败，请重试",
  "logoVote.submitRateLimited": "提交过于频繁，请 24 小时后再试",
  "logoVote.fileTooLarge": "文件过大（最大 2MB）",
  "logoVote.fileInvalidType": "不支持的文件格式",
  "logoVote.builtin": "官方",
  "logoVote.community": "社区",
  "logoVote.anonymous": "匿名",
  "logoVote.noLogos": "暂无 Logo，快来提交第一个吧！",
  "logoVote.uploadedBy": "by {name}",

  // Nav
  "nav.logoVote": "Logo 投票",
  "nav.features": "功能",
  "nav.download": "下载",
  "nav.demo": "在线演示",
  "nav.resources": "资源",
  "nav.extension": "浏览器扩展",
  "nav.themeBuilder": "主题构建器",
  "nav.themeMarket": "主题市场",
  "nav.changelog": "更新日志",
  "nav.faq": "常见问题",
  "nav.apiDocs": "API 文档",
  "nav.docs": "文档",
  "nav.community": "社区",
  "nav.announcements": "公告",
  "nav.feedback": "反馈",
  "nav.sponsor": "赞助",
  "nav.github": "GitHub 仓库",

  // Theme Market
  "themes.badge": "主题市场",
  "themes.title": "FluxDown 社区主题",
  "themes.subtitle":
    "浏览社区制作的主题。下载主题 JSON 后，在桌面客户端「设置 → 外观」中导入即可使用。",
  "themes.searchPlaceholder": "搜索主题、作者、标签...",
  "themes.submitCta": "提交你的主题",
  "themes.clickToZoom": "点击放大",
  "themes.variant.dark": "暗色",
  "themes.variant.light": "亮色",
  "themes.download": "下载主题",
  "themes.download.dark": "暗色 JSON",
  "themes.download.light": "亮色 JSON",
  "themes.loadError": "主题索引加载失败，请稍后重试或直接访问仓库。",
  "themes.empty": "没有匹配的主题。",
  "themes.howTo.title": "如何使用主题",
  "themes.howTo.step1": "下载你喜欢的变体（暗色 / 亮色）对应的主题 JSON 文件。",
  "themes.howTo.step2": "打开 FluxDown → 设置 → 外观 → 导入主题，选择下载的文件。",
  "themes.howTo.step3": "想分享自己的主题？请先查看贡献指南了解规范与流程，再提交你的主题：",
  "themes.howTo.guideLink": "贡献指南 →",

  // Theme Builder
  "tb.badge": "主题构建器",
  "tb.title": "在浏览器里构建 FluxDown 主题",
  "tb.rightClickHint": "右键任意 UI 区块查看 token 值",
  "tb.rightClickDesc":
    "预览区每个视觉块都绑定 token path，右键即可查看并复制路径或当前值。",
  "tb.meta.name": "主题名称",
  "tb.meta.author": "作者",
  "tb.appearance.dark": "暗色",
  "tb.appearance.light": "亮色",
  "tb.actions.import": "导入",
  "tb.actions.export": "导出",
  "tb.actions.copyJson": "复制 JSON",
  "tb.actions.reset": "重置",
  "tb.searchPlaceholder": "搜索 token 路径或名称",
  "tb.groups.surface": "表面层",
  "tb.groups.element": "交互态",
  "tb.groups.text": "文字",
  "tb.groups.border": "边框",
  "tb.groups.accent": "强调色",
  "tb.groups.input": "输入框",
  "tb.groups.dialog": "对话框",
  "tb.groups.switch": "开关",
  "tb.groups.status": "语义状态",
  "tb.groups.shadow": "阴影",
  "tb.groups.segment": "分段调色板",
  "tb.groups.radius": "圆角",
  "tb.groups.spacing": "间距",
  "tb.groups.stroke": "描边",
  "tb.groups.button": "按钮",
  "tb.groups.alpha": "透明度",
  "tb.groups.mobile": "移动端几何",
  "tb.importSuccess": "已导入主题：{name}",
  "tb.importError": "导入失败：{reason}",
  "tb.exportSuccess": "已导出主题：{name}",
  "tb.copySuccess": "复制成功",
  "tb.copyError": "复制失败",
  "tb.resetSuccess": "已重置为默认暗色主题",
  "tb.context.title": "Token 检视器",
  "tb.context.copyPath": "复制路径",
  "tb.context.copyValue": "复制值",
  "tb.compat.title": "桌面端兼容格式",
  "tb.compat.desc":
    "JSON 结构与 FluxThemeTokens.toJson()/fromJson() 一致（appearance + colors + segmentPalette）。",
  "tb.mobileTitle": "请在电脑端打开",
  "tb.mobileHint": "主题设计器需要较宽的屏幕。请在桌面/PC 浏览器中打开本页面，以免布局错乱。",
  "tb.area.downloads": "主窗口",
  "tb.area.settings": "设置页",
  "tb.areaHint": "该 token 在预览中出现的位置",
  "tb.viewDownloads": "主窗口",
  "tb.viewSettings": "设置",

  // Hero
  "hero.badge": "开源免费 · 由 Rust 驱动",
  "hero.title1": "下载，",
  "hero.title2": "全面加速。",
  "hero.subtitle":
    "Rust 驱动的多协议下载管理器，支持 HTTP/FTP/BitTorrent 磁力链接及 HLS/DASH 流媒体，智能多线程加速与浏览器无缝集成。精美界面，极致性能，永久免费，零广告。",
  "hero.cta": "下载",
  "hero.github": "GitHub 开源",
  "hero.stat1.value": "10x",
  "hero.stat1.label": "极速下载",
  "hero.stat2.value": "零",
  "hero.stat2.label": "广告与追踪",
  "hero.stat3.value": "100%",
  "hero.stat3.label": "永久免费",

  // Hero mockup
  "mockup.category": "分类",
  "mockup.allFiles": "全部文件",
  "mockup.video": "视频",
  "mockup.audio": "音频",
  "mockup.document": "文档",
  "mockup.image": "图片",
  "mockup.archive": "压缩包",
  "mockup.other": "其他",
  "mockup.tabAll": "全部",
  "mockup.tabDownloading": "下载中",
  "mockup.tabCompleted": "已完成",
  "mockup.tabPaused": "已暂停",
  "mockup.tabError": "出错",
  "mockup.colFilename": "文件名",
  "mockup.colProgress": "进度",
  "mockup.colSpeed": "速度",
  "mockup.colStatus": "状态",
  "mockup.download": "下载",
  "mockup.downloading": "下载中",
  "mockup.statusActive": "{n} 活跃 · {p} 暂停 · {t} 总计",
  "mockup.noTasks": "暂无任务",
  "mockup.detail": "详情",
  "mockup.distLabel": "下载分布",
  "mockup.labelSize": "大小",
  "mockup.labelDownloaded": "已下载",
  "mockup.labelSpeed": "速度",
  "mockup.labelRemaining": "剩余",
  "mockup.labelStatus": "状态",
  "mockup.labelThreads": "线程",
  "mockup.labelPath": "路径",
  "mockup.labelUrl": "地址",
  "mockup.labelError": "错误",
  "mockup.threadsValue": "{n} 线程（动态）",
  "mockup.btnPause": "暂停",
  "mockup.btnResume": "继续",
  "mockup.btnDelete": "删除",
  "mockup.statusPaused": "已暂停",
  "mockup.statusCompleted": "已完成",
  "mockup.statusDownloading": "下载中",
  "mockup.statusError": "出错",
  "mockup.subtitlePaused": "已暂停",
  "mockup.subtitleTimeout": "连接超时",
  "mockup.eta": "{n} 秒",
  "mockup.errorTimeout": "连接超时 (ETIMEDOUT)",
  "mockup.settingsGeneral": "通用",
  "mockup.settingsAppearance": "外观",
  "mockup.settingsDownload": "下载",
  "mockup.settingsLanguage": "语言",
  "mockup.settingsBack": "返回",
  "mockup.settingsThemeSelect": "主题",
  "mockup.settingsThemeSelectHint": "选择预设或导入自定义",
  "mockup.settingsThemeName": "主题 {n}",
  "mockup.settingsColorScheme": "主题色",
  "mockup.settingsDownloadHint": "默认保存位置与操作",
  "mockup.settingsSelectionSample": "选中文本示例",
  "mockup.settingsBtnSmall": "小",
  "mockup.settingsBtnMedium": "保存",
  "mockup.settingsBtnLarge": "应用",
  "mockup.settingsMisc": "行为",
  "mockup.settingsSwitchOn": "关闭到托盘",
  "mockup.settingsSwitchOff": "开机启动（已禁用）",
  "mockup.settingsGlassPanel": "玻璃面板",
  "mockup.settingsGlassSubtle": "轻玻璃",
  "mockup.settingsScrim": "对话框遮罩",
  "mockup.settingsStates": "状态与对话框",
  "mockup.settingsStateHover": "悬停",
  "mockup.settingsStateBadge": "徽标",
  "mockup.settingsStateDisabled": "禁用",
  "mockup.settingsStateFocused": "聚焦输入框",
  "mockup.settingsStateDialog": "对话框背景示例",

  // Features
  "features.badge": "核心功能",
  "features.title": "极速下载，",
  "features.titleHighlight": "一应俱全",
  "features.subtitle":
    "基于现代技术构建的强大下载管理器，为你带来卓越的性能与可靠性。",
  "features.rustTitle": "Rust 高性能引擎",
  "features.rustDesc":
    "基于 Rust 和 Tokio 构建，实现最大吞吐量。零开销抽象在保证内存安全的同时，提供原生级别的并发下载性能。",
  "features.segTitle": "智能分段",
  "features.segDesc":
    "IDM 风格的智能分段与运行时动态拆分。空闲线程自动接管慢速分段，连接复用避免重建开销，榨取每一份带宽。",
  "features.protoTitle": "多协议支持",
  "features.protoDesc":
    "全面支持 HTTP、HTTPS、FTP、BitTorrent 磁力链接及 HLS/DASH 流媒体。每种协议都有专属优化引擎——从 HTTP Range 请求到 DHT 节点发现，开箱即用。",
  "features.speedTitle": "速度控制",
  "features.speedDesc":
    "基于令牌桶算法的全局限速器。设置带宽限制，让下载在后台运行的同时保持流畅的浏览体验。",
  "features.resumeTitle": "断点续传",
  "features.resumeDesc":
    "完整的断点续传支持。所有下载状态持久化到 SQLite — 安全关闭和重启，不丢失任何一个字节。",
  "features.browserTitle": "浏览器集成",
  "features.browserDesc":
    "Chrome/Firefox 扩展三层拦截引擎，自动识别流媒体资源（HLS/DASH）。页面资源嗅探、智能文件类型过滤、域名规则，一键加速。",
  "features.uiTitle": "精美界面",
  "features.uiDesc":
    "精心打磨的视觉体验——深色与浅色主题、13 套配色方案、可调节面板的响应式布局。一款真正赏心悦目的下载工具。",
  "features.cleanTitle": "纯净安心",
  "features.cleanDesc":
    "零广告、零追踪、无需注册账号。所有数据留在本地——基于 SQLite 的本地优先架构。你的下载，只属于你。",

  // Extension
  "ext.badge": "浏览器扩展",
  "ext.title": "无缝接管",
  "ext.titleHighlight": "下载任务",
  "ext.subtitle":
    "安装 Chrome 或 Firefox 扩展，自动拦截浏览器下载并发送到 FluxDown。支持任意网站，可按需配置。",
  "ext.feat1.title": "一键拦截",
  "ext.feat1.desc": "自动捕获下载请求，或通过右键菜单手动发送，完全掌控",
  "ext.feat2.title": "本地通信",
  "ext.feat2.desc":
    "安全的 localhost:19527 HTTP 端点 — 无云端、无追踪，数据全部本地化",
  "ext.feat3.title": "智能过滤",
  "ext.feat3.desc": "按文件扩展名、域名黑白名单和最小文件大小进行过滤",
  "ext.addToChrome": "添加到 Chrome",
  "ext.addToEdge": "添加到 Edge",
  "ext.connected": "已连接",
  "ext.paused": "已暂停",
  "ext.today": "今日",
  "ext.thisWeek": "本周",
  "ext.total": "总计",
  "ext.autoIntercept": "自动拦截",
  "ext.recentCatches": "最近拦截",
  "ext.fileTypeFilters": "文件类型过滤",
  "ext.minFileSize": "最小文件大小",

  // Download
  "dl.badge": "下载",
  "dl.title": "准备好",
  "dl.titleHighlight": "加速了吗",
  "dl.subtitle": "下载适合你平台的 FluxDown。永久免费。",
  "dl.windows": "Windows",
  "dl.macos": "macOS",
  "dl.linux": "Linux",
  "dl.web": "Web 版",
  "dl.webArch": "自托管 · 浏览器访问",
  "dl.webGuide": "查看部署指南",
  "dl.cli": "命令行",
  "dl.cliArch": "命令行 · aria2 风格",
  "dl.cliGuide": "查看 CLI 文档",
  "dl.mobile": "移动端",
  "dl.docker": "Docker",
  "dl.dockerArch": "自托管 Web 版",
  "dl.dockerDeploy": "Docker 一键部署",
  "dl.dockerHint":
    "启动后访问 http://localhost:17800，管理 token 在首次启动日志中打印（仅显示一次）。",
  "dl.dockerCopy": "复制",
  "dl.dockerCopied": "已复制",
  "dl.scoopTitle": "使用 Scoop 安装",
  "dl.scoopDesc": "Windows 包管理器 —— 一条命令安装并保持更新。",
  "dl.scoopSelfHostedHint":
    "直接从 FluxDown 仓库源安装，始终跟随最新发布版，随时用 scoop update fluxdown 更新。",
  "dl.availableNow": "立即可用",
  "dl.comingSoon": "即将推出",
  "dl.downloadBtn": "下载",
  "dl.version": "v{version}",
  "dl.loading": "加载中...",
  "dl.installPkg": "安装包",
  "dl.portablePkg": "便携版",
  "dl.extensionTitle": "浏览器扩展",
  "dl.extensionDesc":
    "拦截浏览器下载并发送到 FluxDown，支持 Chrome、Edge 和 Firefox，均已在官方商店上架。",
  "dl.extensionFirefox": "Firefox 官方商店",
  "dl.extensionChromeStore": "Chrome 网上应用店",
  "dl.extensionEdgeStore": "Edge 附加组件",
  "dl.extensionOffline": "Chrome 离线包",
  "dl.extensionOtherNote": "适用于 Chrome、Edge、Brave 等 Chromium 内核浏览器",
  "dl.downloadExtension": "下载扩展",
  "dl.totalDownloads": "次下载",
  "dl.dmg": "DMG 安装镜像",
  "dl.appimage": "AppImage",
  "dl.linuxPortable": "便携版",
  "dl.moreFormats": "更多格式",
  "dl.notifyMe": "上线通知",
  "dl.emailPlaceholder": "你的邮箱地址",
  "dl.subscribing": "订阅中...",
  "dl.subscribed": "我们会通知你！",
  "dl.alreadySubscribed": "已经订阅过了",
  "dl.subscribeError": "失败，请重试",
  "dl.macosWarning": "遇到「已损坏」提示？",
  "dl.macosWarningLink": "查看解决方案",
  "dl.sysReq.windows": "需要 Windows 10 或更高版本",

  // macOS Gatekeeper page
  "macos.title": "macOS「已损坏」错误解决方案",
  "macos.subtitle":
    "由于应用未经 Apple 官方签名，macOS 可能会阻止运行。以下方法可帮助你顺利打开 FluxDown。",
  "macos.lastUpdated": "最后更新：2025年",
  "macos.whyTitle": "为什么会出现这个错误？",
  "macos.whyDesc":
    "macOS 的 Gatekeeper 安全机制默认只允许运行来自 App Store 或已认证开发者的应用。FluxDown 是免费软件，暂未购买 Apple 开发者证书进行签名，因此系统会显示「已损坏」或「来自身份不明的开发者」的提示。这不代表软件本身有问题。",
  "macos.method1Title": "方法一：开启「任何来源」（推荐）",
  "macos.method1Step1":
    "打开「系统设置」→「隐私与安全性」→「通用」，检查是否已有「任何来源」选项。",
  "macos.method1Step2":
    "如果没有该选项，打开「终端」，输入以下命令后按回车，并输入密码确认：",
  "macos.method1Step3":
    "执行完毕后，返回「系统设置」→「隐私与安全性」即可看到「任何来源」已启用。",
  "macos.method2Title": "方法二：绕过 Gatekeeper 隔离（仍无法打开时）",
  "macos.method2Desc":
    "开启「任何来源」后仍无法打开时，使用以下命令移除应用的隔离标记：",
  "macos.method2Note":
    "将 FluxDown.app 替换为你实际的应用名称，或将应用从「访达」直接拖入终端命令末尾。注意 quarantine 后面有一个空格。",
  "macos.method3Title": "方法三：本地签名（仍无法打开时）",
  "macos.method3Step1": "先安装 Command Line Tools：",
  "macos.method3Step2":
    "弹出安装窗口后点击「继续安装」，等待完成。然后执行以下命令对应用进行本地签名：",
  "macos.method3Note":
    "将「应用路径」替换为应用实际路径，可从「访达」→「应用程序」将应用拖入命令末尾。",
  "macos.tipTitle": "💡 小提示",
  "macos.tipDesc":
    "以上方法中，方法一通常可以解决 85% 以上的问题，方法二可覆盖到约 90%。如果三种方法都无效，欢迎到我们的社区反馈。",
  "macos.backToDownload": "返回下载页面",

  // Feedback
  "fb.badge": "反馈建议",
  "fb.title": "帮助我们做得",
  "fb.titleHighlight": "更好",
  "fb.subtitle":
    "有功能想法或发现了 Bug？我们很乐意听取你的意见。你的反馈将帮助塑造 FluxDown 的未来。",
  "fb.typeLabel": "反馈类型",
  "fb.type.feature": "功能建议",
  "fb.type.bug": "Bug 报告",
  "fb.type.other": "其他",
  "fb.titleLabel": "标题",
  "fb.titlePlaceholder": "简要描述你的反馈",
  "fb.descLabel": "详细描述",
  "fb.descPlaceholder": "详细描述你的想法或遇到的问题...",
  "fb.contactLabel": "联系方式",
  "fb.contactPlaceholder": "邮箱或其他联系方式",
  "fb.contactHint": "填写邮箱可收到反馈进度通知，其他联系方式可能无法收到通知",
  "fb.optional": "可选",
  "fb.submit": "提交反馈",
  "fb.submitting": "提交中...",
  "fb.success": "感谢你的反馈！",
  "fb.submitError": "提交失败，请稍后重试。",
  "fb.rateLimited": "提交太频繁，请稍等片刻。",

  // Feedback Page (tabs)
  "fbPage.tabList": "反馈看板",
  "fbPage.tabSubmit": "提交反馈",

  // Feedback List
  "fbList.badge": "反馈看板",
  "fbList.title": "社区",
  "fbList.titleHighlight": "反馈",
  "fbList.subtitle": "追踪功能建议、Bug 报告和社区建议的处理进度。",
  "fbList.state.all": "全部",
  "fbList.state.open": "待处理",
  "fbList.state.closed": "已完成",
  "fbList.labelFilter.all": "全部类型",
  "fbList.labelFilter.enhancement": "功能",
  "fbList.labelFilter.bug": "Bug",
  "fbList.labelFilter.feedback": "其他",
  "fbList.label.enhancement": "功能建议",
  "fbList.label.bug": "Bug",
  "fbList.label.feedback": "反馈",
  "fbList.showing": "共 {count} 条",
  "fbList.loading": "加载中...",
  "fbList.error": "加载反馈列表失败，请稍后重试。",
  "fbList.empty": "暂无反馈。",
  "fbList.prev": "上一页",
  "fbList.next": "下一页",
  "fbList.searchPlaceholder": "搜索标题、内容或 #编号...",
  "fbList.searchEmpty": "未找到匹配「{query}」的 Issue。",
  "fbList.searchClear": "清除搜索",

  // Issue Detail
  "issueDetail.open": "待处理",
  "issueDetail.completed": "已完成",
  "issueDetail.notPlanned": "不采纳",
  "issueDetail.duplicate": "重复项",
  "issueDetail.replies": "条回复",
  "issueDetail.commentsTitle": "{count} 条回复",
  "issueDetail.noComments": "暂无回复。",
  "issueDetail.notFound": "未找到该反馈。",
  "issueDetail.error": "加载反馈详情失败。",
  "issueDetail.viewOnGithub": "在 GitHub 上查看",
  "issueDetail.anonymous": "用户",
  "issueDetail.developer": "开发者",
  "issueDetail.meta.typeLabel": "类型：",
  "issueDetail.meta.type.feature": "功能建议",
  "issueDetail.meta.type.bug": "Bug 报告",
  "issueDetail.meta.type.other": "反馈",

  "issueDetail.replyPlaceholder": "写下你的回复...",
  "issueDetail.replySend": "发送",
  "issueDetail.replySending": "发送中...",
  "issueDetail.replySuccess": "回复已发送",
  "issueDetail.replyError": "发送回复失败，请重试。",
  "issueDetail.replyRateLimited": "回复过于频繁，请稍后再试。",
  "issueDetail.replyCharCount": "{count}/2000",

  // 404
  "notFound.title": "页面未找到",
  "notFound.desc": "你访问的页面不存在或已被移动。",
  "notFound.home": "返回首页",
  "notFound.feedback": "发送反馈",

  // FAQ
  "faq.badge": "常见问题",
  "faq.title": "常见",
  "faq.titleHighlight": "问题解答",
  "faq.subtitle": "关于 FluxDown 你需要知道的一切。",
  "faq.moreQuestions": "还有其他问题？",
  "faq.contactUs": "给我们发送反馈",
  "faq.items.0.q": "FluxDown 是免费的吗？",
  "faq.items.0.a":
    "是的，FluxDown 完全免费，没有广告、没有订阅、没有隐藏费用。所有功能对每位用户开放。",
  "faq.items.1.q": "FluxDown 如何加速下载？",
  "faq.items.1.a":
    "FluxDown 使用多线程下载和智能分段技术。它将文件拆分为多个部分并同时下载，原理类似 IDM。基于 Rust 的引擎确保了最大吞吐量和最低的资源占用。",
  "faq.items.2.q": "FluxDown 安全吗？",
  "faq.items.2.a":
    "完全安全。FluxDown 使用 Rust 构建，保证内存安全。浏览器扩展通过 Native Messaging Host（NMH）与应用通信——这是一种浏览器原生的安全协议，使用本地 IPC 通道（Windows 上为命名管道，macOS/Linux 上为 Unix socket）。不会向外部服务器发送任何数据，所有下载数据都保留在你的设备上。",
  "faq.items.3.q": "支持哪些浏览器？",
  "faq.items.3.a":
    "浏览器扩展支持 Chrome、Edge 及其他基于 Chromium 的浏览器，同时也支持 Firefox。扩展会自动拦截下载并发送到 FluxDown 进行加速下载。",
  "faq.items.4.q": "FluxDown 和 IDM 有什么区别？",
  "faq.items.4.a":
    "FluxDown 提供类似的多线程下载加速功能，但完全免费，且使用现代技术（Rust + Flutter）构建。支持 HTTP、HTTPS、FTP、BitTorrent 及 HLS/DASH 流媒体协议，具备基于系统配置的智能分段功能，提供原生桌面体验。",
  "faq.items.5.q": "FluxDown 支持断点续传吗？",
  "faq.items.5.a":
    "支持。FluxDown 具备完整的断点续传功能。所有下载进度都持久化到本地 SQLite 数据库中。你可以安全地关闭应用或重启电脑，不会丢失任何进度。",
  "faq.items.6.q": "支持哪些操作系统？",
  "faq.items.6.a":
    "目前完整支持 Windows（10 及以上，x64/ARM64）、macOS（Apple Silicon 与 Intel）和 Linux（x64）。不支持 Windows 7/8/8.1。macOS 用户可能会遇到「应用已损坏」的提示，请参阅我们的 macOS Gatekeeper 指南快速解决。",
  "faq.items.7.q": "如何安装浏览器扩展？",
  "faq.items.7.a":
    "最简单的方式是直接从 Chrome 应用商店或 Firefox 附加组件商店安装——搜索「FluxDown」点击安装即可。你也可以从下载页面下载离线扩展 zip 文件，解压后打开 chrome://extensions，开启开发者模式，点击「加载已解压的扩展程序」选择解压后的文件夹。",
  "faq.items.8.q":
    "macOS 提示 FluxDown「已损坏」或来自「身份不明的开发者」怎么办？",
  "faq.items.8.a":
    "这是因为 FluxDown 暂未使用 Apple 开发者证书签名，并不意味着应用真的已损坏。大多数情况下只需在终端执行一条命令即可解决。请访问我们的 macOS Gatekeeper 指南（/macos-gatekeeper）查看详细步骤。",

  // Changelog
  "changelog.badge": "更新日志",
  "changelog.title": "最新",
  "changelog.titleHighlight": "变更",
  "changelog.subtitle": "追踪 FluxDown 的每一次更新与改进。",
  "changelog.error": "加载更新日志失败，请稍后重试。",
  "changelog.empty": "暂无发布记录。",
  "changelog.loadMore": "加载更多",
  "changelog.loading": "加载中...",
  "changelog.copyMd": "MD",
  "changelog.copyPlain": "文本",
  "changelog.copied": "已复制",
  "changelog.downloadAssets": "下载此版本",
  "changelog.assetsNote":
    "通过服务端代理转发下载，文件直接来源于 GitHub Release，安全可信。",

  // Privacy Policy
  "privacy.title": "隐私政策",
  "privacy.lastUpdated": "最后更新：2026 年 2 月",
  "privacy.intro":
    "FluxDown（以下简称「我们」或「本软件」）致力于保护您的隐私。本隐私政策说明了当您使用 FluxDown 桌面应用、浏览器扩展和本网站时，我们收集哪些信息、不收集哪些信息以及如何处理您的数据。",

  "privacy.s1.title": "1. 我们收集的信息",
  "privacy.s1.desc": "我们仅收集提供和改进服务所必需的最少信息：",
  "privacy.s1.item1":
    "网站分析：我们使用 Vercel Web Analytics，这是一项注重隐私的分析服务，仅收集匿名的、聚合的页面浏览数据。不使用 Cookie，不收集个人数据，不追踪个人用户。",
  "privacy.s1.item2":
    "反馈提交：当您通过网站自愿提交反馈时，我们收集反馈类型、标题、描述以及可选的联系方式。这些数据以 GitHub Issue 的形式存储在我们的代码仓库中。",
  "privacy.s1.item3":
    "邮件订阅：如果您订阅了平台可用性通知，我们会将您的邮箱地址存储在 GitHub Issue 中，仅用于在所请求的平台可用时通知您。",

  "privacy.s2.title": "2. 我们不收集的信息",
  "privacy.s2.desc": "FluxDown 采用本地优先架构设计。桌面应用程序：",
  "privacy.s2.item1":
    "不会收集、传输或在任何远程服务器上存储您的下载链接、文件名或下载历史。所有下载数据均存储在您设备上的本地 SQLite 数据库中。",
  "privacy.s2.item2": "桌面应用中不包含任何遥测、崩溃报告或使用分析功能。",
  "privacy.s2.item3": "不需要任何形式的账户、登录或注册。",
  "privacy.s2.item4": "不与任何外部服务器通信。安装后应用完全离线运行。",

  "privacy.s3.title": "3. 浏览器扩展",
  "privacy.s3.desc":
    "FluxDown 浏览器扩展仅通过本地 HTTP 端点 (localhost:19527) 与 FluxDown 桌面应用通信。具体而言：",
  "privacy.s3.item1":
    "所有数据传输都在您的设备本地进行，发生在浏览器扩展和桌面应用之间。不会向任何外部服务器发送数据。",
  "privacy.s3.item2":
    "扩展将您的偏好设置（自动拦截开关、文件类型过滤器、域名规则）存储在浏览器的本地存储中 (chrome.storage.sync/local)。这些数据仅通过浏览器自带的同步机制进行同步（如已启用）。",
  "privacy.s3.item3": "扩展不会访问、读取或修改任何网页内容，仅拦截下载请求。",

  "privacy.s4.title": "4. 网站分析",
  "privacy.s4.desc":
    "本网站使用 Vercel Web Analytics，这是一项注重隐私的分析服务。它不使用 Cookie、不收集个人信息、不跨会话追踪用户，符合 GDPR、CCPA 等隐私法规，无需显示 Cookie 同意横幅。",

  "privacy.s5.title": "5. 数据存储与安全",
  "privacy.s5.desc":
    "所有下载数据（任务记录、文件路径、进度）均存储在您设备上由 FluxDown 应用管理的本地 SQLite 数据库中。我们无法访问这些数据。网站反馈和订阅数据存储在我们 GitHub 仓库的 Issue 中，受 GitHub 隐私政策约束。",

  "privacy.s6.title": "6. 第三方服务",
  "privacy.s6.desc": "本网站与以下第三方服务交互：",
  "privacy.s6.item1":
    "GitHub API：在服务端用于获取发布信息和处理反馈提交。您的 IP 地址不会转发至 GitHub。",
  "privacy.s6.item2":
    "Google Fonts：用于加载 Inter 字体。受 Google 隐私政策约束。",

  "privacy.s7.title": "7. 儿童隐私",
  "privacy.s7.desc":
    "FluxDown 不会故意收集 13 岁以下儿童的任何个人信息。由于桌面应用完全不收集个人数据，且网站数据收集仅限于自愿提交的反馈，我们认为本服务对所有年龄段的用户都是安全的。",

  "privacy.s8.title": "8. 政策变更",
  "privacy.s8.desc":
    "我们可能会不时更新本隐私政策。变更将发布在本页面，并更新修订日期。建议您定期查看本页面。",

  "privacy.s9.title": "9. 联系我们",
  "privacy.s9.desc":
    "如果您对本隐私政策有任何疑问，请通过我们网站的反馈页面与我们联系。",

  // Terms of Service
  "terms.title": "服务条款",
  "terms.lastUpdated": "最后更新：2026 年 2 月",
  "terms.intro":
    "请在使用 FluxDown 软件、浏览器扩展和网站（统称「服务」）前仔细阅读本服务条款（以下简称「条款」）。使用本服务即表示您同意受本条款约束。",

  "terms.s1.title": "1. 条款接受",
  "terms.s1.desc":
    "通过下载、安装或使用 FluxDown，您同意本条款。如果您不同意，请不要使用本服务。我们保留随时更新本条款的权利，继续使用即表示接受任何更改。",

  "terms.s2.title": "2. 许可",
  "terms.s2.desc":
    "FluxDown 作为免费软件提供。在遵守本条款的前提下，我们授予您一项非排他性、不可转让、可撤销的许可，允许您将本软件用于个人或商业目的。您可以：",
  "terms.s2.item1":
    "在您拥有或控制的任意数量的设备上下载、安装和使用 FluxDown。",
  "terms.s2.item2": "将浏览器扩展与桌面应用配合使用。",
  "terms.s2.item3": "与他人分享官方下载链接。",

  "terms.s3.title": "3. 合理使用",
  "terms.s3.desc": "您同意仅将 FluxDown 用于合法目的。您不得使用本服务：",
  "terms.s3.item1":
    "下载侵犯他人知识产权的内容，包括未经授权的受版权保护的材料。",
  "terms.s3.item2": "违反任何适用的地方、国家或国际法律法规。",
  "terms.s3.item3":
    "尝试对软件进行逆向工程、反编译或反汇编，除非适用法律允许。",
  "terms.s3.item4": "未经明确许可，以 FluxDown 名义分发修改版本的软件。",

  "terms.s4.title": "4. 知识产权",
  "terms.s4.desc":
    "FluxDown 名称、标志及相关品牌形象是 FluxDown 项目的知识产权。软件源代码受其各自的许可条款约束。本网站上的所有内容，包括文字、图形和设计，除另有说明外，均归 FluxDown 所有。",

  "terms.s5.title": "5. 免责声明",
  "terms.s5.desc":
    "FluxDown 按「原样」和「可用」的基础提供，不提供任何明示或暗示的担保，包括但不限于对适销性、特定用途适用性和不侵权的暗示担保。我们不保证服务不会中断、无错误或不含有害成分。",

  "terms.s6.title": "6. 责任限制",
  "terms.s6.desc":
    "在适用法律允许的最大范围内，FluxDown 及其贡献者不对任何间接的、附带的、特殊的、后果性的或惩罚性的损害赔偿负责，也不对因您使用本服务而直接或间接产生的任何利润或收入损失、数据丢失、使用损失、商誉损失或其他无形损失负责。",

  "terms.s7.title": "7. 用户内容与反馈",
  "terms.s7.desc":
    "当您通过网站提交反馈、错误报告或功能请求时，您授予我们非排他性的、全球范围内的、免版税的许可，允许我们使用、复制和展示该内容，以改进 FluxDown。我们不会与第三方分享您的联系信息。",

  "terms.s8.title": "8. 终止",
  "terms.s8.desc":
    "您可以随时通过卸载软件和移除浏览器扩展来停止使用 FluxDown。我们保留随时修改或终止服务的权利，恕不另行通知。终止后，本条款中因其性质应当继续有效的所有条款将继续有效。",

  "terms.s9.title": "9. 条款变更",
  "terms.s9.desc":
    "我们保留随时修改本条款的权利。更新后的条款将发布在本页面，并注明修订日期。在任何更改之后继续使用本服务即表示您接受新条款。",

  "terms.s10.title": "10. 联系我们",
  "terms.s10.desc":
    "如果您对本服务条款有任何疑问，请通过我们网站的反馈页面与我们联系。",

  // Sponsor (自由付款，接入 zerx pay 网关)
  "sponsor.badge": "支持",
  "sponsor.title": "赞助 ",
  "sponsor.titleHighlight": "FluxDown",
  "sponsor.subtitle":
    "FluxDown 是完全免费项目。任意金额，您的支持将持续推动开发进程。",
  "sponsor.ctaHint": "微信安全支付 · 扫描二维码完成付款",
  "sponsor.pay.cta": "赞助",
  "sponsor.pay.customPlaceholder": "自定义金额",
  "sponsor.pay.wechatBadge": "微信支付",
  "sponsor.pay.scanTitle": "扫码支付",
  "sponsor.pay.scanHint": "打开微信扫描下方二维码",
  "sponsor.pay.waiting": "等待支付中…",
  "sponsor.pay.thanksTitle": "感谢支持！",
  "sponsor.pay.thanksBody": "您的支持是 FluxDown 前进的动力。",
  "sponsor.pay.errorTitle": "支付失败",
  "sponsor.pay.failed": "出错了，请重试。",
  "sponsor.pay.timeout": "支付超时，请重试。",
  "sponsor.pay.unavailable": "支付功能暂时不可用。",
  "sponsor.pay.retry": "重试",
  "sponsor.wall.namePlaceholder": "您的名称（选填，将展示在赞助名录）",
  "sponsor.wall.messagePlaceholder": "留言（选填）",
  "sponsor.wall.hint":
    "支付成功后，名称与留言将自动登记到 GitHub 赞助名录；留空则匿名赞助。",
  "sponsor.wall.link": "查看赞助名录",
  "sponsor.wall.thanksNote": "您的留言将很快出现在赞助名录中。",
  "sponsor.list.title": "最新赞助",

  // Footer
  "footer.desc":
    "基于 Rust 构建的下载管理器，支持 HTTP/FTP/BT 多协议加速、智能分段、浏览器无缝集成。极速可靠，零广告，永久免费。",
  "footer.product": "产品",
  "footer.features": "功能特性",
  "footer.browserExtension": "浏览器扩展",
  "footer.download": "下载",
  "footer.support": "资源",
  "footer.documentation": "文档",
  "footer.faq": "常见问题",
  "footer.contact": "联系我们",
  "footer.feedback": "反馈建议",
  "footer.changelog": "更新日志",
  "footer.themeBuilder": "主题构建器",
  "footer.sponsor": "赞助支持",
  "footer.legal": "法律",
  "footer.privacy": "隐私政策",
  "footer.terms": "服务条款",
  "footer.community": "社区",
  "footer.announcements": "公告动态",
  "footer.logoVote": "Logo 投票",
  "footer.macosGatekeeper": "macOS 修复",
  "footer.copyright": "© {year} FluxDown. 保留所有权利。",
  "footer.builtWith": "Astro + React + Rust",

  // Project Board（看板视图）
  "board.tabLabel": "反馈追踪",
  "board.loading": "加载看板数据...",
  "board.error": "加载看板失败，请稍后重试。",
  "board.noToken": "暂未配置看板访问权限。",
  "board.empty": "暂无内容。",
  "board.noStatus": "未分类",
  "board.issueCount": "{n} 条",
  "board.open": "待处理",
  "board.closed": "已完成",
  "board.viewOnGithub": "在 GitHub 查看",
  "board.colorGray": "灰色",
  "board.colorBlue": "蓝色",
  "board.colorGreen": "绿色",
  "board.colorYellow": "黄色",
  "board.colorOrange": "橙色",
  "board.colorRed": "红色",
  "board.colorPink": "粉色",
  "board.colorPurple": "紫色",
};

export default zhCN;
