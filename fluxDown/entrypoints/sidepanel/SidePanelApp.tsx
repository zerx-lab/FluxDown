/**
 * 侧边栏根组件：跟随当前窗口的活动 tab，四个页签（资源 / 链接 / 任务 / 诊断）。
 */
import type { JSX } from "preact";
import { useCallback, useEffect, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import type { HostStatus } from "@/background/protocol";
import { sendAction } from "@/utils/messaging";
import { loadSettings, saveSettings } from "@/utils/settings";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { Icon } from "@/ui/components/Icon";
import { Tabs, type TabItem } from "@/ui/components/Tabs";
import { ToastHost, showToast } from "@/ui/components/Toast";
import { HandoffCard } from "@/ui/features/HandoffCard";
import { LinkGrabber } from "@/ui/features/LinkGrabber";
import { downloadSelection, ResourceList, type ListSelection } from "@/ui/features/ResourceList";
import { SiteCard } from "@/ui/features/SiteCard";
import { TaskList } from "@/ui/features/TaskList";
import { useTabView } from "@/ui/hooks/useTabView";
import { useT } from "@/ui/i18n";
import { useThemeAttr } from "@/ui/theme";
import { DiagnosticsPane } from "./DiagnosticsPane";
import "@/ui/theme/tokens.css";
import "@/ui/theme/base.css";
import "./sidepanel.css";

type PaneKey = "resources" | "links" | "tasks" | "diagnostics";

interface ActiveTab {
  id: number;
  url: string;
}

const EMPTY_SELECTION: ListSelection = { candidateIds: [], resourceIds: [] };

/** 当前窗口的活动 tab；tab 切换或页面加载完成后重新查询（同一 tab 不会重复订阅）。 */
function useActiveTab(): ActiveTab | null {
  const [active, setActive] = useState<ActiveTab | null>(null);
  useEffect(() => {
    let alive = true;
    const refresh = () => {
      browser.tabs
        .query({ active: true, currentWindow: true })
        .then((tabs) => {
          if (!alive) return;
          const tab = tabs[0];
          setActive(tab?.id === undefined ? null : { id: tab.id, url: tab.url ?? "" });
        })
        .catch(() => {});
    };
    const onUpdated = (_id: number, change: { status?: string }) => {
      if (change.status === "complete") refresh();
    };
    refresh();
    browser.tabs.onActivated.addListener(refresh);
    browser.tabs.onUpdated.addListener(onUpdated);
    return () => {
      alive = false;
      browser.tabs.onActivated.removeListener(refresh);
      browser.tabs.onUpdated.removeListener(onUpdated);
    };
  }, []);
  return active;
}

/** 桌面端状态：挂载、面板重新可见与手动刷新时取一次。 */
function useHostStatus(): { status: HostStatus | null; refresh: () => void } {
  const [status, setStatus] = useState<HostStatus | null>(null);
  const refresh = useCallback(() => {
    sendAction<"getHostStatus">({ action: "getHostStatus" })
      .then(setStatus)
      .catch(() => setStatus(null));
  }, []);
  useEffect(() => {
    refresh();
    const onVisible = () => {
      if (!document.hidden) refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [refresh]);
  return { status, refresh };
}

/** settings.showLowConfidence，随 storage 变化同步。 */
function useShowLowConfidence(): [boolean, (next: boolean) => void] {
  const [show, setShow] = useState(false);
  useEffect(() => {
    let alive = true;
    const load = () => {
      loadSettings()
        .then((s) => {
          if (alive) setShow(s.showLowConfidence);
        })
        .catch(() => {});
    };
    const onChanged = (_changes: unknown, area: string) => {
      if (area === "sync") load();
    };
    load();
    browser.storage.onChanged.addListener(onChanged);
    return () => {
      alive = false;
      browser.storage.onChanged.removeListener(onChanged);
    };
  }, []);
  const update = (next: boolean) => {
    setShow(next);
    saveSettings({ showLowConfidence: next }).catch(() => setShow(!next));
  };
  return [show, update];
}

export function SidePanelApp(): JSX.Element {
  useThemeAttr(document.documentElement);
  const t = useT();
  const active = useActiveTab();
  const view = useTabView(active?.id ?? null).value;
  const host = useHostStatus();
  const [showLow, setShowLow] = useShowLowConfidence();
  const [pane, setPane] = useState<PaneKey>("resources");
  const [selection, setSelection] = useState<ListSelection>(EMPTY_SELECTION);
  const [siteOpen, setSiteOpen] = useState(false);
  const [sending, setSending] = useState(false);

  const selectedCount = selection.candidateIds.length + selection.resourceIds.length;
  const siteHost = view?.site.host ?? "";
  const dotClass = !host.status ? "sp-dot-checking" : host.status.connected ? "sp-dot-ok" : "sp-dot-off";
  const dotLabel = !host.status
    ? t("header.checking")
    : host.status.connected
      ? t("header.connected")
      : t("header.disconnected");

  const tabs: TabItem<PaneKey>[] = [
    { key: "resources", label: t("sidepanel.tab.resources"), count: view?.counts.all },
    { key: "links", label: t("sidepanel.tab.links") },
    { key: "tasks", label: t("sidepanel.tab.tasks") },
    { key: "diagnostics", label: t("sidepanel.tab.diagnostics") },
  ];

  const downloadSelected = async () => {
    if (!view || sending) return;
    setSending(true);
    try {
      const res = await downloadSelection(view, selection);
      if (res?.cancelled) showToast(t("remote.manualCancelled"));
      else if (res?.success) showToast(t("list.sent"), "success");
      else showToast(res?.message || t("list.sendFailed"), "error");
    } catch {
      showToast(t("list.sendFailed"), "error");
    } finally {
      setSending(false);
    }
  };

  return (
    <div class="sp-root">
      <header class="sp-header">
        <span class="sp-host" title={view?.pageUrl ?? active?.url ?? ""}>
          {siteHost || "FluxDown"}
        </span>
        <span class={`sp-dot ${dotClass}`} title={dotLabel} role="img" aria-label={dotLabel} />
      </header>

      <Tabs items={tabs} active={pane} onChange={setPane} />

      <div class="sp-body">
        <div class={`sp-pane${pane === "resources" ? "" : " fd-hidden"}`}>
          {view ? (
            <>
              <div class="sp-scroll">
                <HandoffCard handoff={view.handoff} tabId={view.tabId} />
                <div class="sp-site">
                  <button type="button" class="sp-site-toggle" onClick={() => setSiteOpen(!siteOpen)}>
                    <Icon name={siteOpen ? "chevronDown" : "chevronRight"} size={12} />
                    {t("sidepanel.siteSettings")}
                  </button>
                  {siteOpen ? <SiteCard host={siteHost} /> : null}
                </div>
                <ResourceList
                  view={view}
                  density="full"
                  showSearch
                  batchBar={false}
                  onSelectionChange={setSelection}
                />
              </div>
              {view.hiddenLowConfidence > 0 || showLow ? (
                <div class="sp-lowbar">
                  <span>
                    {showLow
                      ? t("sidepanel.showingLow")
                      : t("sidepanel.hiddenLow", { count: String(view.hiddenLowConfidence) })}
                  </span>
                  <button type="button" class="sp-link" onClick={() => setShowLow(!showLow)}>
                    {showLow ? t("sidepanel.hideLow") : t("sidepanel.showLow")}
                  </button>
                </div>
              ) : null}
              {selectedCount > 0 ? (
                <div class="sp-batchbar">
                  <span>{t("list.selected", { count: String(selectedCount) })}</span>
                  <Button variant="primary" size="sm" disabled={sending} onClick={downloadSelected}>
                    {t("panel.download")}
                  </Button>
                </div>
              ) : null}
            </>
          ) : (
            <EmptyState icon="search" title={t("sidepanel.noView")} />
          )}
        </div>

        {pane === "links" ? (
          <div class="sp-pane sp-scroll">
            {active ? (
              <LinkGrabber
                key={`${active.id}:${view?.pageUrl ?? active.url}`}
                tabId={active.id}
                pageUrl={view?.pageUrl ?? active.url}
              />
            ) : (
              <EmptyState icon="search" title={t("sidepanel.noView")} />
            )}
          </div>
        ) : null}

        {pane === "tasks" ? (
          <div class="sp-pane sp-scroll">
            <TaskList density="full" quickDownload />
          </div>
        ) : null}

        {pane === "diagnostics" ? (
          <div class="sp-pane sp-scroll">
            <DiagnosticsPane view={view} status={host.status} onRefresh={host.refresh} />
          </div>
        ) : null}
      </div>
      <ToastHost />
    </div>
  );
}
