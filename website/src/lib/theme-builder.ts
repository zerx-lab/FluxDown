export type FluxThemeAppearance = "dark" | "light";

export interface FluxMetricJson {
  radius: {
    progress: number;
    xs: number;
    segmentCell: number;
    sm: number;
    md: number;
    input: number;
    card: number;
    iconTile: number;
    dialog: number;
    fieldMobile: number;
    chipLg: number;
    chipXl: number;
    badge: number;
    pill: number;
    sheet: number;
  };
  stroke: {
    thin: number;
    thick: number;
  };
  spacing: {
    xs: number;
    sm: number;
    md: number;
    lg: number;
    xl: number;
  };
  button: {
    heightSm: number;
    heightMd: number;
    heightLg: number;
  };
  alpha: {
    subtle: number;
    soft: number;
    muted: number;
    mutedStrong: number;
    active: number;
    selectedBorder: number;
    scrim: number;
    border: number;
    borderStrong: number;
    disabled: number;
    glass: number;
    focusRing: number;
    shadowStrong: number;
    shadowSoft: number;
    shadowFaint: number;
    faint: number;
    textSelection: number;
    borderSubtle: number;
    borderFaint: number;
    borderMedium: number;
    emphasis: number;
    glassSubtle: number;
  };
  mobile: {
    pageMargin: number;
    cardRadius: number;
    cardGap: number;
    appBarHeight: number;
    tabsHeight: number;
    dockBottomGap: number;
    fabSize: number;
    scrollBottomPadding: number;
  };
}

export interface FluxThemeJson {
  name: string;
  author?: string;
  appearance: FluxThemeAppearance;
  colors: {
    surface: {
      background: string;
      surface1: string;
      surface2: string;
      surface3: string;
    };
    element: {
      hover: string;
      selected: string;
      active: string;
    };
    text: {
      primary: string;
      secondary: string;
      muted: string;
      disabled: string;
    };
    border: {
      default: string;
      focused: string;
    };
    accent: {
      color: string;
      hover: string;
      background: string;
      foreground: string;
    };
    input: {
      background: string;
      border: string;
      focusBorder: string;
      focusBackground: string;
    };
    dialog: {
      background: string;
      barrier: string;
    };
    switch: {
      track: string;
      thumb: string;
    };
    shadow: string;
    status: {
      success: string;
      warning: string;
      error: string;
    };
    segmentPalette: string[];
  };
  metrics?: FluxMetricJson;
  schemaVersion?: number;
}

export type TokenGroupKey =
  | "surface"
  | "element"
  | "text"
  | "border"
  | "accent"
  | "input"
  | "dialog"
  | "switch"
  | "status"
  | "shadow"
  | "segment"
  | "radius"
  | "spacing"
  | "stroke"
  | "button"
  | "alpha"
  | "mobile";

export type PreviewArea = "downloads" | "settings";

export interface TokenDescriptor {
  path: string;
  label: string;
  groupKey: TokenGroupKey;
  /** "color"（默认，省略即视为 color）| "number"（Layer1 圆角/间距/描边/按钮/透明度/移动几何） */
  kind?: "color" | "number";
  /** kind === "number" 时的单位提示：px（默认）| alpha（0-1 无量纲） */
  unit?: "px" | "alpha";
  min?: number;
  max?: number;
  step?: number;
}

/** 该 token 在预览里被承载的主视图（供「区域徽标」与「聚焦自动切换预览视图」用）。
 * 与实际客户端页面对应：downloads = 主下载窗口；settings = 设置页。
 * 多处出现的 token 取最能体现其效果的主视图。 */
const SETTINGS_METRIC_PATHS = new Set<string>([
  "metrics.radius.progress",
  "metrics.radius.xs",
  "metrics.radius.sm",
  "metrics.radius.chipLg",
  "metrics.radius.chipXl",
  "metrics.radius.pill",
  "metrics.radius.dialog",
  "metrics.stroke.thin",
  "metrics.stroke.thick",
  "metrics.spacing.xs",
  "metrics.spacing.sm",
  "metrics.spacing.md",
  "metrics.spacing.lg",
  "metrics.spacing.xl",
  "metrics.button.heightSm",
  "metrics.button.heightMd",
  "metrics.button.heightLg",
]);

