/**
 * 下载页客户端逻辑(由 download.astro 的 <script> 引入,仅在存在 [data-dl-root] 时运行)。
 *
 * - 拉取 /api/release(?channel=frontier),主链接走 /api/download(大陆访客 dl-cn 镜像,其余 GitHub),提供 GitHub 备用直链;
 *   大陆浏览器拿镜像前先过 ESA AI 验证码(download-captcha.ts);
 *   失败时保留服务端渲染的 GitHub Releases 回退链接并显示错误条。
 * - 识别访客 OS / CPU 架构,决定首屏主按钮;`D` 键在本页直接触发主按钮。
 * - 复制按钮、邮件订阅表单。
 */
import { download } from "@/i18n/messages/download";
import { DEFAULT_LANG, INTL_LOCALE, isLang } from "@/i18n/config";
import { formatSize, pickPath, versionAnchor, type ReleaseAsset, type ReleaseInfo } from "@/lib/release-format";
import { withBase } from "@/lib/base";
import { assetDownloadUrl } from "@/lib/download-source";
import { installDownloadCaptcha } from "@/lib/download-captcha";
import { TESTFLIGHT_URL } from "@/lib/site-nav";

type Channel = "stable" | "frontier";
type Os = "windows" | "macos" | "linux" | "android" | "ios";
interface Platform {
  os: Os | null;
  /** true = ARM,false = x86,null = 无法判断 */
  arm: boolean | null;
}
interface Pick {
  asset: ReleaseAsset | null;
  label: string;
  alt: { asset: ReleaseAsset; label: string } | null;
  version: string | null;
  tag: string | null;
  date: string | null;
}

const root = document.querySelector<HTMLElement>("[data-dl-root]");
if (root) init(root);

async function detectPlatform(): Promise<Platform> {
  const ua = navigator.userAgent;
  const uaData = (navigator as Navigator & {
    userAgentData?: { platform?: string; getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }> };
  }).userAgentData;
  const hint = `${uaData?.platform ?? ""} ${ua}`;
  let os: Os | null = null;
  if (/android/i.test(hint)) os = "android";
  else if (/iphone|ipad|ipod/i.test(hint)) os = "ios";
  else if (/win/i.test(hint)) os = "windows";
  else if (/mac/i.test(hint)) os = navigator.maxTouchPoints > 1 ? "ios" : "macos";
  else if (/linux|x11|cros/i.test(hint)) os = "linux";

  let arm: boolean | null = /arm64|aarch64/i.test(ua) ? true : null;
  try {
    const values = await uaData?.getHighEntropyValues?.(["architecture"]);
    if (values?.architecture) arm = values.architecture === "arm";
  } catch {
    /* 高熵提示不可用:保持 UA 推断 */
  }
  return { os, arm };
}

