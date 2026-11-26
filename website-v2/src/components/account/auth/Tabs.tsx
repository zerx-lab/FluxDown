import type { KeyboardEvent } from "react";

export const tabId = (prefix: string, id: string) => `${prefix}-tab-${id}`;
export const panelId = (prefix: string, id: string) => `${prefix}-panel-${id}`;

/**
 * 分段切换(WAI-ARIA tabs,自动激活):方向键 / Home / End 在选项间移动并立即切换,
 * 只有选中的 tab 在 Tab 键序里(roving tabindex)。面板由调用方用 `panelId` / `tabId` 关联,
 * 两个面板都留在 DOM 里(用 `hidden` 隐藏),切换时已填内容不丢。
 */
export default function Tabs<T extends string>({
  label,
  idPrefix,
  items,
  value,
  onChange,
}: {
  label: string;
  idPrefix: string;
  items: readonly { id: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
}) {
  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let next: number;
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        next = (index + 1) % items.length;
        break;
      case "ArrowLeft":
      case "ArrowUp":
        next = (index - 1 + items.length) % items.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = items.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    const target = items[next];
    onChange(target.id);
    document.getElementById(tabId(idPrefix, target.id))?.focus();
  }

  return (
    <div
      role="tablist"
      aria-label={label}
      className="grid gap-1 rounded-[10px] bg-sunken p-1 shadow-[0_0_0_1px_var(--line)_inset]"
      style={{ gridTemplateColumns: `repeat(${items.length}, minmax(0, 1fr))` }}
    >
      {items.map((item, index) => {
        const selected = item.id === value;
        return (
          <button
            key={item.id}
            type="button"
            role="tab"
            id={tabId(idPrefix, item.id)}
            aria-selected={selected}
            aria-controls={panelId(idPrefix, item.id)}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(item.id)}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={`h-8 truncate rounded-[7px] px-3 text-[13px] font-medium transition-[color,background-color,box-shadow] duration-150 ${
              selected ? "bg-elev text-fg shadow-[0_0_0_1px_var(--line-strong)]" : "text-muted hover:text-fg"
            }`}
          >
            {item.label}
          </button>
        );
      })}
    </div>
  );
}
