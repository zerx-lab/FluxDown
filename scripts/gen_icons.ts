#!/usr/bin/env bun
/**
 * gen_icons.ts — 从 fluxdown_logo.svg 生成全平台图标
 *
 * 用法:
 *     bun scripts/gen_icons.ts
 *
 * 依赖: sharp (bun 全局已安装)
 *
 * 从 assets/logo/fluxdown_logo.svg 生成以下全部图标:
 *
 *   assets/logo/
 *     fluxdown_logo.png (600×600)
 *     logo.png (600×600)
 *     tray_iconTemplate.png (36×36, macOS 2x 菜单栏模板图标)
 *     tray_iconTemplate@1x.png (18×18, macOS 1x 菜单栏模板图标)
 *     logo_on_dark.png (64×64, 暗色主题侧边栏专用: 蓝色箭头 + 透明背景)
 *
 *   windows/runner/resources/
 *     app_icon.ico (16,32,48,64,256 多分辨率 ICO)
 *     tray_win_dark.ico (16,32 — 深色模式白色箭头托盘图标)
 *     tray_win_light.ico (16,32 — 浅色模式深蓝色箭头托盘图标)
 *
 *   macos/Runner/Assets.xcassets/AppIcon.appiconset/
 *     app_icon_{16,32,64,128,256,512,1024}.png
 *
 *   ios/Runner/Assets.xcassets/AppIcon.appiconset/
 *     Icon-App-20x20@{1x,2x,3x}.png  Icon-App-29x29@{1x,2x,3x}.png
 *     Icon-App-40x40@{1x,2x,3x}.png  Icon-App-60x60@{2x,3x}.png
 *     Icon-App-76x76@{1x,2x}.png     Icon-App-83.5x83.5@2x.png
 *     Icon-App-1024x1024@1x.png
 *
 *   android/app/src/main/res/
 *     mipmap-{mdpi,hdpi,xhdpi,xxhdpi,xxxhdpi}/ic_launcher.png
 *
 *   web/
 *     favicon.png (32×32)
 *     icons/Icon-{192,512}.png  icons/Icon-maskable-{192,512}.png
 *
 *   fluxDown/public/icon/
 *     {16,32,48,128}.png  {16,32,48,128}-disabled.png
 *     fluxdown_logo.png (128×128)  fluxdown_logo.svg (副本)
 *
 *   website/public/
 *     favicon.ico  favicon.svg  logo.png (1024×1024)  logo.svg (副本)
 */

import sharp from "sharp";
import {
  existsSync,
  mkdirSync,
  writeFileSync,
  copyFileSync,
  readFileSync,
} from "fs";
import { join, resolve, dirname } from "path";

// ─── 项目根目录 ───────────────────────────────────────────────────
const REPO_ROOT = resolve(
  dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Z]:)/, "$1")),
);
// scripts/ 的父目录即项目根
const ROOT = resolve(REPO_ROOT, "..");

const SVG_SRC = join(ROOT, "assets", "logo", "fluxdown_logo.svg");

if (!existsSync(SVG_SRC)) {
  console.error(`❌ 源文件不存在: ${SVG_SRC}`);
  process.exit(1);
}

// ─── 工具函数 ──────────────────────────────────────────────────────

function ensureDir(filePath: string): void {
  const dir = dirname(filePath);
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true });
  }
}

/** 从 SVG 渲染出指定尺寸的 RGBA PNG Buffer */
async function renderPng(size: number): Promise<Buffer> {
  // 使用高 density 渲染，确保清晰度（SVG viewBox 3508×3508）
  // density=72 → 原始尺寸, 我们用 resize 缩放以获得最佳质量
  return sharp(SVG_SRC, { density: 300 })
    .resize(size, size, {
      kernel: sharp.kernel.lanczos3,
      fit: "contain",
      background: { r: 0, g: 0, b: 0, alpha: 0 },
    })
    .png({ compressionLevel: 9 })
    .toBuffer();
}