function init(root: HTMLElement) {
  const lang = isLang(root.dataset.lang) ? root.dataset.lang : DEFAULT_LANG;
  const t = download[lang];
  const changelogHref = root.dataset.changelog ?? withBase("/changelog/");
  const githubUrl = root.dataset.github ?? "";
  const dateFormat = new Intl.DateTimeFormat(INTL_LOCALE[lang], {
    year: "numeric",
    month: "short",
    day: "numeric",
  });

  const cache = new Map<Channel, Promise<ReleaseInfo>>();
  let channel: Channel = "stable";
  let platform: Platform = { os: null, arm: null };
  let detected = false;
  let release: ReleaseInfo | null = null;
  let requestId = 0;

  const $ = <T extends Element = HTMLElement>(sel: string) => root.querySelector<T>(sel);
  const $$ = <T extends Element = HTMLElement>(sel: string) => Array.from(root.querySelectorAll<T>(sel));

  $$("[data-js]").forEach((el) => (el.hidden = false));
  // 大陆镜像验证码只对简体中文站启用中文界面(与 dl-cn 同一受众),其余语言一律英文
  installDownloadCaptcha(lang === "zh" ? "zh" : "en");
  const status = $("[data-hero-status]");
  if (status) status.textContent = t.hero.detecting;

  /* ── Release data ── */
  function fetchRelease(ch: Channel): Promise<ReleaseInfo> {
    let pending = cache.get(ch);
    if (!pending) {
      pending = fetch(withBase(ch === "frontier" ? "/api/release?channel=frontier" : "/api/release")).then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json() as Promise<ReleaseInfo>;
      });
      pending.catch(() => cache.delete(ch));
      cache.set(ch, pending);
    }
    return pending;
  }

  async function load() {
    const id = ++requestId;
    root.dataset.state = "loading";
    $("[data-error]")!.hidden = true;
    try {
      const data = await fetchRelease(channel);
      if (id !== requestId) return;
      release = data;
      applyAssets(data);
      root.dataset.state = "ready";
    } catch {
      if (id !== requestId) return;
      release = null;
      resetAssets();
      root.dataset.state = "error";
      $("[data-error]")!.hidden = false;
    }
    renderHero();
  }

  function applyAssets(data: ReleaseInfo) {
    for (const row of $$<HTMLAnchorElement>("[data-asset]")) {
      const asset = pickPath(data, row.dataset.asset!) as ReleaseAsset | null | undefined;
      row.hidden = !asset;
      let backup = row.nextElementSibling as HTMLAnchorElement | null;
      if (!backup?.hasAttribute("data-asset-backup")) {
        backup = document.createElement("a");
        backup.dataset.assetBackup = "";
        backup.className = "link self-start text-[12px]";
        backup.textContent = t.source.backup;
        backup.target = "_blank";
        backup.rel = "noopener nofollow";
        row.after(backup);
      }
      backup.hidden = !asset;
      if (!asset) {
        backup.removeAttribute("href");
        continue;
      }
      backup.href = assetDownloadUrl(asset, "github");
      backup.title = `${t.source.backup}: ${asset.name}`;
      row.href = assetDownloadUrl(asset, "auto");
      row.target = "_blank";
      row.rel = "noopener nofollow";
      row.title = asset.name;
      row.querySelector("[data-size]")!.textContent = formatSize(asset.size);
    }
    for (const el of $$("[data-version]")) {
      const version = pickPath(data, el.dataset.version!);
      el.hidden = typeof version !== "string";
      el.textContent = typeof version === "string" ? `v${version}` : "";
    }
    for (const group of $$("[data-group]")) {
      const empty = group.querySelector<HTMLElement>("[data-empty]");
      const rows = group.querySelectorAll<HTMLElement>("[data-asset]");
      if (empty) empty.hidden = Array.from(rows).some((row) => !row.hidden);
    }
    renderTotal(data.total_downloads);
  }

  function resetAssets() {
    $$<HTMLAnchorElement>("[data-asset-backup]").forEach((el) => {
      el.hidden = true;
      el.removeAttribute("href");
    });
    for (const row of $$<HTMLAnchorElement>("[data-asset]")) {
      row.hidden = false;
      row.href = row.dataset.fallback ?? row.href;
      row.removeAttribute("title");
      row.querySelector("[data-size]")!.textContent = "";
    }
    $$("[data-version]").forEach((el) => (el.hidden = true));
    $$("[data-empty]").forEach((el) => (el.hidden = true));
  }

  function renderTotal(total: number) {
    const wrap = $("[data-total-wrap]");
    const slot = $("[data-total]");
    if (!wrap || !slot || !(total > 0)) return;
    // 新插入带 data-count 的节点,由 motion.ts 的 MutationObserver 接管计数动画
    const span = document.createElement("span");
    span.dataset.count = String(total);
    span.textContent = new Intl.NumberFormat(INTL_LOCALE[lang]).format(total);
    slot.replaceChildren(span);
    wrap.hidden = false;
  }

  /* ── Hero ── */
  function pickFor(data: ReleaseInfo | null): Pick {
    const a = data?.assets;
    const { os, arm } = platform;
    const base = {
      version: data?.version ?? null,
      tag: data?.tag ?? null,
      date: data?.published_at ?? null,
    };
    const label = (name: string, suffix?: string) => t.hero.downloadFor(suffix ? `${name} (${suffix})` : name);
    const alt = (asset: ReleaseAsset | null | undefined, text: string) => (asset ? { asset, label: text } : null);
    switch (os) {
      case "windows": {
        const useArm = arm === true && !!a?.setup_arm64;
        return {
          ...base,
          asset: (useArm ? a?.setup_arm64 : a?.setup ?? a?.portable) ?? null,
          label: label(t.os.windows, useArm ? "ARM64" : "x64"),
          alt: useArm ? alt(a?.setup, "x64") : alt(a?.setup_arm64, "ARM64"),
        };
      }
      case "macos": {
        const silicon = arm !== false;
        const primary = silicon
          ? (a?.macos_dmg_arm64 ?? a?.macos_tarball_arm64)
          : (a?.macos_dmg_x64 ?? a?.macos_tarball_x64);
        const other = silicon ? a?.macos_dmg_x64 : a?.macos_dmg_arm64;
        return {
          ...base,
          asset: primary ?? null,
          label: label(t.os.macos, silicon ? t.arch.appleSilicon : t.arch.intel),
          alt: alt(other, silicon ? t.arch.intel : t.arch.appleSilicon),
        };
      }
      case "linux":
        return {
          ...base,
          asset: a?.linux_appimage ?? a?.linux_deb ?? a?.linux_tarball ?? null,
          label: label(t.os.linux, a?.linux_appimage ? "AppImage" : undefined),
          alt: a?.linux_appimage ? alt(a?.linux_deb, ".deb") : null,
        };
      case "android": {
        const m = data?.mobile;
        return {
          asset: m?.assets.android_arm64 ?? m?.assets.android_universal ?? null,
          label: label(t.os.android),
          alt: m?.assets.android_arm64 ? alt(m.assets.android_universal, t.android.universal) : null,
          version: m?.version ?? null,
          tag: m?.tag ?? null,
          date: null,
        };
      }
      case "ios":
        // 主按钮恒为 TestFlight 公测(renderHero 的 iOS 分支);未签名侧载 IPA 作为备选下载。
        return { ...base, asset: null, label: t.ios.join, alt: alt(data?.ios?.assets.sideload_ipa, t.ios.sideloadAlt) };
      default:
        return { ...base, asset: null, label: t.hero.allPlatforms, alt: null };
    }
  }

  function renderHero() {
    const { os } = platform;
    $$("[data-os-icon]").forEach((el) => (el.hidden = el.dataset.osIcon !== (os ?? "none")));
    for (const group of $$("[data-group][data-os]")) {
      const current = group.dataset.os === os;
      group.toggleAttribute("data-current", current);
      const chip = group.querySelector<HTMLElement>("[data-yours]");
      if (chip) chip.hidden = !current;
    }

    const pick = pickFor(release);
    const primary = $<HTMLAnchorElement>("[data-primary]")!;
    const primaryLabel = $("[data-primary-label]")!;
    const altLink = $<HTMLAnchorElement>("[data-alt]")!;
    const osName = os ? t.os[os] : null;

    if (status) status.textContent = !detected ? t.hero.detecting : osName && pick.asset ? pick.label : t.hero.choose;
    const primaryBackup = $<HTMLAnchorElement>("[data-primary-backup]")!;
    primaryBackup.hidden = !pick.asset;
    if (pick.asset) {
      primary.href = assetDownloadUrl(pick.asset, "auto");
      primary.target = "_blank";
      primary.rel = "noopener nofollow";
      primaryBackup.href = assetDownloadUrl(pick.asset, "github");
      primary.title = pick.asset.name;
      primaryLabel.textContent = pick.label;
    } else if (os === "ios") {
      primary.href = TESTFLIGHT_URL;
      primary.target = "_blank";
      primary.rel = "noopener";
      primaryBackup.removeAttribute("href");
      primary.removeAttribute("title");
      primaryLabel.textContent = t.ios.join;
    } else {
      primary.href = os === "android" ? "#android" : "#desktop";
      primary.removeAttribute("target");
      primaryBackup.removeAttribute("href");
      primary.removeAttribute("title");
      primaryLabel.textContent =
        osName && root.dataset.state !== "loading" && release ? t.hero.noBuild(osName) : t.hero.allPlatforms;
    }
    const altBackup = $<HTMLAnchorElement>("[data-alt-backup]")!;
    altLink.hidden = !pick.alt;
    altBackup.hidden = !pick.alt;
    if (pick.alt) {
      altLink.href = assetDownloadUrl(pick.alt.asset, "auto");
      altLink.target = "_blank";
      altLink.rel = "noopener nofollow";
      altLink.textContent = t.hero.alt(pick.alt.label);
      altBackup.href = assetDownloadUrl(pick.alt.asset, "github");
      altBackup.textContent = `${t.source.backup} · ${pick.alt.label}`;
    } else {
      altLink.removeAttribute("href");
      altBackup.removeAttribute("href");
    }

    const showAsset = pick.asset ?? null;
    $("[data-hero-version]")!.textContent = pick.version ? `v${pick.version}` : "—";
    $("[data-hero-size]")!.textContent = showAsset ? formatSize(showAsset.size) : "—";
    $("[data-hero-date]")!.textContent = pick.date ? dateFormat.format(new Date(pick.date)) : "—";
    const notes = $<HTMLAnchorElement>("[data-hero-notes]")!;
    notes.href = pick.version ? `${changelogHref}#${versionAnchor(pick.version)}` : changelogHref;
    const gh = $<HTMLAnchorElement>("[data-hero-github]")!;
    gh.href = pick.tag ? `${githubUrl}/releases/tag/${encodeURIComponent(pick.tag)}` : `${githubUrl}/releases/latest`;
  }

  /* ── Channel ── */
  const hint = $("[data-preview-hint]");
  for (const button of $$<HTMLButtonElement>("[data-channel]")) {
    button.addEventListener("click", () => {
      const next = button.dataset.channel as Channel;
      if (next === channel) return;
      channel = next;
      $$("[data-channel]").forEach((b) => b.setAttribute("aria-pressed", String(b === button)));
      if (hint) hint.hidden = channel !== "frontier";
      void load();
    });
  }
  $("[data-retry]")?.addEventListener("click", () => void load());

  /* ── `D` → 主按钮(捕获阶段拦截全站快捷键,避免跳回本页) ── */
  window.addEventListener(
    "keydown",
    (event) => {
      if (event.key.toLowerCase() !== "d" || event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target?.tagName ?? "")) return;
      event.preventDefault();
      event.stopPropagation();
      $<HTMLAnchorElement>("[data-primary]")?.click();
    },
    { capture: true },
  );

  /* ── Copy ── */
  for (const button of $$<HTMLButtonElement>("[data-copy]")) {
    button.addEventListener("click", async () => {
      const text = document.getElementById(button.dataset.copy!)?.textContent ?? "";
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        return;
      }
      const label = button.querySelector("[data-copy-label]");
      if (label) label.textContent = t.copied;
      button.toggleAttribute("data-copied", true);
      window.setTimeout(() => {
        if (label) label.textContent = t.copy;
        button.toggleAttribute("data-copied", false);
      }, 2000);
    });
  }

  /* ── Boot ── */
  void load();
  void detectPlatform().then((result) => {
    platform = result;
    detected = true;
    renderHero();
  });
}
