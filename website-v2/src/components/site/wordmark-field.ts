/**
 * 页脚「FluxDown」巨型字标的粒子版:把字标按页面实际排版栅格化后高密度采样成粒子。
 *
 * - 首次进入视口:粒子自左向右依次从下方散落处汇流成字。
 * - 着色:字腔填充按横向位置从强调色渐变到 cyan,轮廓粒子更大更亮,保证字形清晰可读;
 *   自下而上渐隐,同原遮罩。
 * - 扫光:每隔几秒一道斜向光带掠过字标,经过处粒子点亮并带辉光。
 * - 指针:像一枚放大镜/聚光灯——光标附近的粒子被点亮并向外轻微鼓起几像素
 *   (按归位坐标计算,字形始终可读,不会被搅散);移开后余辉慢慢冷却,留下一道淡淡的光痕。
 * - 按下:从按下点扩散出一圈涟漪,波前经过处粒子短暂发亮并沿径向轻轻起伏,随距离与时间衰减。
 * - 空闲时随机零星闪烁。
 */
import { StrokeBatch, smoothstep, type Palette, type Pointer, type Scene } from "../../scripts/particle-stage";

interface Grain {
  hx: number;
  hy: number;
  x: number;
  y: number;
  vx: number;
  vy: number;
  heat: number;
  edge: boolean;
  cyan: boolean;
  /** 自下而上渐隐系数(按归位坐标,0..1) */
  fade: number;
  /** 入场剩余等待(秒):到时才开始汇流 */
  delay: number;
}

interface Ripple {
  x: number;
  y: number;
  /** 已扩散时长(秒) */
  age: number;
}

const SPRING = 60;
/** 透镜半径与最大鼓起位移(px) */
const LENS_R = 170;
const LENS_PUSH = 7;
/** 涟漪波前速度(px/s)、波带半宽(px)、最大径向起伏(px)、寿命(s) */
const RIPPLE_SPEED = 560;
const RIPPLE_BAND = 46;
const RIPPLE_PUSH = 5;
const RIPPLE_LIFE = 1.8;
const MAX_RIPPLES = 4;
/** 扫光:周期(s)、单次掠过时长(s)、光带半宽(px)、斜率(y 对 x 的贡献) */
const SWEEP_PERIOD = 5.5;
const SWEEP_DURATION = 1.6;
const SWEEP_BAND = 70;
const SWEEP_SLANT = 0.45;
/** 入场:自左向右的最大错峰(s) */
const INTRO_STAGGER = 0.7;

export class WordmarkField implements Scene {
  private grains: Grain[] = [];
  private ripples: Ripple[] = [];
  private width = 0;
  private height = 0;
  private introPending = true;
  /** 入场完成后才开始计时扫光,避免与汇流叠在一起 */
  private clock = -1.2;

  private readonly fillBatch = new StrokeBatch(2);
  private readonly edgeBatch = new StrokeBatch(2.4);
  private readonly glowBatch = new StrokeBatch(7);
  private readonly hotBatch = new StrokeBatch(3);

  constructor(
    private readonly text: HTMLElement,
    private readonly canvas: HTMLCanvasElement,
  ) {}

  resize(width: number, height: number) {
    this.width = width;
    this.height = height;
    this.sample();
  }

  /** 按字标 span 的实际字体与位置栅格化并采样(字体加载完成后需重采)。 */
  sample() {
    const { width, height } = this;
    const node = this.text.firstChild;
    if (!width || !height || !node) return;
    const style = getComputedStyle(this.text);
    const size = Number.parseFloat(style.fontSize) || 200;
    const range = document.createRange();
    range.selectNodeContents(node);
    const box = range.getBoundingClientRect();
    const origin = this.canvas.getBoundingClientRect();

    const off = document.createElement("canvas");
    off.width = Math.ceil(width);
    off.height = Math.ceil(height);
    const ctx = off.getContext("2d", { willReadFrequently: true });
    if (!ctx) return;
    ctx.font = `${style.fontWeight} ${size}px ${style.fontFamily}`;
    if ("letterSpacing" in ctx) ctx.letterSpacing = style.letterSpacing === "normal" ? "0px" : style.letterSpacing;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillStyle = "#000";
    ctx.fillText(this.text.textContent ?? "", box.left - origin.left + box.width / 2, box.top - origin.top + box.height / 2);
    const data = ctx.getImageData(0, 0, off.width, off.height).data;
    const inside = (x: number, y: number) => {
      const ix = Math.round(x);
      const iy = Math.round(y);
      if (ix < 0 || iy < 0 || ix >= off.width || iy >= off.height) return false;
      return (data[(iy * off.width + ix) * 4 + 3] ?? 0) > 110;
    };

    // 栅格足够细才能看清字形笔画(窄屏字号小,下限 3px)
    const step = Math.max(3, Math.round(size / 62));
    const probe = step * 0.75;
    const grains: Grain[] = [];
    for (let y = step / 2; y < height; y += step) {
      for (let x = step / 2; x < width; x += step) {
        if (!inside(x, y)) continue;
        const edge = !inside(x - probe, y) || !inside(x + probe, y) || !inside(x, y - probe) || !inside(x, y + probe);
        grains.push({
          hx: x,
          hy: y,
          x,
          y,
          vx: 0,
          vy: 0,
          heat: 0,
          edge,
          // 横向强调色 → cyan 渐变:按位置抖动取色,整体呈平滑过渡
          cyan: Math.random() < smoothstep(0.1, 0.9, x / width),
          fade: 1 - smoothstep(0.45 * height, 1.05 * height, y),
          delay: 0,
        });
      }
    }
    this.grains = grains;
  }

