import type { Plugin } from "vite";
import { defineConfig } from "wxt";

/**
 * 内联 CSS 虚拟模块的 id：`\0` 前缀让其他插件跳过它；`.js` 后缀不可省，以 `.css` 结尾的 id
 * 会被 Vite 的 CSS 插件当成样式表处理。
 */
const CONTENT_CSS_PREFIX = "\0fluxdown-content-css:";
const CONTENT_CSS_SUFFIX = ".js";

/**
 * 只用于 `cssInjectionMode: "manual"` 的内容脚本构建：把依赖图里的每个 `import "x.css"` 换成
 * 「`x.css?inline` 字符串 + 登记到 ui/content-css.ts」的虚拟模块，产物不再有 content-scripts/*.css，
 * UI 经 `createShadowRootUi(ctx, { css: contentCss() })` 注入，运行期零 fetch（原因见 ui/content-css.ts）。
 * 组件仍照常 `import "./x.css"`，扩展页面构建不受影响。
 */
function inlineContentCss(): Plugin {
  return {
    name: "fluxdown:inline-content-css",
    enforce: "pre",
    async resolveId(source, importer, options) {
      if (!importer || !source.endsWith(".css")) return null;
      if (importer.startsWith(CONTENT_CSS_PREFIX) || /\.css(?:$|\?)/.test(importer)) return null;
      const resolved = await this.resolve(source, importer, { ...options, skipSelf: true });
      if (!resolved || resolved.external) return null;
      return CONTENT_CSS_PREFIX + resolved.id + CONTENT_CSS_SUFFIX;
    },
    load(id) {
      if (!id.startsWith(CONTENT_CSS_PREFIX) || !id.endsWith(CONTENT_CSS_SUFFIX)) return null;
      const file = id.slice(CONTENT_CSS_PREFIX.length, -CONTENT_CSS_SUFFIX.length).replaceAll("\\", "/");
      return [
        `import css from ${JSON.stringify(`${file}?inline`)};`,
        `import { registerContentCss } from "@/ui/content-css";`,
        "registerContentCss(css);",
      ].join("\n");
    },
  };
}

