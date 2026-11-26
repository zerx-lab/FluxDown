import { defineMessages } from "../define";

/** 支持类页面文案:安全警告(仿冒站)、404。 */
export const guides = defineMessages("guides", {
  en: {
    security: {
      meta: {
        title: "Security Alert: Fake FluxDown Site - FluxDown",
        description:
          "A fake site (fluxdown.com.cn) is impersonating FluxDown and distributing the SilverFox remote-access trojan. Learn which domains are official and what to do if you downloaded from the fake site.",
      },
      eyebrow: "Security notice",
      title: "Security <em>Alert</em>",
      lede: 'We have found unofficial sites impersonating FluxDown and distributing malicious installers carrying the "SilverFox" remote-access trojan. Do not download or run any files from such sites to avoid loss of funds and privacy.',
      fakeLabel: "Fake site (do NOT visit)",
      fakeSite: "fluxdown.com.cn",
      officialLabel: "Official sites (trust only these domains)",
      primary: "primary",
      adviceTitle: "If you downloaded from the fake site",
      advice: [
        "Delete the installer immediately and do not run it.",
        "If you already ran it, disconnect from the network and run a full scan with reputable antivirus software — SilverFox is a remote-access trojan.",
        "Change passwords for important accounts from a clean device, and check payment/banking accounts for unusual activity.",
      ],
      verifyTitle: "How to verify a download",
      verify: [
        "Check the address bar: the domain must exactly match one of the official sites listed above. Look-alike domains with extra suffixes (such as .com.cn) are not ours.",
        "Release files are published on GitHub under zerx-lab/FluxDown. Any other GitHub account or mirror is unofficial.",
        "Windows installers from official channels are code-signed. Right-click the file → Properties → Digital Signatures to confirm a valid signature is present.",
      ],
      signingLink: "Code signing policy",
      downloadTitle: "Official download channels",
      downloadNote:
        "Only download FluxDown from the official sites above or from GitHub Releases. We never distribute installers through third-party download stations.",
      downloadCta: "Download from official site",
    },
    notFound: {
      title: "Page not found",
      desc: "The page you're looking for doesn't exist or has been moved.",
      readout: "segment 05 · http 404 · not found",
      home: "Back to Home",
      docs: "Documentation",
      download: "Download",
      feedback: "Send Feedback",
    },
  },
  zh: {
    security: {
      meta: {
        title: "安全警告：仿冒 FluxDown 网站 - FluxDown",
        description:
          "仿冒网站 fluxdown.com.cn 冒充 FluxDown 传播「银狐」远控木马。了解哪些域名是官方的，以及从仿冒网站下载后应该怎么做。",
      },
      eyebrow: "安全公告",
      title: "安全<em>警告</em>",
      lede: "我们发现有非官方网站冒充 FluxDown，传播携带「银狐」远控木马的恶意安装包。请勿在该网站下载或运行任何文件，谨防资金与隐私损失。",
      fakeLabel: "仿冒网站（请勿访问）",
      fakeSite: "fluxdown.com.cn",
      officialLabel: "官方网址（认准以下域名）",
      primary: "主域名",
      adviceTitle: "如果你从仿冒网站下载过",
      advice: [
        "立即删除安装包，不要运行。",
        "如果已经运行过，请先断网，并使用可靠的杀毒软件全盘查杀——「银狐」是远程控制木马。",
        "在干净的设备上修改重要账号密码，并检查支付、网银账户有无异常。",
      ],
      verifyTitle: "如何核实下载来源",
      verify: [
        "检查地址栏：域名必须与上方列出的官方网址完全一致。带有额外后缀的相似域名（例如 .com.cn）都不是我们的。",
        "发布文件在 GitHub 的 zerx-lab/FluxDown 仓库发布。任何其他 GitHub 账号或镜像都是非官方的。",
        "官方渠道提供的 Windows 安装包带有代码签名。右键文件 →「属性」→「数字签名」，确认存在有效签名。",
      ],
      signingLink: "代码签名政策",
      downloadTitle: "官方下载渠道",
      downloadNote:
        "请只从上方官方网站或 GitHub Releases 下载 FluxDown，我们从不通过第三方下载站分发安装包。",
      downloadCta: "去官网下载",
    },
    notFound: {
      title: "页面未找到",
      desc: "你访问的页面不存在或已被移动。",
      readout: "segment 05 · http 404 · not found",
      home: "返回首页",
      docs: "文档",
      download: "下载",
      feedback: "发送反馈",
    },
  },
});
