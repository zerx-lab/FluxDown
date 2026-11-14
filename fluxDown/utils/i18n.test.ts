import { afterAll, beforeEach, describe, expect, mock, test } from "bun:test";
import en from "./locales/en";
import ru from "./locales/ru";
import zhCN from "./locales/zh-CN";
import zhTW from "./locales/zh-TW";
import type { MessageKey } from "./locales/zh-CN";

const storage: Record<string, unknown> = {};
let storageUnavailable = false;
mock.module("wxt/browser", () => ({
  browser: {
    storage: {
      local: {
        async get(key: string) {
          if (storageUnavailable) throw new Error("Storage unavailable");
          return { [key]: storage[key] };
        },
        async set(values: Record<string, unknown>) {
          Object.assign(storage, values);
        },
        async remove(key: string) {
          delete storage[key];
        },
      },
    },
  },
}));

// WXT resolves browser APIs when the module loads; install the browser boundary
// mock before importing so initialization can be tested outside an extension.
const {
  initI18n, getLocale, setLocale, saveLocale, clearLocale, getSavedLocale,
  getNextLocale, t,
} = await import("./i18n");

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");
let browserLanguages: string[];
let browserLanguage: string;

beforeEach(() => {
  delete storage.fluxdown_locale;
  storageUnavailable = false;
  browserLanguages = ["en-US"];
  browserLanguage = "en-US";
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {
      get languages() { return browserLanguages; },
      get language() { return browserLanguage; },
    },
  });
  setLocale("en");
});

afterAll(() => {
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

const localeCases = [
  ["zh-TW", "zh-TW"], ["zh-HK", "zh-TW"], ["zh-MO", "zh-TW"],
  ["zh-Hant", "zh-TW"], ["zh-Hant-CN", "zh-TW"],
  ["zh-HK-u-nu-hanidec", "zh-TW"], ["zh-MO-u-ca-chinese", "zh-TW"],
  [" ZH_hANT_tw ", "zh-TW"], ["zh-Hans-TW", "zh-CN"],
  ["zh-Hans-HK", "zh-CN"], ["zh-Hans-MO", "zh-CN"],
  ["zh", "zh-CN"], ["zh-CN", "zh-CN"], ["zh-SG", "zh-CN"],
  ["zh-Hantfoo", "zh-CN"], ["zh-u-rg-twzzzz", "zh-CN"],
  ["en-GB", "en"], ["EN_us", "en"],
  ["ru", "ru"], ["ru-RU", "ru"], ["RU_ru", "ru"],
] as const;

for (const [input, expected] of localeCases) {
  test(`resolves ${input} consistently for manual, saved and browser languages`, async () => {
    setLocale(input);
    expect(getLocale()).toBe(expected);
    const messages = expected === "zh-TW" ? zhTW : expected === "zh-CN" ? zhCN : expected === "ru" ? ru : en;
    expect(t("options.general.languageTitle")).toBe(messages["options.general.languageTitle"]);

    // Old saved aliases must match one of the canonical options values.
    storage.fluxdown_locale = input;
    expect(await getSavedLocale()).toBe(expected);
    expect(await initI18n()).toBe(expected);
    await saveLocale(input);
    expect(storage.fluxdown_locale).toBe(expected);

    delete storage.fluxdown_locale;
    browserLanguages = [input];
    expect(await initI18n()).toBe(expected);
  });
}

describe("language preference and fallback", () => {
  test("saved language overrides the browser until automatic mode is restored", async () => {
    browserLanguages = ["zh-HK"];
    await saveLocale("zh-CN");
    expect(await initI18n()).toBe("zh-CN");
    await clearLocale();
    expect(await getSavedLocale()).toBeNull();
    expect(getLocale()).toBe("zh-TW");
    expect(await initI18n()).toBe("zh-TW");
  });

  test("uses the first supported browser language and falls back to English", async () => {
    browserLanguages = ["fr-FR", "zh-MO", "en-US"];
    expect(await initI18n()).toBe("zh-TW");
    browserLanguages = ["en-US", "zh-Hant"];
    expect(await initI18n()).toBe("en");
    browserLanguages = ["fr-FR"];
    expect(await initI18n()).toBe("en");
    setLocale("fr-FR");
    expect(getLocale()).toBe("en");
  });

  test("uses navigator.language when the language list is empty", async () => {
    browserLanguages = [];
    browserLanguage = "zh-Hant-HK";
    expect(await initI18n()).toBe("zh-TW");
  });

  test("invalid saved values and inaccessible storage follow the browser", async () => {
    browserLanguages = ["zh-TW"];
    for (const value of [undefined, "", "fr-FR", 42, { locale: "en" }]) {
      storage.fluxdown_locale = value;
      expect(await getSavedLocale()).toBeNull();
      expect(await initI18n()).toBe("zh-TW");
    }
    storageUnavailable = true;
    expect(await initI18n()).toBe("zh-TW");
  });

  test("popup cycle preserves distinct simplified and traditional selections", async () => {
    for (const expected of ["zh-CN", "zh-TW", "ru", "en"] as const) {
      const next = getNextLocale();
      expect(next).toBe(expected);
      await saveLocale(next);
      expect(await getSavedLocale()).toBe(expected);
      expect(await initI18n()).toBe(expected);
    }
  });
});


test("Taiwan translations cover the baseline keys and preserve every placeholder", () => {
  const keys = Object.keys(zhCN).sort() as MessageKey[];
  expect(Object.keys(zhTW).sort()).toEqual(keys);
  expect(Object.keys(en).sort()).toEqual(keys);
  for (const key of keys) {
    expect(zhTW[key].trim().length).toBeGreaterThan(0);
    const placeholders = zhCN[key].match(/\{[A-Za-z][A-Za-z0-9_]*\}/g) ?? [];
    expect((zhTW[key].match(/\{[A-Za-z][A-Za-z0-9_]*\}/g) ?? []).sort()).toEqual(placeholders.sort());
    expect((en[key].match(/\{[A-Za-z][A-Za-z0-9_]*\}/g) ?? []).sort()).toEqual(placeholders.sort());
  }
});

test("Russian translations cover the baseline keys and preserve every placeholder", () => {
  const keys = Object.keys(zhCN).sort() as MessageKey[];
  expect(Object.keys(ru).sort()).toEqual(keys);
  for (const key of keys) {
    expect(ru[key].trim().length).toBeGreaterThan(0);
    const placeholders = zhCN[key].match(/\{[A-Za-z][A-Za-z0-9_]*\}/g) ?? [];
    expect((ru[key].match(/\{[A-Za-z][A-Za-z0-9_]*\}/g) ?? []).sort()).toEqual(placeholders.sort());
  }
});