export function tokenArea(path: string): PreviewArea {
  if (SETTINGS_METRIC_PATHS.has(path)) return "settings";
  if (path.startsWith("metrics.alpha.")) {
    return path === "metrics.alpha.shadowStrong" ? "downloads" : "settings";
  }
  if (path.startsWith("colors.switch.")) return "settings";
  if (path.startsWith("colors.dialog.")) return "settings";
  return "downloads";
}

const DEFAULT_SEGMENT_PALETTE = [
  "ff22c55e",
  "fff59e0b",
  "ffa855f7",
  "ff06b6d4",
  "ffec4899",
  "ff14b8a6",
  "ffef4444",
  "ff8b5cf6",
  "fff97316",
  "ff10b981",
  "ffe11d48",
  "ff0ea5e9",
  "ffd946ef",
  "ff84cc16",
  "ff64748b",
  "ff3b82f6",
];

/** 与 Dart `FluxMetricTokens` 默认值逐字段对齐（亮/暗主题共用，不区分明暗）。 */
export const DEFAULT_METRICS: FluxMetricJson = {
  radius: {
    progress: 1.5,
    xs: 2,
    segmentCell: 2.5,
    sm: 4,
    md: 6,
    input: 8,
    card: 8,
    iconTile: 9,
    dialog: 10,
    fieldMobile: 11,
    chipLg: 12,
    chipXl: 14,
    badge: 18,
    pill: 999,
    sheet: 26,
  },
  stroke: {
    thin: 1,
    thick: 1.5,
  },
  spacing: {
    xs: 4,
    sm: 8,
    md: 12,
    lg: 16,
    xl: 24,
  },
  button: {
    heightSm: 28,
    heightMd: 32,
    heightLg: 36,
  },
  alpha: {
    subtle: 0.08,
    soft: 0.1,
    muted: 0.12,
    mutedStrong: 0.14,
    active: 0.18,
    selectedBorder: 0.35,
    scrim: 0.45,
    border: 0.5,
    borderStrong: 0.8,
    disabled: 0.5,
    glass: 0.72,
    focusRing: 0.6,
    shadowStrong: 0.25,
    shadowSoft: 0.16,
    shadowFaint: 0.08,
    faint: 0.06,
    textSelection: 0.25,
    borderSubtle: 0.3,
    borderFaint: 0.4,
    borderMedium: 0.6,
    emphasis: 0.7,
    glassSubtle: 0.55,
  },
  mobile: {
    pageMargin: 16,
    cardRadius: 12,
    cardGap: 10,
    appBarHeight: 56,
    tabsHeight: 44,
    dockBottomGap: 16,
    fabSize: 46,
    scrollBottomPadding: 120,
  },
};

export const TOKEN_GROUPS: ReadonlyArray<{ key: TokenGroupKey; labelKey: string }> = [
  { key: "surface", labelKey: "tb.groups.surface" },
  { key: "element", labelKey: "tb.groups.element" },
  { key: "text", labelKey: "tb.groups.text" },
  { key: "border", labelKey: "tb.groups.border" },
  { key: "accent", labelKey: "tb.groups.accent" },
  { key: "input", labelKey: "tb.groups.input" },
  { key: "dialog", labelKey: "tb.groups.dialog" },
  { key: "switch", labelKey: "tb.groups.switch" },
  { key: "status", labelKey: "tb.groups.status" },
  { key: "shadow", labelKey: "tb.groups.shadow" },
  { key: "segment", labelKey: "tb.groups.segment" },
  { key: "radius", labelKey: "tb.groups.radius" },
  { key: "spacing", labelKey: "tb.groups.spacing" },
  { key: "stroke", labelKey: "tb.groups.stroke" },
  { key: "button", labelKey: "tb.groups.button" },
  { key: "alpha", labelKey: "tb.groups.alpha" },
];

