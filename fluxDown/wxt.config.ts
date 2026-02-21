import { defineConfig } from "wxt";

export default defineConfig({
  manifest: {
    name: "__MSG_extensionName__",
    description: "__MSG_extensionDescription__",
    default_locale: "en",
    permissions: [
      "downloads",
      "cookies",
      "webRequest",
      "storage",
      "notifications",
      "activeTab",
      "tabs",
      "scripting",
    ],
    host_permissions: ["<all_urls>", "http://127.0.0.1/*", "http://localhost/*"],
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
        // @ts-expect-error AMO requires data_collection_permissions since Nov 2025, WXT types not yet updated
        data_collection_permissions: {
          required: ["none"],
        },
      },
    },
  },
});