/** 生成灰度（disabled）版本的 PNG Buffer */
async function renderDisabledPng(size: number): Promise<Buffer> {
  const src = await renderPng(size);
  // 去色 + 降低对比度 → "禁用" 外观
  return (
    sharp(src)
      .grayscale()
      // 降低整体亮度使其看起来更"禁用"
      .modulate({ brightness: 0.6 })
      .png({ compressionLevel: 9 })
      .toBuffer()
  );
}

/**
 * 生成 macOS 菜单栏模板图标（黑色剪影 + alpha）
 *
 * macOS 模板图标规范:
 *   - 仅使用黑色（或白色），系统根据菜单栏外观自动着色
 *   - 透明区域保持不变
 *   - 非透明像素转为黑色剪影，亮度越高越透明
 *
 * 算法:
 *   1. 计算每个像素的感知亮度 (ITU-R BT.709)
 *   2. 使用平滑阈值映射:
 *      - 亮度 >= 240 → 完全透明（白色背景）
 *      - 亮度 <= 180 → 完全不透明（图标主体）
 *      - 180 < 亮度 < 240 → 线性插值（抗锯齿边缘）
 *   3. RGB 全部设为 0（黑色），仅通过 alpha 表达形状
 *   4. 最终 alpha = min(原始 alpha, 计算的 alpha)，保留原有透明度
 */
async function renderTrayTemplate(size: number): Promise<Buffer> {
  const src = await renderPng(size);
  const { data, info } = await sharp(src)
    .raw()
    .toBuffer({ resolveWithObject: true });

  const LO = 180; // 亮度 <= LO → 完全不透明
  const HI = 240; // 亮度 >= HI → 完全透明
  const RANGE = HI - LO;

  for (let i = 0; i < data.length; i += 4) {
    const r = data[i];
    const g = data[i + 1];
    const b = data[i + 2];
    const origAlpha = data[i + 3];

    // 感知亮度 (BT.709 权重)
    const lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    // 平滑阈值: 暗→不透明, 亮→透明
    let shapeAlpha: number;
    if (lum >= HI) {
      shapeAlpha = 0;
    } else if (lum <= LO) {
      shapeAlpha = 255;
    } else {
      shapeAlpha = Math.round(255 * (1 - (lum - LO) / RANGE));
    }

    data[i] = 0; // R → 黑色
    data[i + 1] = 0; // G → 黑色
    data[i + 2] = 0; // B → 黑色
    // 取原始 alpha 和计算 alpha 的较小值，保留原有透明区域
    data[i + 3] = Math.min(origAlpha, shapeAlpha);
  }

  return sharp(data, {
    raw: { width: info.width, height: info.height, channels: 4 },
  })
    .png({ compressionLevel: 9 })
    .toBuffer();
}

/**
 * 生成 Windows 系统托盘专用箭头图标（透明背景，纯色箭头）
 *
 * 与应用图标不同，托盘图标不带圆角矩形背景，仅保留下载箭头形状。
 * - 深色模式（深色任务栏）→ 白色箭头 (#FFFFFF)
 * - 浅色模式（浅色任务栏）→ 深蓝色箭头 (#1e3a8a)
 *
 * viewBox "106 105 300 300" 以 300×300 正方形裁剪原始箭头路径：
 *   箭头 x:[149,363] y:[119,390]，中心 (256,255)，各边留约 15% 边距
 */
async function renderWindowsTrayArrow(
  size: number,
  color: string,
): Promise<Buffer> {
  const svg = `<svg width="512" height="512" viewBox="106 105 300 300" xmlns="http://www.w3.org/2000/svg">
    <path d="
      M 226 131
      Q 226 119 238 119
      L 274 119
      Q 286 119 286 131
      L 286 296
      L 331 251
      Q 340 242 349 251
      L 363 265
      Q 372 274 363 283
      L 265 381
      Q 256 390 247 381
      L 149 283
      Q 140 274 149 265
      L 163 251
      Q 172 242 181 251
      L 226 296
      Z
    " fill="${color}"/>
  </svg>`;
  return sharp(Buffer.from(svg), { density: 300 })
    .resize(size, size, {
      kernel: sharp.kernel.lanczos3,
      fit: "contain",
      background: { r: 0, g: 0, b: 0, alpha: 0 },
    })
    .png({ compressionLevel: 9 })
    .toBuffer();
}

