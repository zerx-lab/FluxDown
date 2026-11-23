/**
 * 页面脚本面板：列出桌面端插件声明的页面脚本；每个插件首次启用需经确认对话框（明确危害），
 * 确认后在同一用户手势内请求 `userScripts` 可选权限，再引导打开浏览器开关。
 * 已启用的插件可「停用并移除」或只读查看源码（按需从桌面端拉取，不落 storage）。
 */
import type { JSX } from "preact";
import { useCallback, useEffect, useRef, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import type { PageScriptPluginView, PageScriptsStatus } from "@/background/page-scripts/model";
import { Badge } from "@/ui/components/Badge";
import { Button } from "@/ui/components/Button";
import { Icon } from "@/ui/components/Icon";
import { Modal } from "@/ui/components/Modal";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import { sendAction } from "@/utils/messaging";
import {
  ERROR_LABEL,
  extensionDetailsUrl,
  matchHosts,
  STATE_LABEL,
  STATE_TONE,
  switchGuide,
} from "../lib/page-scripts";
import { ConfirmModal, PanelShell } from "../parts";
import { logError } from "@/utils/error-log";

/** 需要浏览器侧配合（权限 / 开关）才能工作的状态：回到本页时自动重新检查。 */
function needsBrowserSetup(status: PageScriptsStatus | null): boolean {
  return status?.plugins.some((p) => p.state === "switch-off" || p.state === "permission-missing") === true;
}

function usePageScriptsStatus(): {
  status: PageScriptsStatus | null;
  setStatus: (status: PageScriptsStatus) => void;
  recheck: () => Promise<void>;
} {
  const [status, setStatus] = useState<PageScriptsStatus | null>(null);
  const latest = useRef<PageScriptsStatus | null>(null);
  latest.current = status;

  const recheck = useCallback(async () => {
    try {
      setStatus(await sendAction<"pageScriptsStatus">({ action: "pageScriptsStatus", refresh: "force" }));
    } catch (e) {
      logError("[FluxDown Options] pageScriptsStatus failed:", e);
    }
  }, []);

  useEffect(() => {
    void recheck();
    // 用户去浏览器详情页打开开关后回到本页：自动重新检查。
    const onFocus = () => {
      if (needsBrowserSetup(latest.current)) void recheck();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [recheck]);

  return { status, setStatus, recheck };
}

/** 插件脚本匹配的全部站点（去重，保持声明顺序）。 */
function matchesOf(plugin: PageScriptPluginView): string[] {
  return [...new Set(plugin.scripts.flatMap((script) => script.matches))];
}

function EnableModal(props: {
  plugin: PageScriptPluginView;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}): JSX.Element {
  const t = useT();
  const { plugin } = props;
  return (
    <Modal
      title={t("pageScripts.enableTitle", { name: plugin.name })}
      onClose={props.onCancel}
      width={520}
      footer={
        <>
          <Button onClick={props.onCancel}>{t("options.cancel")}</Button>
          <Button variant="danger" disabled={props.busy} onClick={props.onConfirm}>
            {t("pageScripts.enableConfirm")}
          </Button>
        </>
      }
    >
      <div class="opt-confirm">
        <p>{t("pageScripts.warnIntro")}</p>
        <ul class="opt-ps-warn">
          <li>{t("pageScripts.warnRead")}</li>
          <li>{t("pageScripts.warnAct")}</li>
          <li>{t("pageScripts.warnAuto")}</li>
          <li>
            <strong>{t("pageScripts.warnTrust")}</strong>
          </li>
        </ul>
        <div class="opt-ps-matches-title">{t("pageScripts.matchesLabel")}</div>
        <ul class="opt-ps-matches">
          {matchesOf(plugin).map((pattern) => (
            <li key={pattern} class="opt-cell-mono">
              {pattern}
            </li>
          ))}
        </ul>
      </div>
    </Modal>
  );
}

function SourceModal(props: { plugin: PageScriptPluginView; scriptId: string; onClose: () => void }): JSX.Element {
  const t = useT();
  const script = props.plugin.scripts.find((item) => item.id === props.scriptId);
  const [state, setState] = useState<{ code: string; sha256: string } | { error: string } | null>(null);

  useEffect(() => {
    let alive = true;
    sendAction<"pageScriptsSource">({ action: "pageScriptsSource", identity: props.plugin.identity, id: props.scriptId })
      .then((result) => {
        if (!alive) return;
        setState(result.ok ? { code: result.code, sha256: result.sha256 } : { error: result.message });
      })
      .catch((e) => {
        if (alive) setState({ error: String(e) });
      });
    return () => {
      alive = false;
    };
  }, [props.plugin.identity, props.scriptId]);

  return (
    <Modal
      title={t("pageScripts.sourceTitle", { title: script?.title ?? props.scriptId })}
      onClose={props.onClose}
      width={720}
      footer={<Button onClick={props.onClose}>{t("ui.close")}</Button>}
    >
      {state === null ? (
        <div class="opt-row-desc">{t("pageScripts.sourceLoading")}</div>
      ) : "error" in state ? (
        <div class="opt-error-text">{t("pageScripts.sourceFailed", { message: state.error })}</div>
      ) : (
        <>
          <div class="opt-row-desc opt-cell-mono">{t("pageScripts.sourceSha", { sha: state.sha256 })}</div>
          <pre class="opt-ps-source" tabIndex={0}>
            {state.code}
          </pre>
        </>
      )}
    </Modal>
  );
}

function SwitchGuide(): JSX.Element | null {
  const t = useT();
  const guide = switchGuide(navigator.userAgent);
  if (guide === "none") return null;
  return (
    <div class="opt-card">
      <h2 class="opt-card-title">{t("pageScripts.switchTitle")}</h2>
      <p class="opt-row-desc">
        {guide === "details-page" ? t("pageScripts.switchChrome138") : t("pageScripts.switchChromeLegacy")}
      </p>
      {guide === "details-page" ? (
        <Button
          variant="primary"
          size="sm"
          onClick={() => {
            void browser.tabs.create({ url: extensionDetailsUrl(navigator.userAgent, browser.runtime.id) });
          }}
        >
          <Icon name="external" size={13} />
          {t("pageScripts.switchOpenDetails")}
        </Button>
      ) : null}
    </div>
  );
}

/** 摘要里直接列出的站点个数；其余折成「等 N 个网站」，完整规则进详情弹窗。 */
const SITES_PREVIEW = 2;

function SitesSummary(props: { matches: string[] }): JSX.Element {
  const t = useT();
  const hosts = matchHosts(props.matches);
  const shown = hosts.slice(0, SITES_PREVIEW).join(t("pageScripts.sitesSeparator"));
  const text =
    hosts.length > SITES_PREVIEW ? t("pageScripts.sitesSummary", { sites: shown, count: String(hosts.length) }) : shown;
  return (
    <div class="opt-ps-sites" title={hosts.join("\n")}>
      {text}
    </div>
  );
}

function DetailModal(props: {
  plugin: PageScriptPluginView;
  scriptId: string;
  onClose: () => void;
  onViewSource: (plugin: PageScriptPluginView, scriptId: string) => void;
}): JSX.Element | null {
  const t = useT();
  const script = props.plugin.scripts.find((item) => item.id === props.scriptId);
  if (!script) return null;
  return (
    <Modal
      title={script.title}
      onClose={props.onClose}
      width={560}
      footer={
        <>
          {props.plugin.approved ? (
            <Button onClick={() => props.onViewSource(props.plugin, script.id)}>{t("pageScripts.viewSource")}</Button>
          ) : null}
          <Button variant="primary" onClick={props.onClose}>
            {t("ui.close")}
          </Button>
        </>
      }
    >
      <dl class="opt-ps-detail">
        <dt>{t("pageScripts.colWorld")}</dt>
        <dd>{script.world === "main" ? t("pageScripts.worldMain") : t("pageScripts.worldIsolated")}</dd>
        <dt>{t("pageScripts.matchRules", { count: String(script.matches.length) })}</dt>
        <dd>
          <ul class="opt-ps-matches opt-ps-matches-tall">
            {script.matches.map((pattern) => (
              <li key={pattern} class="opt-cell-mono">
                {pattern}
              </li>
            ))}
          </ul>
        </dd>
      </dl>
    </Modal>
  );
}

function PluginCard(props: {
  plugin: PageScriptPluginView;
  disabled: boolean;
  onEnable: (plugin: PageScriptPluginView) => void;
  onGrant: (plugin: PageScriptPluginView) => void;
  onDisable: (plugin: PageScriptPluginView) => void;
  onDetails: (plugin: PageScriptPluginView, scriptId: string) => void;
}): JSX.Element {
  const t = useT();
  const { plugin } = props;
  const error = plugin.error;
  return (
    <div class="opt-card">
      <div class="opt-card-head">
        <div>
          <h2 class="opt-card-title">{plugin.name}</h2>
          <div class="opt-row-desc">
            {plugin.identity}
            {plugin.version ? ` · ${t("pageScripts.version", { version: plugin.version })}` : ""}
          </div>
        </div>
        <div class="opt-status-cell">
          <Badge tone={STATE_TONE[plugin.state]}>{t(STATE_LABEL[plugin.state])}</Badge>
          {!plugin.approved ? (
            <Button variant="primary" size="sm" disabled={props.disabled} onClick={() => props.onEnable(plugin)}>
              {t("pageScripts.enable")}
            </Button>
          ) : plugin.state === "permission-missing" ? (
            <Button variant="primary" size="sm" disabled={props.disabled} onClick={() => props.onGrant(plugin)}>
              {t("pageScripts.grantPermission")}
            </Button>
          ) : null}
          {plugin.approved ? (
            <Button variant="danger" size="sm" disabled={props.disabled} onClick={() => props.onDisable(plugin)}>
              {t("pageScripts.disable")}
            </Button>
          ) : null}
        </div>
      </div>

      {error ? (
        <p class="opt-error-text">
          {t(ERROR_LABEL[error.code], { detail: error.detail ?? "" })}
          {error.scriptId ? ` (${error.scriptId})` : ""}
        </p>
      ) : null}
      {!plugin.provided ? <p class="opt-row-desc">{t("pageScripts.notProvided")}</p> : null}

      {plugin.scripts.length > 0 ? (
        <div class="opt-table-wrap">
          <table class="opt-table opt-table-compact opt-ps-table">
            <colgroup>
              <col class="opt-ps-col-script" />
              <col />
              <col class="opt-ps-col-actions" />
            </colgroup>
            <thead>
              <tr>
                <th>{t("pageScripts.colScript")}</th>
                <th>{t("pageScripts.colMatches")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {plugin.scripts.map((script) => (
                <tr key={script.id}>
                  <td>
                    <div class="opt-rule-title">{script.title}</div>
                    <div class="opt-rule-id opt-cell-mono">{script.id}</div>
                  </td>
                  <td>
                    <SitesSummary matches={script.matches} />
                  </td>
                  <td class="opt-col-actions">
                    <Button size="sm" variant="ghost" onClick={() => props.onDetails(plugin, script.id)}>
                      {t("pageScripts.details")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}

export function PageScriptsPanel(): JSX.Element {
  const t = useT();
  const { status, setStatus, recheck } = usePageScriptsStatus();
  const [enabling, setEnabling] = useState<PageScriptPluginView | null>(null);
  const [disabling, setDisabling] = useState<PageScriptPluginView | null>(null);
  const [source, setSource] = useState<{ plugin: PageScriptPluginView; scriptId: string } | null>(null);
  const [detail, setDetail] = useState<{ plugin: PageScriptPluginView; scriptId: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const permissionMissing = status?.availability === "permission-missing";

  /** 记录批准（后台生成令牌并立即同步）。 */
  const approve = async (plugin: PageScriptPluginView): Promise<void> => {
    const result = await sendAction<"pageScriptsApprove">({ action: "pageScriptsApprove", identity: plugin.identity });
    setStatus(result.status);
    showToast(
      result.ok ? t("pageScripts.enabledToast") : t("pageScripts.enableFailed", { message: result.message ?? "" }),
      result.ok ? "success" : "error",
    );
  };

  /**
   * 确认对话框的「启用」：`permissions.request` 必须在用户手势的同步调用栈内发起，
   * 所以第一件事就是调用它（之前不能有任何 await）。
   */
  const confirmEnable = (plugin: PageScriptPluginView): void => {
    const request = permissionMissing ? browser.permissions.request({ permissions: ["userScripts"] }) : null;
    setBusy(true);
    void (async () => {
      try {
        if (request && !(await request)) {
          showToast(t("pageScripts.permissionDenied"), "error");
          return;
        }
        await approve(plugin);
      } catch (e) {
        showToast(t("pageScripts.enableFailed", { message: String(e) }), "error");
      } finally {
        setBusy(false);
        setEnabling(null);
      }
    })();
  };

  /** 已批准但权限被撤销：直接在点击手势里重新请求权限。 */
  const grantPermission = (): void => {
    const request = browser.permissions.request({ permissions: ["userScripts"] });
    setBusy(true);
    void (async () => {
      try {
        if (!(await request)) showToast(t("pageScripts.permissionDenied"), "error");
        await recheck();
      } catch (e) {
        showToast(t("pageScripts.enableFailed", { message: String(e) }), "error");
      } finally {
        setBusy(false);
      }
    })();
  };

  const confirmDisable = async (plugin: PageScriptPluginView): Promise<void> => {
    setBusy(true);
    try {
      const result = await sendAction<"pageScriptsRevoke">({ action: "pageScriptsRevoke", identity: plugin.identity });
      setStatus(result.status);
      showToast(result.ok ? t("pageScripts.disabledToast") : (result.message ?? ""), result.ok ? "success" : "error");
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy(false);
      setDisabling(null);
    }
  };

  const switchOff = status?.plugins.some((p) => p.state === "switch-off") === true;

  return (
    <PanelShell title={t("options.nav.pageScripts")} desc={t("pageScripts.desc")}>
      <div class="opt-card-head">
        <span />
        <Button size="sm" onClick={() => void recheck()}>
          <Icon name="refresh" size={13} />
          {t("pageScripts.recheck")}
        </Button>
      </div>

      {status?.blocked === "remote-mode" ? <p class="opt-error-text">{t("pageScripts.blockedRemote")}</p> : null}
      {status?.blocked === "host-unsupported" ? <p class="opt-error-text">{t("pageScripts.blockedHost")}</p> : null}
      {status?.availability === "browser-unsupported" ? (
        <p class="opt-error-text">{t("pageScripts.unsupported")}</p>
      ) : null}
      {permissionMissing && status?.plugins.some((p) => p.approved) ? (
        <p class="opt-row-desc opt-row-note">{t("pageScripts.permissionMissing")}</p>
      ) : null}
      {switchOff ? <SwitchGuide /> : null}

      {status === null ? (
        <div class="opt-row-desc">{t("pageScripts.loading")}</div>
      ) : status.plugins.length === 0 ? (
        <div class="opt-row-desc">{t("pageScripts.empty")}</div>
      ) : (
        status.plugins.map((plugin) => (
          <PluginCard
            key={plugin.identity}
            plugin={plugin}
            disabled={busy || status.blocked !== null || status.availability === "browser-unsupported"}
            onEnable={setEnabling}
            onGrant={grantPermission}
            onDisable={setDisabling}
            onDetails={(p, scriptId) => setDetail({ plugin: p, scriptId })}
          />
        ))
      )}

      {enabling ? (
        <EnableModal
          plugin={enabling}
          busy={busy}
          onConfirm={() => confirmEnable(enabling)}
          onCancel={() => setEnabling(null)}
        />
      ) : null}
      {disabling ? (
        <ConfirmModal
          title={t("pageScripts.disableTitle", { name: disabling.name })}
          message={<p>{t("pageScripts.disableMessage")}</p>}
          confirmLabel={t("pageScripts.disable")}
          danger
          onConfirm={() => void confirmDisable(disabling)}
          onCancel={() => setDisabling(null)}
        />
      ) : null}
      {detail ? (
        <DetailModal
          plugin={detail.plugin}
          scriptId={detail.scriptId}
          onClose={() => setDetail(null)}
          onViewSource={(p, scriptId) => {
            setDetail(null);
            setSource({ plugin: p, scriptId });
          }}
        />
      ) : null}
      {source ? (
        <SourceModal plugin={source.plugin} scriptId={source.scriptId} onClose={() => setSource(null)} />
      ) : null}
    </PanelShell>
  );
}
