import { useEffect, useRef, type CSSProperties } from "react";
import { Bell, Crown, Laptop, LayoutDashboard, ShieldCheck, type LucideIcon } from "lucide-react";
import type { AccountMessages } from "@/i18n/messages/account";
import { SECTIONS, type Section } from "./context";
import { sectionLink } from "./useSection";

const ICONS: Record<Section, LucideIcon> = {
  overview: LayoutDashboard,
  membership: Crown,
  devices: Laptop,
  notifications: Bell,
  security: ShieldCheck,
};

const EDGE = "color-mix(in oklch, var(--fg) 16%, transparent)";

/** 窄屏横向滚动条的边缘提示:纯 CSS(`local` 遮罩盖住滚动到头的一侧阴影),无需脚本。 */
const SCROLL_SHADOW: CSSProperties = {
  backgroundColor: "var(--bg)",
  backgroundImage: [
    "linear-gradient(to right, var(--bg), transparent)",
    "linear-gradient(to left, var(--bg), transparent)",
    `linear-gradient(to right, ${EDGE}, transparent)`,
    `linear-gradient(to left, ${EDGE}, transparent)`,
  ].join(", "),
  backgroundPosition: "left, right, left, right",
  backgroundSize: "28px 100%, 28px 100%, 12px 100%, 12px 100%",
  backgroundRepeat: "no-repeat",
  backgroundAttachment: "local, local, scroll, scroll",
};

interface Props {
  section: Section;
  labels: AccountMessages["nav"];
  go: (section: Section) => void;
}

/**
 * 分区导航:每项是指向 `#section` 的真链接(可复制、可中键新开),普通左键点击走 pushState。
 * lg 起为左侧 sticky 竖排;窄屏为吸顶的横向滚动标签条。
 */
export default function SectionNav({ section, labels, go }: Props) {
  const navRef = useRef<HTMLElement>(null);

  // 窄屏下把当前项滚到条中央(只改条自己的 scrollLeft,不牵动页面滚动)。
  useEffect(() => {
    const nav = navRef.current;
    const active = nav?.querySelector<HTMLElement>('[aria-current="page"]');
    if (nav && active && nav.scrollWidth > nav.clientWidth) {
      nav.scrollLeft = active.offsetLeft - (nav.clientWidth - active.offsetWidth) / 2;
    }
  }, [section]);

  return (
    <nav
      ref={navRef}
      aria-label={labels.label}
      style={SCROLL_SHADOW}
      className="sticky top-[var(--header-h)] z-10 overflow-x-auto [scrollbar-width:none] max-lg:-mx-[clamp(20px,4vw,56px)] max-lg:border-b max-lg:border-line max-lg:px-[clamp(20px,4vw,56px)] lg:top-[calc(var(--header-h)+24px)] lg:self-start lg:overflow-visible"
    >
      <ul className="flex gap-1 lg:flex-col">
        {SECTIONS.map((id) => {
          const Icon = ICONS[id];
          const active = id === section;
          return (
            <li key={id} className="shrink-0 lg:shrink">
              <a
                {...sectionLink(id, go)}
                aria-current={active ? "page" : undefined}
                className={`flex h-11 items-center gap-2.5 whitespace-nowrap border-b-2 px-3 text-sm transition-colors focus-visible:outline-offset-[-2px] lg:h-10 lg:border-b-0 lg:border-l-2 ${
                  active
                    ? "border-accent bg-accent-soft font-medium text-accent-ink"
                    : "border-transparent text-muted hover:bg-sunken hover:text-fg"
                }`}
              >
                <Icon size={16} aria-hidden />
                {labels[id]}
              </a>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
