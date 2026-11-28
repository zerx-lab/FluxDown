import { defineMessages } from "../define";

/** 「参与贡献」分区文案:文档首页贡献区、页脚开源列、市场页贡献入口。键与 `lib/ecosystem.ts` 的 `ContributeId` 对应。 */
export const contribute = defineMessages("contribute", {
  en: {
    eyebrow: "Contribute",
    title: "Build FluxDown <em>with us</em>",
    lede: "FluxDown is spread across a few public repositories. Pick what you want to share — a plugin, a theme, a sniffing rule, a translation or a docs fix — and each guide walks you through it.",
    overview: "Ways to contribute",
    guide: "Read the guide",
    repo: "Repository",
    areas: {
      plugins: {
        title: "Plugins",
        desc: "Publish resolvers, hooks and views to the plugin market. Your plugin stays in your own repository; register it once in the index.",
      },
      themes: {
        title: "Themes",
        desc: "Share a color scheme in the theme market. Design it in the Theme Builder, then open a pull request with the JSON and screenshots.",
      },
      sniffRules: {
        title: "Sniffing rules",
        desc: "Teach the browser extension a new site. Maintain your own rule repository or submit a one-off rule through an issue form.",
      },
      translations: {
        title: "Translations",
        desc: "Bring the desktop, mobile and Web apps to your language. Partial translations are welcome — missing strings fall back to English.",
      },
      docs: {
        title: "Docs & code",
        desc: "Fix a page, add a guide or send a patch. Every docs page links to its source; pull requests go to the main branch.",
      },
    },
    sdk: "Plugin SDK",
    sdkDesc: "fluxdown-plugin-sdk on npm",
  },
  zh: {
    eyebrow: "参与贡献",
    title: "和我们一起<em>共建</em> FluxDown",
    lede: "FluxDown 分布在几个公开仓库中。插件、主题、嗅探规则、翻译还是文档修订——选一样你想分享的，对应指南会一步步带你完成。",
    overview: "贡献方式总览",
    guide: "阅读指南",
    repo: "仓库",
    areas: {
      plugins: {
        title: "插件",
        desc: "把解析器、钩子与视图发布到插件市场。插件留在你自己的仓库里，只需在索引中登记一次。",
      },
      themes: {
        title: "主题",
        desc: "把配色方案分享到主题市场。用主题编辑器设计，再连同 JSON 与截图提交 Pull Request。",
      },
      sniffRules: {
        title: "嗅探规则",
        desc: "让浏览器扩展认识新的网站。维护自己的规则仓库，或通过 Issue 表单一次性提交。",
      },
      translations: {
        title: "翻译",
        desc: "把桌面端、移动端与 Web 应用带到你的语言。翻译一部分也欢迎——缺失的文案会回退英文。",
      },
      docs: {
        title: "文档与代码",
        desc: "修正一页文档、补一篇指南或提交补丁。每页文档都链接到源文件，Pull Request 提交到 main 分支。",
      },
    },
    sdk: "插件 SDK",
    sdkDesc: "npm 上的 fluxdown-plugin-sdk",
  },
});
