import { defineConfig } from "wxt";

export default defineConfig({
  // 关闭 unimport 自动导入：utils/ 下 download-dispatch 与 native-messaging
  // 曾因同名导出触发 "Duplicated imports" 警告；全项目已改为显式导入
  // （browser 来自 wxt/browser，define* 来自 wxt/utils/*），不再需要魔法全局。
  imports: false,
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
  //   4. 启动 FluxDown App（flutter run -d macos）
  //   5. 在正式 Chrome 里测试扩展连接状态
  //
  // WXT dev Chrome 仍可用于调试 UI / 下载拦截逻辑，只是连接状态始终显示"未连接"属正常。
  manifest: ({ browser, mode }) => ({
    name: "__MSG_extensionName__",
    description: "__MSG_extensionDescription__",
    default_locale: "en",
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
      "downloads.shelf", // setShelfEnabled 隐藏下载栏
      "cookies",
      "webRequest",
      // Firefox MV3 仍支持 blocking webRequest：用 onHeadersReceived {cancel:true}
      // 从源头拦截下载，避免残留"已取消"记录（issue #21）。Chrome MV3 已弃用，故仅 Firefox 加。
      ...(browser === "firefox" ? ["webRequestBlocking"] : []),
      "storage",
      "alarms",
      "notifications",
      "activeTab",
      "tabs",
      "nativeMessaging",
      "contextMenus",
    ],
    host_permissions: ["<all_urls>"],
    web_accessible_resources: [
      {
        resources: ["/fetch-interceptor.js"],
        matches: ["<all_urls>"],
      },
    ],
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
    commands: {
      "toggle-intercept": {
        suggested_key: {
          default: "Alt+Shift+D",
        },
        description:
          browser === "chrome"
            ? "__MSG_extensionName__: Toggle download interception"
            : "Toggle download interception",
      },
    },
  }),
  // WXT bug: addDevModeCsp() 只给 script-src 加了 localhost，未加 style-src，
  // 导致 Firefox 严格执行默认 style-src 'self' 拦截 dev server 的 CSS 加载。
  // 此 hook 在 addDevModeCsp() 之后、convertCspToMv2() 之前执行，
  // 在对象格式的 CSP 上补充 style-src 白名单。
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
  },
});
