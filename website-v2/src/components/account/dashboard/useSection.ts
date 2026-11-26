import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { SECTIONS, type Section } from "./context";

function fromHash(hash: string): Section {
  const id = hash.replace(/^#/, "");
  return (SECTIONS as readonly string[]).includes(id) ? (id as Section) : "overview";
}

/**
 * 分区状态 = URL hash(`#devices` 等)。首屏与 SSR 一致地从 overview 起步,挂载后读 hash;
 * 点击导航用 pushState 写历史(前进 / 后退经 popstate / hashchange 回流),不触发浏览器的锚点滚动。
 */
export function useSection(): [Section, (next: Section) => void] {
  const [section, setSection] = useState<Section>("overview");

  useEffect(() => {
    const sync = () => setSection(fromHash(location.hash));
    sync();
    window.addEventListener("hashchange", sync);
    window.addEventListener("popstate", sync);
    return () => {
      window.removeEventListener("hashchange", sync);
      window.removeEventListener("popstate", sync);
    };
  }, []);

  const go = useCallback((next: Section) => {
    if (location.hash !== `#${next}`) history.pushState(null, "", `#${next}`);
    setSection(next);
  }, []);

  return [section, go];
}

/** 指向分区的真链接属性:左键走 pushState(不触发锚点滚动),中键 / 修饰键保留浏览器默认行为。 */
export function sectionLink(id: Section, go: (next: Section) => void) {
  return {
    href: `#${id}`,
    onClick: (e: MouseEvent<HTMLAnchorElement>) => {
      if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      e.preventDefault();
      go(id);
    },
  };
}