const STATIC_TOKEN_DESCRIPTORS: ReadonlyArray<TokenDescriptor> = [
  { path: "colors.surface.background", label: "Background", groupKey: "surface" },
  { path: "colors.surface.surface1", label: "Surface 1", groupKey: "surface" },
  { path: "colors.surface.surface2", label: "Surface 2", groupKey: "surface" },
  { path: "colors.surface.surface3", label: "Surface 3", groupKey: "surface" },

  { path: "colors.element.hover", label: "Hover", groupKey: "element" },
  { path: "colors.element.selected", label: "Selected", groupKey: "element" },
  { path: "colors.element.active", label: "Active", groupKey: "element" },

  { path: "colors.text.primary", label: "Primary", groupKey: "text" },
  { path: "colors.text.secondary", label: "Secondary", groupKey: "text" },
  { path: "colors.text.muted", label: "Muted", groupKey: "text" },
  { path: "colors.text.disabled", label: "Disabled", groupKey: "text" },

  { path: "colors.border.default", label: "Default", groupKey: "border" },
  { path: "colors.border.focused", label: "Focused", groupKey: "border" },

  { path: "colors.accent.color", label: "Color", groupKey: "accent" },
  { path: "colors.accent.hover", label: "Hover", groupKey: "accent" },
  { path: "colors.accent.background", label: "Background", groupKey: "accent" },
  { path: "colors.accent.foreground", label: "Foreground", groupKey: "accent" },

  { path: "colors.input.background", label: "Background", groupKey: "input" },
  { path: "colors.input.border", label: "Border", groupKey: "input" },
  { path: "colors.input.focusBorder", label: "Focus Border", groupKey: "input" },
  { path: "colors.input.focusBackground", label: "Focus Background", groupKey: "input" },

  { path: "colors.dialog.background", label: "Background", groupKey: "dialog" },
  { path: "colors.dialog.barrier", label: "Barrier", groupKey: "dialog" },

  { path: "colors.switch.track", label: "Track", groupKey: "switch" },
  { path: "colors.switch.thumb", label: "Thumb", groupKey: "switch" },

  { path: "colors.status.success", label: "Success", groupKey: "status" },
  { path: "colors.status.warning", label: "Warning", groupKey: "status" },
  { path: "colors.status.error", label: "Error", groupKey: "status" },

  { path: "colors.shadow", label: "Shadow", groupKey: "shadow" },
];

