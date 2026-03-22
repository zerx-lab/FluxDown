import { defineConfig } from "wxt";

export default defineConfig({
  zip: {
    excludeSources: ["*.zip", "*.html", "stats.html"],
  },
  manifest: ({ browser, mode }) => ({
    name: "__MSG_extensionName__",
    description: "__MSG_extensionDescription__",
    default_locale: "en",
    // Stable key to pin extension ID during local development (Chrome/Edge only).
    // Firefox pins its ID via browser_specific_settings.gecko.id instead.
    // 商店上传不允许包含 key 字段，因此仅在开发模式下注入。
    // Corresponding Chrome extension ID: meleenglfggcmcajknpeeeiobnpfmahc
    ...(browser !== "firefox" && mode === "development"
      ? {
          key: "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuf6dyYDofdb37oWv25Rks/FLPA03UonRHvfgCw0KVtMJFUKSTyYbHJ3KWx8j/j8CZBKsPG+U75KEEeV7DTgxb0OUQDY93RzqdcIZlaLQaOxoFgmLI4I0dwjY7pIZs2lxkibqxHOZFZMwH3IMfIp0+u6CmumUPAtd40KaK9oTt0yIruWX6JaoSHJeNAGJ2SAPUl9WSAvB/VuGyL2JDeoT1Li4EZsYlCeaf1d3DHCt3Ye10kKt8a7Pv9iSOkgJlKSDQ24qRcHnch5Xe1IZfJYtAaeH8jYq5HdARFUcYnPgJ9gJEWUglQ2ADXywGyQF9gkOcDKmQJFukjqVDsQGpHbZcwIDAQAB",
        }
      : {}),
    permissions: [
      "downloads",
      "cookies",
      "webRequest",
      "storage",
      "notifications",
      "activeTab",
      "tabs",
      "nativeMessaging",
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
  }),
});