/**
 * 手动构建 ICO 文件（多分辨率，PNG 压缩帧）
 *
 * ICO 布局:
 *   [6B  文件头]        reserved=0, type=1, count=N
 *   [16B × N 目录条目]  width, height, colorCount, reserved, planes, bpp, size, offset
 *   [各帧 PNG 数据]
 */
function buildIco(frames: { size: number; data: Buffer }[]): Buffer {
  const n = frames.length;

  // 文件头: 6 bytes
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type = ICO
  header.writeUInt16LE(n, 4); // count

  // 目录条目: 16 bytes each
  const directory = Buffer.alloc(16 * n);
  let dataOffset = 6 + 16 * n;

  for (let i = 0; i < n; i++) {
    const { size, data } = frames[i];
    const offset = i * 16;

    // width/height: 0 代表 256（ICO 规范，单字节无法存 256）
    directory.writeUInt8(size < 256 ? size : 0, offset); // width
    directory.writeUInt8(size < 256 ? size : 0, offset + 1); // height
    directory.writeUInt8(0, offset + 2); // color count (0 = true color)
    directory.writeUInt8(0, offset + 3); // reserved
    directory.writeUInt16LE(1, offset + 4); // planes
    directory.writeUInt16LE(32, offset + 6); // bits per pixel (RGBA)
    directory.writeUInt32LE(data.length, offset + 8); // data size
    directory.writeUInt32LE(dataOffset, offset + 12); // data offset

    dataOffset += data.length;
  }

  return Buffer.concat([header, directory, ...frames.map((f) => f.data)]);
}

/** 保存 Buffer 到文件，打印路径 */
async function saveFile(relPath: string, data: Buffer | string): Promise<void> {
  const absPath = join(ROOT, relPath);
  ensureDir(absPath);
  writeFileSync(absPath, data);
  const size = Buffer.isBuffer(data) ? data.length : Buffer.byteLength(data);
  const sizeStr = size > 1024 ? `${(size / 1024).toFixed(1)} KB` : `${size} B`;
  console.log(`  ✓ ${relPath} (${sizeStr})`);
}

/** 渲染并保存指定尺寸 PNG */
async function savePng(relPath: string, size: number): Promise<void> {
  const buf = await renderPng(size);
  await saveFile(relPath, buf);
}

// ─── 预缓存常用尺寸 ──────────────────────────────────────────────
// 同一尺寸可能被多处使用，缓存避免重复渲染
const pngCache = new Map<number, Buffer>();

async function getCachedPng(size: number): Promise<Buffer> {
  if (!pngCache.has(size)) {
    pngCache.set(size, await renderPng(size));
  }
  return pngCache.get(size)!;
}

// ─── 主流程 ────────────────────────────────────────────────────────