  press(x: number, y: number) {
    if (this.ripples.length >= MAX_RIPPLES) this.ripples.shift();
    this.ripples.push({ x, y, age: 0 });
  }

  frame(ctx: CanvasRenderingContext2D, dt: number, p: Pointer, pal: Palette) {
    if (!this.grains.length) return;
    if (dt > 0 && this.introPending) {
      // 汇流入场:自左向右依次从下方散落处飞回字形
      this.introPending = false;
      for (const g of this.grains) {
        g.x = g.hx + (Math.random() - 0.5) * this.width * 0.3;
        g.y = this.height + 20 + Math.random() * 160;
        g.vx = g.vy = 0;
        g.delay = (g.hx / this.width) * INTRO_STAGGER + Math.random() * 0.15;
      }
    }

    const damp = Math.exp(-7 * dt);
    const cool = Math.exp(-1.6 * dt);
    if (dt > 0) {
      this.clock += dt;
      // 空闲闪烁
      for (let i = 0; i < 5; i++) {
        const g = this.grains[Math.floor(Math.random() * this.grains.length)];
        if (g) g.heat = Math.max(g.heat, 0.6);
      }
      let alive = 0;
      for (const r of this.ripples) {
        r.age += dt;
        if (r.age < RIPPLE_LIFE) this.ripples[alive++] = r;
      }
      this.ripples.length = alive;

      // 扫光波前(沿 u = x + SWEEP_SLANT * y 推进);不在掠过阶段时为 null
      const phase = this.clock > 0 ? this.clock % SWEEP_PERIOD : -1;
      const span = this.width + SWEEP_SLANT * this.height + 2 * SWEEP_BAND;
      const sweep = phase >= 0 && phase < SWEEP_DURATION ? smoothstep(0, 1, phase / SWEEP_DURATION) * span - SWEEP_BAND : null;

      for (const g of this.grains) {
        if (g.delay > 0) {
          g.delay -= dt;
          continue;
        }
        // 目标位置 = 归位点 + 透镜鼓起 + 涟漪起伏;位移只按归位坐标算,避免与当前位置互相反馈抖动
        let tx = g.hx;
        let ty = g.hy;
        if (p.presence > 0.01) {
          const dx = g.hx - p.x;
          const dy = g.hy - p.y;
          const d2 = dx * dx + dy * dy;
          if (d2 < LENS_R * LENS_R) {
            const d = Math.sqrt(d2) || 1;
            const t = d / LENS_R;
            const push = LENS_PUSH * Math.sin(Math.PI * t) * p.presence;
            tx += (dx / d) * push;
            ty += (dy / d) * push;
            g.heat = Math.max(g.heat, smoothstep(1, 0, t) * p.presence);
          }
        }
        if (sweep !== null) {
          const band = 1 - Math.abs(g.hx + SWEEP_SLANT * g.hy - sweep) / SWEEP_BAND;
          if (band > 0) g.heat = Math.max(g.heat, band * band * 0.85);
        }
        for (const r of this.ripples) {
          const dx = g.hx - r.x;
          const dy = g.hy - r.y;
          const d = Math.hypot(dx, dy) || 1;
          const band = 1 - Math.abs(d - r.age * RIPPLE_SPEED) / RIPPLE_BAND;
          if (band <= 0) continue;
          const k = band * band * (1 - r.age / RIPPLE_LIFE) ** 2;
          tx += (dx / d) * RIPPLE_PUSH * k;
          ty += (dy / d) * RIPPLE_PUSH * k;
          g.heat = Math.max(g.heat, k);
        }
        g.vx = (g.vx + (tx - g.x) * SPRING * dt) * damp;
        g.vy = (g.vy + (ty - g.y) * SPRING * dt) * damp;
        g.x += g.vx * dt;
        g.y += g.vy * dt;
        g.heat *= cool;
      }
    }

    const tone = pal.dark ? 1 : 0.92;
    for (const g of this.grains) {
      const moving = Math.abs(g.vx) + Math.abs(g.vy) > 30;
      // 运动中拖出短尾迹(入场汇流时形成流星感)
      const x0 = moving ? g.x - g.vx * 0.022 : g.x;
      const y0 = moving ? g.y - g.vy * 0.022 : g.y;
      const x1 = moving ? g.x : g.x + 0.01;
      // 运动中的粒子(入场汇流)不受渐隐影响太多,保证入场可见
      const fade = Math.max(g.fade, moving ? 0.6 : 0) * tone;
      const hue = g.cyan ? "cyan" : "accent";
      if (g.heat > 0.3) {
        this.glowBatch.line(pal, hue, (pal.dark ? 0.22 : 0.14) * g.heat * fade, x0, y0, x1, g.y);
        this.hotBatch.line(pal, hue, (0.55 + 0.45 * g.heat) * fade, x0, y0, x1, g.y);
      } else if (g.edge) {
        this.edgeBatch.line(pal, hue, (0.78 + 0.6 * g.heat) * fade, x0, y0, x1, g.y);
      } else {
        this.fillBatch.line(pal, hue, (0.3 + 0.8 * g.heat) * fade, x0, y0, x1, g.y);
      }
    }
    this.fillBatch.flush(ctx);
    this.edgeBatch.flush(ctx);
    if (pal.dark) ctx.globalCompositeOperation = "lighter";
    this.glowBatch.flush(ctx);
    this.hotBatch.flush(ctx);
    ctx.globalCompositeOperation = "source-over";
  }
}
