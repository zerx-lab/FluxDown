/** options 各面板共用的版式组件。 */
import type { ComponentChildren, JSX } from "preact";
import { Button } from "@/ui/components/Button";
import { IconButton } from "@/ui/components/IconButton";
import { Modal } from "@/ui/components/Modal";
import { Select } from "@/ui/components/Select";
import { Toggle } from "@/ui/components/Toggle";
import { useT } from "@/ui/i18n";
import { PAGE_SIZE_OPTIONS, type PageSize, type PageSlice, pageWindow } from "./lib/pagination";

export function PanelShell(props: { title: string; desc?: string; children?: ComponentChildren }): JSX.Element {
  return (
    <section class="opt-panel">
      <h1 class="opt-panel-title">{props.title}</h1>
      {props.desc ? <p class="opt-panel-desc">{props.desc}</p> : null}
      {props.children}
    </section>
  );
}

/**
 * 面板内分组：组标题 + 一组设置行。语义相反或相互独立的设置（如「拦截范围」与「排除规则」）
 * 分进不同组，避免相邻行被读成同一规则的从属项。
 */
export function SettingGroup(props: { title: string; desc?: string; children?: ComponentChildren }): JSX.Element {
  return (
    <section class="opt-group">
      <div class="opt-group-head">
        <h2 class="opt-group-title">{props.title}</h2>
        {props.desc ? <p class="opt-group-desc">{props.desc}</p> : null}
      </div>
      {props.children}
    </section>
  );
}

interface SettingRowProps {
  title: string;
  desc?: string;
  /** 说明下方的补充提示（强调色）。 */
  note?: string;
  /** 堆叠布局：说明在上、控件在下（列表类内容较宽）。 */
  stack?: boolean;
  /** 控件横向靠右排列（开关 / 按钮）。 */
  inline?: boolean;
  children?: ComponentChildren;
}

export function SettingRow(props: SettingRowProps): JSX.Element {
  return (
    <div class={`opt-row${props.stack ? " opt-row-stack" : ""}`}>
      <div class="opt-row-info">
        <div class="opt-row-title">{props.title}</div>
        {props.desc ? <div class="opt-row-desc">{props.desc}</div> : null}
        {props.note ? <div class="opt-row-desc opt-row-note">{props.note}</div> : null}
      </div>
      <div class={props.stack ? "opt-row-wide" : `opt-row-control${props.inline ? " opt-row-control-inline" : ""}`}>
        {props.children}
      </div>
    </div>
  );
}

interface ToggleRowProps {
  title: string;
  desc?: string;
  note?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
}

export function ToggleRow(props: ToggleRowProps): JSX.Element {
  return (
    <SettingRow title={props.title} desc={props.desc} note={props.note} inline>
      <Toggle checked={props.checked} onChange={props.onChange} disabled={props.disabled} label={props.title} />
    </SettingRow>
  );
}

interface ConfirmModalProps {
  title: string;
  message: ComponentChildren;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmModal(props: ConfirmModalProps): JSX.Element {
  const t = useT();
  return (
    <Modal
      title={props.title}
      onClose={props.onCancel}
      width={440}
      footer={
        <>
          <Button onClick={props.onCancel}>{t("options.cancel")}</Button>
          <Button variant={props.danger ? "danger" : "primary"} onClick={props.onConfirm}>
            {props.confirmLabel}
          </Button>
        </>
      }
    >
      <div class="opt-confirm">{props.message}</div>
    </Modal>
  );
}

interface PaginationProps {
  total: number;
  slice: PageSlice;
  pageSize: PageSize;
  onPageChange: (page: number) => void;
  onPageSizeChange: (size: PageSize) => void;
}

/** 列表分页条；总数不超过最小每页条数时不渲染。翻页后若卡片顶部已滚出视口，滚回卡片顶部。 */
export function Pagination(props: PaginationProps): JSX.Element | null {
  const t = useT();
  if (props.total <= PAGE_SIZE_OPTIONS[0]) return null;
  const { page, pageCount, start, end } = props.slice;

  const go = (target: number, e: MouseEvent) => {
    if (target === page) return;
    const card = (e.currentTarget as HTMLElement).closest(".opt-card");
    props.onPageChange(target);
    if (card && card.getBoundingClientRect().top < 0) card.scrollIntoView({ block: "start" });
  };

  return (
    <div class="opt-pager">
      <span class="opt-pager-summary">
        {t("options.pager.summary", { from: String(start + 1), to: String(end), total: String(props.total) })}
      </span>
      <div class="opt-pager-nav">
        <Select<`${PageSize}`>
          value={`${props.pageSize}`}
          label={t("options.pager.pageSize")}
          options={PAGE_SIZE_OPTIONS.map((size) => ({
            value: `${size}`,
            label: t("options.pager.perPage", { count: String(size) }),
          }))}
          onChange={(v) => props.onPageSizeChange(Number(v) as PageSize)}
        />
        <IconButton
          icon="chevronLeft"
          label={t("options.pager.prev")}
          disabled={page <= 1}
          onClick={(e) => go(page - 1, e)}
        />
        {pageWindow(page, pageCount).map((item, i) =>
          item === "gap" ? (
            <span key={`gap-${i}`} class="opt-pager-gap">
              …
            </span>
          ) : (
            <button
              key={item}
              type="button"
              class={`opt-pager-page${item === page ? " opt-pager-current" : ""}`}
              aria-current={item === page ? "page" : undefined}
              onClick={(e) => go(item, e)}
            >
              {item}
            </button>
          ),
        )}
        <IconButton
          icon="chevronRight"
          label={t("options.pager.next")}
          disabled={page >= pageCount}
          onClick={(e) => go(page + 1, e)}
        />
      </div>
    </div>
  );
}