async function main() {
  console.log("🦅 FluxDown 全平台图标生成器");
  console.log(`   源文件: ${SVG_SRC}`);
  console.log("");

  let totalCount = 0;

  // ──────────────────────────────────────────
  // 1. assets/logo/ — 源 PNG 和托盘模板图标
  // ──────────────────────────────────────────
  console.log("📁 assets/logo/");
  {
    const logo600 = await getCachedPng(600);
    await saveFile("assets/logo/fluxdown_logo.png", logo600);
    await saveFile("assets/logo/logo.png", logo600);

    // macOS 菜单栏模板图标
    const tray36 = await renderTrayTemplate(36);
    await saveFile("assets/logo/tray_iconTemplate.png", tray36);
    const tray18 = await renderTrayTemplate(18);
    await saveFile("assets/logo/tray_iconTemplate@1x.png", tray18);

    // 暗色主题侧边栏 logo — 蓝色箭头 (#3B82F6) + 透明背景，64px 保证高 DPI 清晰度
    // 不含圆角矩形背景，直接在深色 surface1 上显示
    const logoDark = await renderWindowsTrayArrow(64, "#3B82F6");
    await saveFile("assets/logo/logo_on_dark.png", logoDark);

    totalCount += 5;
  }

  // ──────────────────────────────────────────
  // 2. Windows ICO — 多分辨率
  // ──────────────────────────────────────────
  console.log("\n📁 windows/runner/resources/");
  {
    const icoSizes = [16, 32, 48, 64, 256];
    const frames: { size: number; data: Buffer }[] = [];
    for (const size of icoSizes) {
      const data = await getCachedPng(size);
      frames.push({ size, data });
    }
    const ico = buildIco(frames);
    await saveFile("windows/runner/resources/app_icon.ico", ico);
    console.log(
      `     (包含分辨率: ${icoSizes.map((s) => `${s}×${s}`).join(", ")})`,
    );
    totalCount += 1;
  }

  // ──────────────────────────────────────────
  // 2b. Windows 托盘图标 — 深/浅色模式各一套
  //     仅含箭头形状，透明背景，适配任务栏颜色
  // ──────────────────────────────────────────
  console.log("\n📁 windows/runner/resources/ (tray icons)");
  {
    const traySizes = [16, 32];

    // 深色模式托盘图标（白色箭头，用于深色任务栏）
    const darkFrames: { size: number; data: Buffer }[] = [];
    for (const size of traySizes) {
      darkFrames.push({
        size,
        data: await renderWindowsTrayArrow(size, "#FFFFFF"),
      });
    }
    const trayDarkIco = buildIco(darkFrames);
    await saveFile("windows/runner/resources/tray_win_dark.ico", trayDarkIco);

    // 浅色模式托盘图标（深蓝色箭头，用于浅色任务栏）
    const lightFrames: { size: number; data: Buffer }[] = [];
    for (const size of traySizes) {
      lightFrames.push({
        size,
        data: await renderWindowsTrayArrow(size, "#1e3a8a"),
      });
    }
    const trayLightIco = buildIco(lightFrames);
    await saveFile(
      "windows/runner/resources/tray_win_light.ico",
      trayLightIco,
    );

    console.log(`     (含分辨率: ${traySizes.map((s) => `${s}×${s}`).join(", ")})`);
    totalCount += 2;
  }

  // ──────────────────────────────────────────
  // 3. macOS AppIcon — 7 个尺寸
  // ──────────────────────────────────────────
  console.log("\n📁 macos/Runner/Assets.xcassets/AppIcon.appiconset/");
  {
    const macSizes = [16, 32, 64, 128, 256, 512, 1024];
    for (const size of macSizes) {
      const buf = await getCachedPng(size);
      await saveFile(
        `macos/Runner/Assets.xcassets/AppIcon.appiconset/app_icon_${size}.png`,
        buf,
      );
    }
    totalCount += macSizes.length;
  }

  // ──────────────────────────────────────────
  // 4. iOS AppIcon — 15 个文件
  // ──────────────────────────────────────────
  console.log("\n📁 ios/Runner/Assets.xcassets/AppIcon.appiconset/");
  {
    // { 文件名: 实际像素尺寸 }
    const iosIcons: Record<string, number> = {
      "Icon-App-20x20@1x.png": 20,
      "Icon-App-20x20@2x.png": 40,
      "Icon-App-20x20@3x.png": 60,
      "Icon-App-29x29@1x.png": 29,
      "Icon-App-29x29@2x.png": 58,
      "Icon-App-29x29@3x.png": 87,
      "Icon-App-40x40@1x.png": 40,
      "Icon-App-40x40@2x.png": 80,
      "Icon-App-40x40@3x.png": 120,
      "Icon-App-60x60@2x.png": 120,
      "Icon-App-60x60@3x.png": 180,
      "Icon-App-76x76@1x.png": 76,
      "Icon-App-76x76@2x.png": 152,
      "Icon-App-83.5x83.5@2x.png": 167,
      "Icon-App-1024x1024@1x.png": 1024,
    };
    for (const [filename, pixelSize] of Object.entries(iosIcons)) {
      const buf = await getCachedPng(pixelSize);
      await saveFile(
        `ios/Runner/Assets.xcassets/AppIcon.appiconset/${filename}`,
        buf,
      );
    }
    totalCount += Object.keys(iosIcons).length;
  }

  // ──────────────────────────────────────────
  // 5. Android mipmap — 5 个 DPI 变体
  // ──────────────────────────────────────────
  console.log("\n📁 android/app/src/main/res/");
  {
    const androidIcons: Record<string, number> = {
      "mipmap-mdpi": 48,
      "mipmap-hdpi": 72,
      "mipmap-xhdpi": 96,
      "mipmap-xxhdpi": 144,
      "mipmap-xxxhdpi": 192,
    };
    for (const [folder, size] of Object.entries(androidIcons)) {
      const buf = await getCachedPng(size);
      await saveFile(`android/app/src/main/res/${folder}/ic_launcher.png`, buf);
    }
    totalCount += Object.keys(androidIcons).length;
  }

  // ──────────────────────────────────────────
  // 6. Web — favicon + PWA 图标
  // ──────────────────────────────────────────
  console.log("\n📁 web/");
  {
    const favicon = await getCachedPng(32);
    await saveFile("web/favicon.png", favicon);

    const icon192 = await getCachedPng(192);
    const icon512 = await getCachedPng(512);
    await saveFile("web/icons/Icon-192.png", icon192);
    await saveFile("web/icons/Icon-512.png", icon512);
    await saveFile("web/icons/Icon-maskable-192.png", icon192);
    await saveFile("web/icons/Icon-maskable-512.png", icon512);

    totalCount += 5;
  }

  // ──────────────────────────────────────────
  // 7. 浏览器扩展图标 — 正常 + disabled + logo
  // ──────────────────────────────────────────
  console.log("\n📁 fluxDown/public/icon/");
  {
    const extSizes = [16, 32, 48, 128];

    // 正常图标
    for (const size of extSizes) {
      const buf = await getCachedPng(size);
      await saveFile(`fluxDown/public/icon/${size}.png`, buf);
    }

    // disabled（灰度）图标
    for (const size of extSizes) {
      const buf = await renderDisabledPng(size);
      await saveFile(`fluxDown/public/icon/${size}-disabled.png`, buf);
    }

    // 扩展 logo
    const extLogo = await getCachedPng(128);
    await saveFile("fluxDown/public/icon/fluxdown_logo.png", extLogo);

    // 复制 SVG 到扩展
    const svgContent = readFileSync(SVG_SRC);
    await saveFile("fluxDown/public/icon/fluxdown_logo.svg", svgContent);

    totalCount += extSizes.length * 2 + 2;
  }

  // ──────────────────────────────────────────
  // 8. 官网 — favicon + logo
  // ──────────────────────────────────────────
  console.log("\n📁 website/public/");
  {
    // favicon.ico（多分辨率: 16, 32, 48）
    const faviconSizes = [16, 32, 48];
    const faviconFrames: { size: number; data: Buffer }[] = [];
    for (const size of faviconSizes) {
      faviconFrames.push({ size, data: await getCachedPng(size) });
    }
    const faviconIco = buildIco(faviconFrames);
    await saveFile("website/public/favicon.ico", faviconIco);

    // favicon.svg — 将 1024px PNG 嵌入 SVG（与现有格式一致）
    const logo1024 = await getCachedPng(1024);
    const pngBase64 = logo1024.toString("base64");
    const faviconSvg = [
      `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 512 512" width="512" height="512">`,
      `  <image href="data:image/png;base64,${pngBase64}" width="512" height="512"/>`,
      `</svg>`,
      "",
    ].join("\n");
    await saveFile("website/public/favicon.svg", faviconSvg);

    // logo.png (1024×1024)
    await saveFile("website/public/logo.png", logo1024);

    // logo.svg (复制源 SVG)
    const svgContent = readFileSync(SVG_SRC);
    await saveFile("website/public/logo.svg", svgContent);

    totalCount += 4;
  }

  // ──────────────────────────────────────────
  // 完成
  // ──────────────────────────────────────────
  console.log(`\n✅ 完成！共生成 ${totalCount} 个文件。`);
  console.log("   重新构建各平台应用后图标即可更新。");
}

main().catch((err) => {
  console.error("❌ 生成失败:", err);
  process.exit(1);
});
