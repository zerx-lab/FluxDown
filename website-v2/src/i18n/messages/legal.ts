import { defineMessages } from "../define";

/**
 * 法务类长文(隐私政策 / 服务条款 / 代码签名政策)。
 *
 * 每篇文档是 `sections` 数组;`body` 与 `items` 允许内联 HTML(链接、强调、代码),
 * 由 `LegalDoc` 以 `set:html` 渲染——内容均为本文件中的静态字面量,不接受外部输入。
 */
const GH = "https://github.com/zerx-lab/FluxDown";
const ext = (url: string, label: string) =>
  `<a href="${url}" rel="noopener noreferrer" target="_blank">${label}</a>`;

export const legal = defineMessages({
  en: {
    index: "On this page",
    updated: "Last updated",
    privacy: {
      meta: {
        title: "Privacy Policy - FluxDown",
        description:
          "FluxDown privacy policy. No ads, no tracking, account optional. Download data stays on your device; this page lists exactly what the apps and the optional FluxDown account send.",
      },
      eyebrow: "Legal · Privacy",
      title: "Privacy <em>Policy</em>",
      updated: "November 2026",
      intro:
        'FluxDown ("we", "our", or "the software") is committed to protecting your privacy. This Privacy Policy explains what information we collect, what we do not collect, and how your data is handled when you use the FluxDown desktop application, the FluxDown mobile apps for iOS and Android, the browser extension, the optional FluxDown account, and this website.',
      sections: [
        {
          id: "collect",
          title: "Information We Collect",
          body: "We collect minimal information necessary to provide and improve our services:",
          items: [
            "<strong>Website analytics:</strong> We use Vercel Web Analytics, a privacy-friendly analytics service that collects anonymous, aggregated page view data. No cookies are used, no personal data is collected, and no individual users are tracked.",
            "<strong>Feedback submissions:</strong> When you voluntarily submit feedback through our website, we collect the feedback type, title, description, and optionally your contact information. This data is stored as GitHub Issues in our repository.",
            "<strong>Email subscriptions:</strong> If you subscribe to platform availability notifications, we store your email address in a GitHub Issue. This is used solely to notify you when the requested platform becomes available.",
            "<strong>Anonymous usage counters (desktop and server):</strong> The desktop application and the headless server send two anonymous events: <code>app_installed</code> once and <code>app_active</code> at most once a day. They contain a random installation ID, the operating system, CPU architecture and app version — never URLs, file names or account data. You can turn them off with the Anonymous Usage Statistics switch in settings. The mobile apps do not send these events.",
          ],
        },
        {
          id: "account",
          title: "Optional FluxDown Account",
          body: "Every download feature works without an account. If you choose to create or sign in to a FluxDown account, we store the following on our servers to provide account features:",
          items: [
            "<strong>Account details:</strong> your email address, your password (stored only as a salted hash), and the nickname you choose.",
            "<strong>Signed-in devices:</strong> for each device you sign in on, a randomly generated device ID, the device name, platform, app version, and the IP address and time it was last seen, so you can see and manage your devices and keep your account secure.",
            "<strong>Settings sync (optional):</strong> if you turn on sync, the settings groups you select are stored so they can be applied on your other devices. Download tasks and history are never synced.",
            "<strong>Links you send to your devices:</strong> when you choose to send a download to another of your devices, the link and its options are stored so that device can receive and run it.",
            "<strong>Cloud notifications (opt-in, desktop and server):</strong> if you enable them, task events (file name, size, status, error and queue name) are sent so we can deliver email or Telegram notifications. Download URLs and save folders are only included if you turn on those options separately. Events are kept for 7 days.",
            "<strong>Download route quality:</strong> while signed in, the app reports connection measurements for download servers (server host name, IP address, connection time, throughput and success) so FluxDown can choose faster routes. These measurements never include URL paths, query strings, file names or tokens.",
            "<strong>Plans:</strong> if you buy an optional plan on the desktop app or the website, we keep the order record (plan, amount and status) with your account. The mobile apps do not offer purchases.",
            "<strong>Private extension grants and download records:</strong> if an administrator enables a private extension (an extension built for specific customers) for your account, we store the grant (the extension, its validity period and whether it was revoked) and a record of each download of that private extension's package (your account, the extension and version, and the time). Download records are deleted automatically after 365 days, and they do not include your IP address or device information. This data is used only to check that you are entitled to the extension, to safely withdraw a version that turns out to be malicious (the app receives the withdrawal list for extensions you were granted, including after a grant is revoked), and for access auditing. It does not involve your own downloads or download history.",
          ],
        },
        {
          id: "bt-guard",
          title: "BT Leech Protection Evidence Reports",
          body: 'The BT leech protection feature can optionally report evidence about abusive BitTorrent peers to FluxDown so that a shared, signed block list can be built. <strong>It is off by default, and nothing is reported unless you are signed in and have explicitly turned on the evidence reporting switch.</strong> Detection itself runs only on your device. If you turn reporting on, we process:',
          items: [
            "<strong>What is reported:</strong> for each peer that matched a detection rule: the peer's IP address group (a single IPv4 address, or an IPv6 /64 network), the rule that matched, metering figures (bytes delivered to it, redundant bytes, the delivered-to-torrent-size ratio, re-request and rewind counters), the time range it was observed in, and the peer's client fingerprint (the peer ID prefix and client name it announced).",
            "<strong>Torrent identifier:</strong> the torrent's infohash is turned into a one-way keyed hash (HMAC) with a secret key on our server as soon as it arrives. The original infohash is never stored, so the hash cannot be traced back to a torrent.",
            "<strong>What is never reported:</strong> file names, paths and torrent names are not included. Private torrents are never reported. Peers on local, loopback and link-local networks are never reported.",
            "<strong>Purpose:</strong> reports from many signed-in users are compared; only when several independent users report the same address group for several different torrents is it added to a public, signed block list that the apps can download. A single report never lists anyone.",
            "<strong>Retention:</strong> evidence is deleted after a 14-day window. A list entry is taken down automatically 14 days after the last new evidence for it.",
            "<strong>Third-party data:</strong> the block list also contains community data from BTN-Collected-Rules by PBH-BTN, licensed under CC BY 4.0 (" + ext("https://github.com/PBH-BTN/BTN-Collected-Rules", "source") + ").",
            "<strong>If you were listed:</strong> list entries are shown by IP address group only, never with personal data. You can check whether your current IP address is listed, see the reason and the expiry time, and file an appeal on the <a href=\"/bt-guard\">BT leech protection self-check page</a>. Approved appeals are removed and allow-listed.",
          ],
        },
        {
          id: "not-collect",
          title: "Information We Do NOT Collect",
          body: "FluxDown is designed with a local-first architecture. The FluxDown apps:",
          items: [
            "Do NOT collect or store your download history on any remote server. Download tasks, file paths and progress are stored locally in a SQLite database on your device. Download links only leave your device when you explicitly send them to another of your devices or enable the notification options described above.",
            "Do NOT include advertising, third-party trackers, or crash reporting.",
            "Do NOT require an account, login, or registration to download files.",
            "Do NOT sell or share your personal data with third parties for advertising or tracking.",
          ],
        },
        {
          id: "mobile",
          title: "Mobile Apps",
          body: "The FluxDown apps for iOS and Android run the same download engine locally on your device:",
          items: [
            "Downloaded files and task records stay in the app's storage on your device until you delete them.",
            "Local network access is used only to discover and connect to FluxDown hosts (your computer, NAS or server) that you add or pair with. Connections to your own hosts go directly to them, not through our servers.",
            "Face ID / Touch ID is used only on the device to protect access keys; biometric data never leaves your device and is never available to us.",
            "Notifications about finished or failed downloads are generated locally on your device.",
          ],
        },
        {
          id: "extension",
          title: "Browser Extension",
          body: "The FluxDown browser extension communicates exclusively with the FluxDown desktop application via a local HTTP endpoint (<code>localhost:19527</code>). Specifically:",
          items: [
            "All data transfer occurs locally on your machine between the browser extension and the desktop application. No data is sent to any external server.",
            "The extension stores your preferences (auto-intercept toggle, file type filters, domain rules) in your browser's local storage (<code>chrome.storage.sync/local</code>). This data syncs only through your browser's built-in sync mechanism, if enabled.",
            "The extension does not access, read, or modify any web page content beyond intercepting download requests.",
          ],
        },
        {
          id: "analytics",
          title: "Website Analytics",
          body: "This website uses Vercel Web Analytics, which is a privacy-focused analytics service. It does not use cookies, does not collect personal information, does not track individual users across sessions, and is compliant with GDPR, CCPA, and other privacy regulations without requiring a cookie consent banner.",
          items: [],
        },
        {
          id: "storage",
          title: "Data Storage & Security",
          body: "All download data (task records, file paths, progress) is stored locally on your device in a SQLite database managed by the FluxDown application. We have no access to this data. Account data is stored on our own servers and transmitted over HTTPS. Website feedback and subscription data is stored in GitHub Issues within our repository, subject to GitHub's privacy policy.",
          items: [],
        },
        {
          id: "deletion",
          title: "Your Choices & Account Deletion",
          body: "You stay in control of the data described above:",
          items: [
            "Sign out at any time, or remove individual devices from your account, in the app's account settings.",
            "Turn settings sync, cloud notifications and the anonymous usage counters on or off in settings.",
            'Delete your FluxDown account at any time in the app: Settings → Account → Delete Account (confirmed with your password or an email code). This permanently removes your account, signed-in devices, synced settings, sent links, notification settings and private extension grants, and deletes your private extension download records. Administrators\' operation logs of granting and revoking access are kept for security auditing but are no longer linked to your account. Order records are kept only where we are legally required to. Files you downloaded stay on your device. If you cannot access the app, contact us through the <a href="{feedback}">Feedback page</a> from your account email.',
          ],
        },
        {
          id: "third-party",
          title: "Third-Party Services",
          body: "Our website interacts with the following third-party services:",
          items: [
            "<strong>GitHub API:</strong> Used server-side to fetch release information and process feedback submissions. Your IP address is not forwarded to GitHub.",
            "<strong>Fonts:</strong> All web fonts are self-hosted on this website. No font requests are made to Google Fonts or any other third-party font service.",
          ],
        },
        {
          id: "children",
          title: "Children's Privacy",
          body: "FluxDown does not knowingly collect any personal information from children under the age of 13. Downloading files does not require an account, and the optional account only stores the data described above. If you believe a child under 13 has created an account, contact us and we will delete it.",
          items: [],
        },
        {
          id: "changes",
          title: "Changes to This Policy",
          body: "We may update this Privacy Policy from time to time. Changes will be posted on this page with an updated revision date. We encourage you to review this page periodically.",
          items: [],
        },
        {
          id: "contact",
          title: "Contact Us",
          body: 'If you have any questions about this Privacy Policy, please reach out to us through the <a href="{feedback}">Feedback page</a> on our website.',
          items: [],
        },
      ],
    },
    terms: {
      meta: {
        title: "Terms of Service - FluxDown",
        description:
          "FluxDown terms of service: license, acceptable use, warranties and liability for the FluxDown desktop app, browser extension and website.",
      },
      eyebrow: "Legal · Terms",
      title: "Terms of <em>Service</em>",
      updated: "October 2026",
      intro:
        'Please read these Terms of Service ("Terms") carefully before using FluxDown software, browser extension, and website (collectively, the "Service"). By using the Service, you agree to be bound by these Terms.',
      sections: [
        {
          id: "acceptance",
          title: "Acceptance of Terms",
          body: "By downloading, installing, or using FluxDown, you agree to these Terms. If you do not agree, please do not use the Service. We reserve the right to update these Terms at any time, and continued use constitutes acceptance of any changes.",
          items: [],
        },
        {
          id: "license",
          title: "License",
          body: "FluxDown is provided as free software. Subject to these Terms, we grant you a non-exclusive, non-transferable, revocable license to use the software for personal or commercial purposes. You may:",
          items: [
            "Download, install, and use FluxDown on any number of devices you own or control.",
            "Use the browser extension alongside the desktop application.",
            "Share the official download link with others.",
          ],
        },
        {
          id: "acceptable-use",
          title: "Acceptable Use",
          body: "You agree to use FluxDown only for lawful purposes. You must NOT use the Service to:",
          items: [
            "Download content that infringes upon the intellectual property rights of others, including copyrighted material without authorization.",
            "Violate any applicable local, national, or international laws or regulations.",
            "Attempt to reverse-engineer, decompile, or disassemble the software, except as permitted by applicable law.",
            "Distribute modified versions of the software under the FluxDown name without explicit permission.",
          ],
        },
        {
          id: "ip",
          title: "Intellectual Property",
          body: "The FluxDown name, logo, and associated branding are the intellectual property of the FluxDown project. The software's source code is subject to its respective license terms. All content on this website, including text, graphics, and design, is owned by FluxDown unless otherwise noted.",
          items: [],
        },
        {
          id: "warranties",
          title: "Disclaimer of Warranties",
          body: 'FluxDown is provided "AS IS" and "AS AVAILABLE" without warranties of any kind, either express or implied, including but not limited to implied warranties of merchantability, fitness for a particular purpose, and non-infringement. We do not warrant that the Service will be uninterrupted, error-free, or free of harmful components.',
          items: [],
        },
        {
          id: "liability",
          title: "Limitation of Liability",
          body: "To the maximum extent permitted by applicable law, FluxDown and its contributors shall not be liable for any indirect, incidental, special, consequential, or punitive damages, or any loss of profits or revenues, whether incurred directly or indirectly, or any loss of data, use, goodwill, or other intangible losses resulting from your use of the Service.",
          items: [],
        },
        {
          id: "feedback",
          title: "User Content & Feedback",
          body: "When you submit feedback, bug reports, or feature requests through our website, you grant us a non-exclusive, worldwide, royalty-free license to use, reproduce, and display such content for the purpose of improving FluxDown. We will not share your contact information with third parties.",
          items: [],
        },
        {
          id: "termination",
          title: "Termination",
          body: "You may stop using FluxDown at any time by uninstalling the software and removing the browser extension. We reserve the right to modify or discontinue the Service at any time without prior notice. Upon termination, all provisions of these Terms that by their nature should survive will remain in effect.",
          items: [],
        },
        {
          id: "plans",
          title: "Plans & Membership Cards",
          body: "These rules apply to the plans that come with FluxCloud.",
          items: [
            "Membership cards are bought for a period and renewed manually; they never auto-renew and are never charged automatically. Renewing the same card extends it from its current end date. When a card expires you automatically return to your permanent plan's entitlements. Time that has already taken effect is non-refundable.",
            "For plans marked as entitlement-locked, published entitlements can only be increased, never reduced; the plan page states whether a plan is locked.",
            "Plans are for your own use. Reselling, sharing accounts or abuse (including bulk automated downloading) may result in restricted or revoked entitlements.",
          ],
        },
        {
          id: "changes",
          title: "Changes to These Terms",
          body: "We reserve the right to modify these Terms at any time. Updated Terms will be posted on this page with a revised date. Your continued use of the Service after any changes indicates your acceptance of the new Terms.",
          items: [],
        },
        {
          id: "contact",
          title: "Contact Us",
          body: 'If you have any questions about these Terms of Service, please reach out to us through the <a href="{feedback}">Feedback page</a> on our website.',
          items: [],
        },
      ],
    },
    signing: {
      meta: {
        title: "Code Signing Policy - FluxDown",
        description:
          "FluxDown code signing policy. Free code signing provided by SignPath.io, certificate by SignPath Foundation.",
      },
      eyebrow: "Trust · Code signing",
      title: "Code Signing <em>Policy</em>",
      updated: "July 10, 2026",
      intro: `This project signs and distributes release artifacts. Free code signing provided by ${ext("https://about.signpath.io", "SignPath.io")}, certificate by ${ext("https://signpath.org", "SignPath Foundation")}.`,
      pipeline: {
        caption: "signing pipeline · windows",
        steps: [
          { k: "source", v: "zerx-lab/FluxDown" },
          { k: "build", v: "GitHub Actions CI" },
          { k: "approve", v: "Maintainer, manual" },
          { k: "sign", v: "SignPath · HSM key" },
          { k: "publish", v: "GitHub Releases" },
        ],
      },
      sections: [
        {
          id: "what",
          title: "What is signed",
          body: "",
          items: [
            `Windows installer packages and executables (<code>.exe</code>, <code>.msi</code>) published on ${ext(`${GH}/releases`, "GitHub Releases")}.`,
          ],
        },
        {
          id: "process",
          title: "Build and signing process",
          body: "",
          items: [
            `All artifacts are built from the public repository ${ext(GH, "zerx-lab/FluxDown")} using GitHub Actions CI.`,
            "Only CI-built artifacts are submitted to SignPath for signing.",
            "The private key is held by SignPath on HSM. This project does not store or have access to the private key.",
            "Every signing request requires explicit manual approval by the maintainer.",
          ],
        },
        {
          id: "roles",
          title: "Team roles",
          body: "",
          items: [
            `<strong>Authors</strong> (commit access, can modify the repository without additional reviews): ${ext("https://github.com/zerx-lab", "zerx-lab")}`,
            `<strong>Reviewers</strong> (all external pull requests are reviewed before merge): ${ext("https://github.com/zerx-lab", "zerx-lab")}`,
            `<strong>Approvers</strong> (each signing request requires explicit approval): ${ext("https://github.com/zerx-lab", "zerx-lab")}`,
          ],
        },
        {
          id: "platforms",
          title: "Other platforms",
          body: "",
          items: [
            "macOS: the app and DMG are signed with an Apple Developer ID (Hardened Runtime) and notarized by Apple; the notarization ticket is stapled, so Gatekeeper verifies them even offline.",
            "Linux: artifacts (AppImage / deb / Arch / portable) are currently not cryptographically signed; obtain them only from the official GitHub Releases page.",
          ],
        },
        {
          id: "privacy",
          title: "Privacy policy",
          body: 'FluxDown does not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it (e.g. checking for updates on GitHub Releases). See the full <a href="{privacy}">Privacy Policy</a>.',
          items: [],
        },
      ],
    },
  },
  zh: {
    index: "本页目录",
    updated: "最后更新",
    privacy: {
      meta: {
        title: "隐私政策 - FluxDown",
        description: "FluxDown 隐私政策。无广告、无追踪，账户可选。下载数据保存在你的设备本地；本页逐项列出各应用与可选的 FluxDown 账户会发送哪些数据。",
      },
      eyebrow: "法律 · 隐私",
      title: "隐私<em>政策</em>",
      updated: "2026 年 11 月",
      intro:
        "FluxDown（以下简称「我们」或「本软件」）致力于保护您的隐私。本隐私政策说明了当您使用 FluxDown 桌面应用、iOS 与 Android 移动应用、浏览器扩展、可选的 FluxDown 账户和本网站时，我们收集哪些信息、不收集哪些信息以及如何处理您的数据。",
      sections: [
        {
          id: "collect",
          title: "我们收集的信息",
          body: "我们仅收集提供和改进服务所必需的最少信息：",
          items: [
            "<strong>网站分析：</strong>我们使用 Vercel Web Analytics，这是一项注重隐私的分析服务，仅收集匿名的、聚合的页面浏览数据。不使用 Cookie，不收集个人数据，不追踪个人用户。",
            "<strong>反馈提交：</strong>当您通过网站自愿提交反馈时，我们收集反馈类型、标题、描述以及可选的联系方式。这些数据以 GitHub Issue 的形式存储在我们的代码仓库中。",
            "<strong>邮件订阅：</strong>如果您订阅了平台可用性通知，我们会将您的邮箱地址存储在 GitHub Issue 中，仅用于在所请求的平台可用时通知您。",
            "<strong>匿名使用计数（桌面端与服务器端）：</strong>桌面应用与无界面服务器会发送两个匿名事件：安装时一次 <code>app_installed</code>，每天至多一次 <code>app_active</code>。其中只包含随机安装 ID、操作系统、CPU 架构和应用版本，绝不包含链接、文件名或账户信息，可在设置中通过「匿名使用统计」开关关闭。移动应用不发送这些事件。",
          ],
        },
        {
          id: "account",
          title: "可选的 FluxDown 账户",
          body: "不登录账户也能使用全部下载功能。如果您选择注册或登录 FluxDown 账户，我们会在服务器上保存以下数据以提供账户功能：",
          items: [
            "<strong>账户资料：</strong>您的邮箱地址、密码（仅以加盐哈希形式保存）以及您设置的昵称。",
            "<strong>已登录设备：</strong>每台登录设备的随机设备 ID、设备名称、平台、应用版本，以及最近一次在线的 IP 地址与时间，用于让您查看和管理设备并保障账户安全。",
            "<strong>设置同步（可选）：</strong>开启同步后，保存您所选的设置分组，以便应用到您的其他设备。下载任务与历史永不同步。",
            "<strong>发送到您设备的链接：</strong>当您选择把下载发送到自己的另一台设备时，会保存该链接及其选项，以便那台设备接收并执行。",
            "<strong>云端推送（需主动开启，桌面端与服务器端）：</strong>开启后会发送任务事件（文件名、大小、状态、错误与队列名），用于投递邮件或 Telegram 通知。下载地址与保存目录只有在您分别开启对应选项时才会包含。事件保留 7 天。",
            "<strong>下载线路质量：</strong>登录期间，应用会上报下载服务器的连接测量数据（服务器主机名、IP 地址、连接耗时、吞吐量与是否成功），用于为 FluxDown 选择更快的线路。其中绝不包含链接路径、查询参数、文件名或令牌。",
            "<strong>套餐：</strong>如果您在桌面应用或网站上购买可选套餐，我们会在账户中保存订单记录（套餐、金额与状态）。移动应用不提供任何购买。",
            "<strong>私有扩展授权与下载记录：</strong>如果管理员为您的账户开通了某个私有扩展（面向特定客户定制的扩展），我们会保存授权记录（扩展、有效期、是否已撤销），以及每次下载该私有扩展安装包的记录（您的账户、扩展与版本、下载时间）。下载记录保留 365 天后自动删除；这些记录不包含您的 IP 地址或设备信息。这些数据只用于三件事：校验您是否有权获取该扩展；在某个版本被判定为恶意时安全撤回（应用会收到您曾获授权的扩展的撤回清单，授权被撤销后同样如此）；以及访问审计。该功能不涉及您自己的下载内容或下载历史。",
          ],
        },
        {
          id: "bt-guard",
          title: "BT 防吸血证据上报",
          body: 'BT 防吸血功能可以选择性地把恶意 BitTorrent 对端的证据上报给 FluxDown，用于生成共享的签名黑名单。<strong>该功能默认关闭；只有您已登录并明确开启「证据上报」开关后才会上报任何内容。</strong>检测本身只在您的设备上运行。开启上报后，我们会处理：',
          items: [
            "<strong>上报内容：</strong>对每个命中检测规则的对端：其 IP 地址组（单个 IPv4 地址，或 IPv6 /64 网络）、命中的规则、计量数据（向其送达的字节数、重复字节数、送达量与种子体积之比、重复请求与回退计数）、观察到的时间段，以及对端的客户端特征（其声明的 peer ID 前缀与客户端名称）。",
            "<strong>种子标识：</strong>种子的 infohash 在到达我们服务器时立即用服务端密钥做单向散列（HMAC），不保存原值，散列值无法反推出具体种子。",
            "<strong>绝不上报的内容：</strong>不包含文件名、路径与种子名；私有种子永不上报；局域网、回环与链路本地地址的对端永不上报。",
            "<strong>用途：</strong>我们对多位已登录用户的上报做交叉比对；只有多个独立用户针对多个不同种子上报同一地址组时，才会把它加入应用可下载的公开签名名单。单次上报不会使任何人被列入。",
            "<strong>保存期限：</strong>证据在 14 天窗口后删除；名单条目在最近一次新证据后 14 天无新证据即自动下架。",
            "<strong>第三方数据：</strong>名单还包含来自 PBH-BTN 的 BTN-Collected-Rules 的社区数据，采用 CC BY 4.0 许可（" + ext("https://github.com/PBH-BTN/BTN-Collected-Rules", "来源") + "）。",
            "<strong>如果您被列入名单：</strong>名单条目只以 IP 地址组展示，不含任何个人信息。您可以在 <a href=\"/zh/bt-guard\">BT 防吸血自查页</a> 查询当前 IP 地址是否在名单中、查看原因与到期时间并提出申诉；申诉通过后条目会被移除并加入白名单。",
          ],
        },
        {
          id: "not-collect",
          title: "我们不收集的信息",
          body: "FluxDown 采用本地优先架构设计。FluxDown 各应用：",
          items: [
            "不会在任何远程服务器上收集或保存您的下载历史。下载任务、文件路径与进度均存储在您设备上的本地 SQLite 数据库中。只有当您主动把链接发送到自己的其他设备，或开启上文所述的推送选项时，下载链接才会离开您的设备。",
            "不包含广告、第三方追踪器或崩溃报告。",
            "下载文件不需要任何形式的账户、登录或注册。",
            "不会出于广告或追踪目的向第三方出售或共享您的个人数据。",
          ],
        },
        {
          id: "mobile",
          title: "移动应用",
          body: "FluxDown 的 iOS 与 Android 应用在您的设备本地运行同一套下载引擎：",
          items: [
            "下载的文件与任务记录保存在您设备上的应用存储中，直到您将其删除。",
            "本地网络权限仅用于发现并连接您添加或配对的 FluxDown 主机（您的电脑、NAS 或服务器）。与您自己主机的连接直接建立，不经过我们的服务器。",
            "面容 ID / 触控 ID 仅在设备本地用于保护访问密钥；生物识别数据不会离开您的设备，我们也无法获取。",
            "下载完成或失败的通知均由您的设备在本地生成。",
          ],
        },
        {
          id: "extension",
          title: "浏览器扩展",
          body: "FluxDown 浏览器扩展仅通过本地 HTTP 端点（<code>localhost:19527</code>）与 FluxDown 桌面应用通信。具体而言：",
          items: [
            "所有数据传输都在您的设备本地进行，发生在浏览器扩展和桌面应用之间。不会向任何外部服务器发送数据。",
            "扩展将您的偏好设置（自动拦截开关、文件类型过滤器、域名规则）存储在浏览器的本地存储中（<code>chrome.storage.sync/local</code>）。这些数据仅通过浏览器自带的同步机制进行同步（如已启用）。",
            "扩展不会访问、读取或修改任何网页内容，仅拦截下载请求。",
          ],
        },
        {
          id: "analytics",
          title: "网站分析",
          body: "本网站使用 Vercel Web Analytics，这是一项注重隐私的分析服务。它不使用 Cookie、不收集个人信息、不跨会话追踪用户，符合 GDPR、CCPA 等隐私法规，无需显示 Cookie 同意横幅。",
          items: [],
        },
        {
          id: "storage",
          title: "数据存储与安全",
          body: "所有下载数据（任务记录、文件路径、进度）均存储在您设备上由 FluxDown 应用管理的本地 SQLite 数据库中。我们无法访问这些数据。账户数据保存在我们自己的服务器上，并通过 HTTPS 传输。网站反馈和订阅数据存储在我们 GitHub 仓库的 Issue 中，受 GitHub 隐私政策约束。",
          items: [],
        },
        {
          id: "deletion",
          title: "您的选择与账户删除",
          body: "您可以随时掌控上述数据：",
          items: [
            "随时在应用的账户设置中退出登录，或从账户中移除单台设备。",
            "在设置中开启或关闭设置同步、云端推送与匿名使用计数。",
            '您可以随时在应用中删除 FluxDown 账户：设置 → 账户 → 删除账户（需验证密码或邮箱验证码）。删除后，账户、已登录设备、同步的设置、发送的链接、推送设置与私有扩展授权将被永久移除，您的私有扩展下载记录也会一并删除；管理员对授权与撤销的操作日志会保留用于安全审计，但不再与您的账户关联；仅在法律要求时保留订单记录。已下载的文件仍保留在您的设备上。如果无法使用应用，请用账户邮箱通过<a href="{feedback}">反馈页面</a>联系我们。',
          ],
        },
        {
          id: "third-party",
          title: "第三方服务",
          body: "本网站与以下第三方服务交互：",
          items: [
            "<strong>GitHub API：</strong>在服务端用于获取发布信息和处理反馈提交。您的 IP 地址不会转发至 GitHub。",
            "<strong>字体：</strong>本网站所有网页字体均为自托管，不会向 Google Fonts 或任何第三方字体服务发起请求。",
          ],
        },
        {
          id: "children",
          title: "儿童隐私",
          body: "FluxDown 不会故意收集 13 岁以下儿童的任何个人信息。下载文件无需账户，可选账户只保存上文所述的数据。如果您认为有 13 岁以下儿童注册了账户，请联系我们，我们会将其删除。",
          items: [],
        },
        {
          id: "changes",
          title: "政策变更",
          body: "我们可能会不时更新本隐私政策。变更将发布在本页面，并更新修订日期。建议您定期查看本页面。",
          items: [],
        },
        {
          id: "contact",
          title: "联系我们",
          body: '如果您对本隐私政策有任何疑问，请通过我们网站的<a href="{feedback}">反馈页面</a>与我们联系。',
          items: [],
        },
      ],
    },
    terms: {
      meta: {
        title: "服务条款 - FluxDown",
        description: "FluxDown 服务条款：FluxDown 桌面应用、浏览器扩展与网站的许可、合理使用、担保与责任限制。",
      },
      eyebrow: "法律 · 条款",
      title: "服务<em>条款</em>",
      updated: "2026 年 10 月",
      intro:
        "请在使用 FluxDown 软件、浏览器扩展和网站（统称「服务」）前仔细阅读本服务条款（以下简称「条款」）。使用本服务即表示您同意受本条款约束。",
      sections: [
        {
          id: "acceptance",
          title: "条款接受",
          body: "通过下载、安装或使用 FluxDown，您同意本条款。如果您不同意，请不要使用本服务。我们保留随时更新本条款的权利，继续使用即表示接受任何更改。",
          items: [],
        },
        {
          id: "license",
          title: "许可",
          body: "FluxDown 作为免费软件提供。在遵守本条款的前提下，我们授予您一项非排他性、不可转让、可撤销的许可，允许您将本软件用于个人或商业目的。您可以：",
          items: [
            "在您拥有或控制的任意数量的设备上下载、安装和使用 FluxDown。",
            "将浏览器扩展与桌面应用配合使用。",
            "与他人分享官方下载链接。",
          ],
        },
        {
          id: "acceptable-use",
          title: "合理使用",
          body: "您同意仅将 FluxDown 用于合法目的。您不得使用本服务：",
          items: [
            "下载侵犯他人知识产权的内容，包括未经授权的受版权保护的材料。",
            "违反任何适用的地方、国家或国际法律法规。",
            "尝试对软件进行逆向工程、反编译或反汇编，除非适用法律允许。",
            "未经明确许可，以 FluxDown 名义分发修改版本的软件。",
          ],
        },
        {
          id: "ip",
          title: "知识产权",
          body: "FluxDown 名称、标志及相关品牌形象是 FluxDown 项目的知识产权。软件源代码受其各自的许可条款约束。本网站上的所有内容，包括文字、图形和设计，除另有说明外，均归 FluxDown 所有。",
          items: [],
        },
        {
          id: "warranties",
          title: "免责声明",
          body: "FluxDown 按「原样」和「可用」的基础提供，不提供任何明示或暗示的担保，包括但不限于对适销性、特定用途适用性和不侵权的暗示担保。我们不保证服务不会中断、无错误或不含有害成分。",
          items: [],
        },
        {
          id: "liability",
          title: "责任限制",
          body: "在适用法律允许的最大范围内，FluxDown 及其贡献者不对任何间接的、附带的、特殊的、后果性的或惩罚性的损害赔偿负责，也不对因您使用本服务而直接或间接产生的任何利润或收入损失、数据丢失、使用损失、商誉损失或其他无形损失负责。",
          items: [],
        },
        {
          id: "feedback",
          title: "用户内容与反馈",
          body: "当您通过网站提交反馈、错误报告或功能请求时，您授予我们非排他性的、全球范围内的、免版税的许可，允许我们使用、复制和展示该内容，以改进 FluxDown。我们不会与第三方分享您的联系信息。",
          items: [],
        },
        {
          id: "termination",
          title: "终止",
          body: "您可以随时通过卸载软件和移除浏览器扩展来停止使用 FluxDown。我们保留随时修改或终止服务的权利，恕不另行通知。终止后，本条款中因其性质应当继续有效的所有条款将继续有效。",
          items: [],
        },
        {
          id: "plans",
          title: "套餐与会员卡",
          body: "以下规则适用于 FluxCloud 的套餐。",
          items: [
            "会员卡按周期购买、手动续费，不会自动续费或自动扣款；续费同一张卡时从当前到期时间顺延。到期后自动回到永久套餐的权益。已生效的时长不退款。",
            "开启「权益锁定」的套餐，已公示权益只增不减；是否锁定以套餐页标注为准。",
            "套餐限本人使用，转售、共享账号或滥用（含批量自动化下载）可限制或回收权益。",
          ],
        },
        {
          id: "changes",
          title: "条款变更",
          body: "我们保留随时修改本条款的权利。更新后的条款将发布在本页面，并注明修订日期。在任何更改之后继续使用本服务即表示您接受新条款。",
          items: [],
        },
        {
          id: "contact",
          title: "联系我们",
          body: '如果您对本服务条款有任何疑问，请通过我们网站的<a href="{feedback}">反馈页面</a>与我们联系。',
          items: [],
        },
      ],
    },
    signing: {
      meta: {
        title: "代码签名政策 - FluxDown",
        description: "FluxDown 代码签名政策。免费代码签名由 SignPath.io 提供，证书由 SignPath Foundation 颁发。",
      },
      eyebrow: "信任 · 代码签名",
      title: "代码签名<em>政策</em>",
      updated: "2026 年 7 月 10 日",
      intro: `本项目对发布产物进行签名并分发。免费代码签名由 ${ext("https://about.signpath.io", "SignPath.io")} 提供，证书由 ${ext("https://signpath.org", "SignPath Foundation")} 颁发。`,
      pipeline: {
        caption: "signing pipeline · windows",
        steps: [
          { k: "source", v: "zerx-lab/FluxDown" },
          { k: "build", v: "GitHub Actions CI" },
          { k: "approve", v: "维护者人工审批" },
          { k: "sign", v: "SignPath · HSM 私钥" },
          { k: "publish", v: "GitHub Releases" },
        ],
      },
      sections: [
        {
          id: "what",
          title: "签名范围",
          body: "",
          items: [
            `发布在 ${ext(`${GH}/releases`, "GitHub Releases")} 上的 Windows 安装包与可执行文件（<code>.exe</code>、<code>.msi</code>）。`,
          ],
        },
        {
          id: "process",
          title: "构建与签名流程",
          body: "",
          items: [
            `所有产物均由公开仓库 ${ext(GH, "zerx-lab/FluxDown")} 通过 GitHub Actions CI 构建。`,
            "只有 CI 构建的产物才会提交给 SignPath 签名。",
            "私钥由 SignPath 保存在 HSM 中。本项目不存储、也无法访问该私钥。",
            "每一次签名请求都需要维护者明确的人工审批。",
          ],
        },
        {
          id: "roles",
          title: "团队角色",
          body: "",
          items: [
            `<strong>作者</strong>（拥有提交权限，无需额外审查即可修改仓库）：${ext("https://github.com/zerx-lab", "zerx-lab")}`,
            `<strong>审查者</strong>（所有外部 Pull Request 合并前均经过审查）：${ext("https://github.com/zerx-lab", "zerx-lab")}`,
            `<strong>审批者</strong>（每一次签名请求都需明确审批）：${ext("https://github.com/zerx-lab", "zerx-lab")}`,
          ],
        },
        {
          id: "platforms",
          title: "其他平台",
          body: "",
          items: [
            "macOS：应用与 DMG 使用 Apple Developer ID 签名（Hardened Runtime）并经 Apple 公证，公证票据已装订（staple），离线时 Gatekeeper 也能校验。",
            "Linux：产物（AppImage / deb / Arch / 便携版）目前未做加密签名，请仅从官方 GitHub Releases 页面获取。",
          ],
        },
        {
          id: "privacy",
          title: "隐私政策",
          body: 'FluxDown 不会向其他联网系统传输任何信息，除非用户或安装、运行它的人明确要求（例如在 GitHub Releases 上检查更新）。完整内容请参阅<a href="{privacy}">隐私政策</a>。',
          items: [],
        },
      ],
    },
  },
});