export const METRIC_TOKEN_DESCRIPTORS: ReadonlyArray<TokenDescriptor> = [
  { path: "metrics.radius.progress", label: "Progress", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.xs", label: "Extra Small", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.segmentCell", label: "Segment Cell", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.sm", label: "Small", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.md", label: "Medium", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.input", label: "Input", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.card", label: "Card", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.iconTile", label: "Icon Tile", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.dialog", label: "Dialog", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.chipLg", label: "Chip Large", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.chipXl", label: "Chip Extra Large", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.badge", label: "Badge", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 0.5 },
  { path: "metrics.radius.pill", label: "Pill", groupKey: "radius", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },

  { path: "metrics.stroke.thin", label: "Thin", groupKey: "stroke", kind: "number", unit: "px", min: 0, max: 8, step: 0.5 },
  { path: "metrics.stroke.thick", label: "Thick", groupKey: "stroke", kind: "number", unit: "px", min: 0, max: 8, step: 0.5 },

  { path: "metrics.spacing.xs", label: "Extra Small", groupKey: "spacing", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.spacing.sm", label: "Small", groupKey: "spacing", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.spacing.md", label: "Medium", groupKey: "spacing", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.spacing.lg", label: "Large", groupKey: "spacing", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.spacing.xl", label: "Extra Large", groupKey: "spacing", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },

  { path: "metrics.button.heightSm", label: "Height Small", groupKey: "button", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.button.heightMd", label: "Height Medium", groupKey: "button", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },
  { path: "metrics.button.heightLg", label: "Height Large", groupKey: "button", kind: "number", unit: "px", min: 0, max: 2000, step: 1 },

  { path: "metrics.alpha.subtle", label: "Subtle", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.soft", label: "Soft", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.muted", label: "Muted", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.mutedStrong", label: "Muted Strong", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.active", label: "Active", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.selectedBorder", label: "Selected Border", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.scrim", label: "Scrim", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.border", label: "Border", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.borderStrong", label: "Border Strong", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.disabled", label: "Disabled", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.glass", label: "Glass", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.focusRing", label: "Focus Ring", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.shadowStrong", label: "Shadow Strong", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.shadowSoft", label: "Shadow Soft", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.shadowFaint", label: "Shadow Faint", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.faint", label: "Faint", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.textSelection", label: "Text Selection", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.borderSubtle", label: "Border Subtle", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.borderFaint", label: "Border Faint", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.borderMedium", label: "Border Medium", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.emphasis", label: "Emphasis", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
  { path: "metrics.alpha.glassSubtle", label: "Glass Subtle", groupKey: "alpha", kind: "number", unit: "alpha", min: 0, max: 1, step: 0.01 },
];

export const defaultDarkTheme: FluxThemeJson = {
  name: "Default Dark",
  appearance: "dark",
  colors: {
    surface: {
      background: "ff1c1c1e",
      surface1: "ff2c2c2e",
      surface2: "ff3a3a3c",
      surface3: "ff48484a",
    },
    element: {
      hover: "ff424245",
      selected: "ff3a3a3c",
      active: "2e3b82f6",
    },
    text: {
      primary: "fff5f5f7",
      secondary: "ffa1a1a6",
      muted: "ff8e8e93",
      disabled: "808e8e93",
    },
    border: {
      default: "ff48484a",
      focused: "ff3b82f6",
    },
    accent: {
      color: "ff3b82f6",
      hover: "ff629bf8",
      background: "2e3b82f6",
      foreground: "ffffffff",
    },
    input: {
      background: "ff1c1c1e",
      border: "ff48484a",
      focusBorder: "ff3b82f6",
      focusBackground: "143b82f6",
    },
    dialog: {
      background: "ff2c2c2e",
      barrier: "40000000",
    },
    switch: {
      track: "ff636366",
      thumb: "ffffffff",
    },
    shadow: "ff000000",
    status: {
      success: "ff22c55e",
      warning: "fff59e0b",
      error: "ffef4444",
    },
    segmentPalette: DEFAULT_SEGMENT_PALETTE,
  },
  metrics: DEFAULT_METRICS,
  schemaVersion: 2,
};

export const defaultLightTheme: FluxThemeJson = {
  name: "Default Light",
  appearance: "light",
  colors: {
    surface: {
      background: "fff8f9fa",
      surface1: "ffffffff",
      surface2: "fff1f3f5",
      surface3: "ffe9ecef",
    },
    element: {
      hover: "fff1f3f5",
      selected: "1a3b82f6",
      active: "1a3b82f6",
    },
    text: {
      primary: "ff09090b",
      secondary: "ff71717a",
      muted: "ffa1a1aa",
      disabled: "80a1a1aa",
    },
    border: {
      default: "ffe4e4e7",
      focused: "ff3b82f6",
    },
    accent: {
      color: "ff3b82f6",
      hover: "ff5895f7",
      background: "1a3b82f6",
      foreground: "ffffffff",
    },
    input: {
      background: "ffffffff",
      border: "ffe4e4e7",
      focusBorder: "ff3b82f6",
      focusBackground: "ffffffff",
    },
    dialog: {
      background: "ffffffff",
      barrier: "1a000000",
    },
    switch: {
      track: "ffe5e5ea",
      thumb: "ffffffff",
    },
    shadow: "ff000000",
    status: {
      success: "ff22c55e",
      warning: "fff59e0b",
      error: "ffef4444",
    },
    segmentPalette: DEFAULT_SEGMENT_PALETTE,
  },
  metrics: DEFAULT_METRICS,
  schemaVersion: 2,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function deepCloneTheme(theme: FluxThemeJson): FluxThemeJson {
  return JSON.parse(JSON.stringify(theme)) as FluxThemeJson;
}

export function normalizeHex8(input: string): string {
  const trimmed = input.trim().replace(/^#/, "").toLowerCase();
  if (!/^[0-9a-f]+$/.test(trimmed)) return "ff000000";
  if (trimmed.length === 3) {
    const rgb = trimmed.split("").map((char) => `${char}${char}`).join("");
    return `ff${rgb}`;
  }
  if (trimmed.length === 4) {
    return trimmed.split("").map((char) => `${char}${char}`).join("");
  }
  if (trimmed.length === 6) return `ff${trimmed}`;
  if (trimmed.length === 8) return trimmed;
  return "ff000000";
}

export function argbToCssRgba(hex8: string): string {
  const normalized = normalizeHex8(hex8);
  const alpha = Number.parseInt(normalized.slice(0, 2), 16) / 255;
  const red = Number.parseInt(normalized.slice(2, 4), 16);
  const green = Number.parseInt(normalized.slice(4, 6), 16);
  const blue = Number.parseInt(normalized.slice(6, 8), 16);
  return `rgba(${red}, ${green}, ${blue}, ${alpha.toFixed(3)})`;
}

export function argbToRgbHex(hex8: string): string {
  const normalized = normalizeHex8(hex8);
  return `#${normalized.slice(2, 8)}`;
}

export function withRgb(hex8: string, rgb: string): string {
  const normalizedColor = normalizeHex8(hex8);
  const rgbNormalized = normalizeHex8(rgb).slice(2, 8);
  return `${normalizedColor.slice(0, 2)}${rgbNormalized}`;
}

export function withAlpha(hex8: string, alpha: number): string {
  const normalizedColor = normalizeHex8(hex8);
  const clamped = Math.max(0, Math.min(255, Math.round(alpha)));
  const alphaHex = clamped.toString(16).padStart(2, "0");
  return `${alphaHex}${normalizedColor.slice(2, 8)}`;
}

export function getTokenDescriptors(theme: FluxThemeJson): TokenDescriptor[] {
  const segmentDescriptors = theme.colors.segmentPalette.map((_, index) => ({
    path: `colors.segmentPalette.${index}`,
    label: `Segment ${index + 1}`,
    groupKey: "segment" as const,
  }));
  return [...STATIC_TOKEN_DESCRIPTORS, ...METRIC_TOKEN_DESCRIPTORS, ...segmentDescriptors];
}

export function getPathValue(root: unknown, path: string): unknown {
  const parts = path.split(".");
  let current: unknown = root;

  for (const part of parts) {
    if (Array.isArray(current)) {
      const index = Number.parseInt(part, 10);
      if (Number.isNaN(index)) return undefined;
      current = current[index];
      continue;
    }

    if (!isRecord(current)) return undefined;
    current = current[part];
  }

  return current;
}

export function setPathValue(
  theme: FluxThemeJson,
  path: string,
  value: string | string[] | number,
): FluxThemeJson {
  const next = deepCloneTheme(theme);
  const parts = path.split(".");
  let current: unknown = next;

  for (let i = 0; i < parts.length - 1; i += 1) {
    const part = parts[i]!;
    if (Array.isArray(current)) {
      const index = Number.parseInt(part, 10);
      if (Number.isNaN(index)) return theme;
      current = current[index];
      continue;
    }
    if (!isRecord(current)) return theme;
    current = current[part];
  }

  const last = parts[parts.length - 1]!;
  if (Array.isArray(current)) {
    const index = Number.parseInt(last, 10);
    if (!Number.isNaN(index)) current[index] = value;
    return next;
  }

  if (!isRecord(current)) return theme;
  current[last] = value;
  return next;
}

function readMaybeString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

export function parseFluxThemeJson(raw: string): FluxThemeJson {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error("Invalid JSON");
  }

  if (!isRecord(parsed)) {
    throw new Error("Invalid theme schema");
  }
  if (typeof parsed.name !== "string" || parsed.name.trim() === "") {
    throw new Error("Theme name is required");
  }
  if (!isRecord(parsed.colors)) {
    throw new Error("Invalid theme schema: missing colors");
  }

  return normalizeFluxThemeJson(parsed);
}

export function normalizeFluxThemeJson(raw: unknown): FluxThemeJson {
  if (!isRecord(raw)) return deepCloneTheme(defaultDarkTheme);
  const appearance = raw.appearance === "light" ? "light" : "dark";
  let next = deepCloneTheme(appearance === "light" ? defaultLightTheme : defaultDarkTheme);

  const name = readMaybeString(raw.name);
  if (name && name.trim() !== "") {
    next.name = name.trim();
  }
  const author = readMaybeString(raw.author);
  if (author && author.trim() !== "") {
    next.author = author.trim();
  } else {
    delete next.author;
  }
  next.appearance = appearance;

  const colors = isRecord(raw.colors) ? raw.colors : undefined;
  if (!colors) return next;

  const applyColor = (path: string, value: unknown) => {
    if (typeof value !== "string") return;
    const normalized = normalizeHex8(value);
    next = setPathValue(next, path, normalized);
  };

  const surface = isRecord(colors.surface) ? colors.surface : {};
  applyColor("colors.surface.background", surface.background);
  applyColor("colors.surface.surface1", surface.surface1);
  applyColor("colors.surface.surface2", surface.surface2);
  applyColor("colors.surface.surface3", surface.surface3);

  const element = isRecord(colors.element) ? colors.element : {};
  applyColor("colors.element.hover", element.hover);
  applyColor("colors.element.selected", element.selected);
  applyColor("colors.element.active", element.active);

  const text = isRecord(colors.text) ? colors.text : {};
  applyColor("colors.text.primary", text.primary);
  applyColor("colors.text.secondary", text.secondary);
  applyColor("colors.text.muted", text.muted);
  applyColor("colors.text.disabled", text.disabled);

  const border = isRecord(colors.border) ? colors.border : {};
  applyColor("colors.border.default", border.default);
  applyColor("colors.border.focused", border.focused);

  const accent = isRecord(colors.accent) ? colors.accent : {};
  applyColor("colors.accent.color", accent.color);
  applyColor("colors.accent.hover", accent.hover);
  applyColor("colors.accent.background", accent.background);
  applyColor("colors.accent.foreground", accent.foreground);

  const input = isRecord(colors.input) ? colors.input : {};
  applyColor("colors.input.background", input.background);
  applyColor("colors.input.border", input.border);
  applyColor("colors.input.focusBorder", input.focusBorder);
  applyColor("colors.input.focusBackground", input.focusBackground);

  const dialog = isRecord(colors.dialog) ? colors.dialog : {};
  applyColor("colors.dialog.background", dialog.background);
  applyColor("colors.dialog.barrier", dialog.barrier);

  const switchColors = isRecord(colors.switch) ? colors.switch : {};
  applyColor("colors.switch.track", switchColors.track);
  applyColor("colors.switch.thumb", switchColors.thumb);

  applyColor("colors.shadow", colors.shadow);

  const status = isRecord(colors.status) ? colors.status : {};
  applyColor("colors.status.success", status.success);
  applyColor("colors.status.warning", status.warning);
  applyColor("colors.status.error", status.error);

  if (Array.isArray(colors.segmentPalette)) {
    const normalizedPalette = colors.segmentPalette
      .filter((value): value is string => typeof value === "string")
      .map((value) => normalizeHex8(value));
    if (normalizedPalette.length > 0) {
      next = setPathValue(next, "colors.segmentPalette", normalizedPalette);
    }
  }

  const metrics = isRecord(raw.metrics) ? raw.metrics : undefined;
  if (metrics) {
    for (const descriptor of METRIC_TOKEN_DESCRIPTORS) {
      const value = getPathValue({ metrics }, descriptor.path);
      if (typeof value !== "number" || Number.isNaN(value)) continue;
      const min = descriptor.min ?? 0;
      const max = descriptor.max ?? Number.POSITIVE_INFINITY;
      const clamped = Math.min(max, Math.max(min, value));
      next = setPathValue(next, descriptor.path, clamped);
    }
  }

  return next;
}

export function exportFluxThemeJson(theme: FluxThemeJson): string {
  return `${JSON.stringify(theme, null, 2)}\n`;
}