export default defineConfig({
  // 关闭 unimport 自动导入：utils/ 下 download-dispatch 与 native-messaging
  // 曾因同名导出触发 "Duplicated imports" 警告；全项目已改为显式导入
  // （browser 来自 wxt/browser，define* 来自 wxt/utils/*），不再需要魔法全局。
  imports: false,
  // 两个浏览器统一产出 MV3（输出目录 .output/{chrome,firefox}-mv3[-dev]）；
  // Firefox 的 background 为非持久 event page（manifest 的 background.scripts），不是 service worker。
  manifestVersion: 3,
  // UI 使用 Preact：esbuild 的 automatic JSX 运行时指向 preact/jsx-runtime。
  vite: () => ({
    esbuild: { jsx: "automatic", jsxImportSource: "preact" },
  }),
  zip: {
    excludeSources: ["*.zip", "*.html", "stats.html"],
  },
  // ⚠️  Native Messaging 开发说明：
  //
  // WXT dev 模式（npm run dev）在 Chrome 126+ 上通过 Extensions.loadUnpacked (CDP) +
  // --enable-unsafe-extension-debugging 加载扩展。Chrome 在此模式下会调起 NMH 进程但
  // 立即关闭 stdin，导致 connectNative() 始终失败，popup 显示"未连接"。
  //
  // 测试 native messaging 的正确方式：
  //   1. 运行 `npm run dev` 构建扩展到 .output/chrome-mv3-dev/
  //   2. 打开正式 Chrome → 扩展管理页 → 开启开发者模式
  //   3. "加载已解压的扩展" → 选择 fluxDown/.output/chrome-mv3-dev/
  //   4. 启动 FluxDown GPUI 桌面客户端并启用浏览器扩展连接
  //   5. 在正式 Chrome 里测试扩展连接状态
  //
  // WXT dev Chrome 仍可用于调试 UI / 下载拦截逻辑，只是连接状态始终显示"未连接"属正常。
  manifest: ({ browser, mode }) => ({
    name: "__MSG_extensionName__",
    description: "__MSG_extensionDescription__",
    default_locale: "en",
    // 版本号策略：真实版本号仅由 CI（GitHub Actions，release.yml 用 tag 重写
    // package.json 的 version）产出；任何本地构建（dev 或 build）一律固定为
    // 0.0.0，并以 version_name="dev" 在 UI 上显示，防止本地产物冒充正式版本。
    ...(process.env.GITHUB_ACTIONS === "true"
      ? {}
      : { version: "0.0.0", version_name: "dev" }),
    // Stable key to pin Chrome extension ID across all builds (Chrome only).
    // Firefox 通过 browser_specific_settings.gecko.id 固定 ID。
    // Edge 不支持 key 字段（加载时会报错），且 Edge 侧载扩展 ID
    // 由 crx 签名或加载路径决定，无法通过 key 固定。
    // Chrome Web Store 会忽略 manifest 中的 key 字段，不影响上传。
    // 侧载（从 GitHub Release 下载 zip 手动加载）时，若缺少 key，Chrome 会
    // 根据加载路径生成随机 ID，导致与 NMH manifest 中硬编码的 allowed_origins
    // 不匹配，connectNative() 被拒绝 → 插件无法连接桌面应用。
    // Corresponding Chrome extension ID: meleenglfggcmcajknpeeeiobnpfmahc
    ...(browser === "chrome"
      ? {
          key: "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuf6dyYDofdb37oWv25Rks/FLPA03UonRHvfgCw0KVtMJFUKSTyYbHJ3KWx8j/j8CZBKsPG+U75KEEeV7DTgxb0OUQDY93RzqdcIZlaLQaOxoFgmLI4I0dwjY7pIZs2lxkibqxHOZFZMwH3IMfIp0+u6CmumUPAtd40KaK9oTt0yIruWX6JaoSHJeNAGJ2SAPUl9WSAvB/VuGyL2JDeoT1Li4EZsYlCeaf1d3DHCt3Ye10kKt8a7Pv9iSOkgJlKSDQ24qRcHnch5Xe1IZfJYtAaeH8jYq5HdARFUcYnPgJ9gJEWUglQ2ADXywGyQF9gkOcDKmQJFukjqVDsQGpHbZcwIDAQAB",
        }
      : {}),
    permissions: [
      "downloads",
      // setShelfEnabled 隐藏下载栏；Firefox 没有该 API，声明会被判为无效权限。
      ...(browser === "firefox" ? [] : ["downloads.shelf"]),
      "cookies",
      "webRequest",
      // Firefox MV3 仍支持 blocking webRequest：用 onHeadersReceived {cancel:true}
      // 从源头拦截下载，避免残留"已取消"记录。Chrome MV3 已弃用，故仅 Firefox 加。
      ...(browser === "firefox" ? ["webRequestBlocking"] : []),
      // 扩展安装 / 更新后给已打开的页面补注入 sensor 与 MAIN 钩子（background/probe.ts）。
      "scripting",
      "storage",
      "alarms",
      "notifications",
      "activeTab",
      "tabs",
      "nativeMessaging",
      "contextMenus",
    ],
    host_permissions: ["<all_urls>"],
    // 插件页面脚本经 userScripts API 注册；该权限只能在用户手势内按需申请（Firefox 仅支持可选）。
    optional_permissions: ["userScripts"],
    action: {
      default_icon: {
        16: "/icon/16.png",
        32: "/icon/32.png",
        48: "/icon/48.png",
        128: "/icon/128.png",
      },
    },
    icons: {
      16: "/icon/16.png",
      32: "/icon/32.png",
      48: "/icon/48.png",
      128: "/icon/128.png",
    },
    browser_specific_settings: {
      gecko: {
        id: "fluxdown@fluxdown.app",
        strict_min_version: "140.0",
        data_collection_permissions: {
          required: ["none"],
        },
      },
    },
    // Firefox MV3 默认扩展页 CSP 为 "script-src 'self'; upgrade-insecure-requests;"，会把
    // background / 选项页对用户自部署远程服务器（如 http://192.168.1.10:17800）的请求强制升级为
    // https 而失败（#1025）。显式声明同等 script-src、去掉 upgrade-insecure-requests；
    // Chromium 默认 CSP 无此指令，不改。
    ...(browser === "firefox"
      ? { content_security_policy: { extension_pages: "script-src 'self';" } }
      : {}),
    // 描述经 _locales 跟随浏览器语言；浏览器快捷键页已按扩展分组，无需再加扩展名前缀。
    // 打开侧边栏：Firefox 用内置 `_execute_sidebar_action`（浏览器直接切换 sidebar_action）；
    // Chromium 无内置命令，以自定义命令在 background 中调用 sidePanel.open。
    commands: {
      "toggle-intercept": {
        suggested_key: {
          default: "Alt+Shift+D",
        },
        description: "__MSG_commandToggleIntercept__",
      },
      "download-playing-media": {
        suggested_key: {
          default: "Alt+Shift+V",
        },
        description: "__MSG_commandDownloadPlayingMedia__",
      },
      [browser === "firefox" ? "_execute_sidebar_action" : "open-side-panel"]: {
        suggested_key: {
          default: "Alt+Shift+S",
        },
        description: "__MSG_commandOpenSidePanel__",
      },
    },
  }),
  // WXT bug: addDevModeCsp() 只给 script-src 加了 localhost，未加 style-src，
  // 导致 Firefox 严格执行默认 style-src 'self' 拦截 dev server 的 CSS 加载。
  // MV3 清单的 content_security_policy 是对象格式（extension_pages / sandbox），
  // 此 hook 在 addDevModeCsp() 之后执行，给 extension_pages 补充 style-src 白名单。
  hooks: {
    "build:manifestGenerated"(wxt, manifest) {
      if (wxt.config.command !== "serve") return;
      const csp = manifest.content_security_policy;
      if (typeof csp === "object" && csp.extension_pages) {
        const origin = wxt.server?.origin ?? "http://localhost:3000";
        // 追加 style-src 允许从 dev server 加载 CSS
        csp.extension_pages += `; style-src 'self' ${origin} 'unsafe-inline'`;
      }
    },
    // 内容脚本逐个单独构建：cssInjectionMode "manual" 的（panel / sensor）把 CSS 内联进脚本，见 inlineContentCss。
    "vite:build:extendConfig"(entrypoints, viteConfig) {
      const inline = entrypoints.some((e) => e.type === "content-script" && e.options.cssInjectionMode === "manual");
      if (inline) viteConfig.plugins = [...(viteConfig.plugins ?? []), inlineContentCss()];
    },
  },
});
