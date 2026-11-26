/** 链接抓取：按扩展名分组勾选、自定义扩展名、文本过滤，发送所选到 FluxDown（downloadUrls）。 */
import type { JSX } from "preact";
import { useEffect, useMemo, useState } from "preact/hooks";
import type { ClassifiedLink } from "@/background/protocol";
import type { ResourceType } from "@/utils/resource-types";
import { extractExtension } from "@/rules/url";
import { sendAction } from "@/utils/messaging";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { TagInput } from "@/ui/components/TagInput";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import "./features.css";

type Group = "video" | "audio" | "archive" | "document" | "image" | "executable" | "other";
const GROUPS: Group[] = ["video", "audio", "archive", "document", "image", "executable", "other"];

function groupOf(type: ResourceType): Group {
  if (type === "stream") return "video";
  if (type === "video" || type === "audio" || type === "archive" || type === "document" || type === "image" || type === "executable") {
    return type;
  }
  return "other";
}

export function LinkGrabber(props: { tabId: number; pageUrl: string }): JSX.Element {
  const t = useT();
  const [links, setLinks] = useState<ClassifiedLink[] | null>(null);
  const [groups, setGroups] = useState<Set<Group>>(new Set(["video", "audio", "archive", "document"]));
  const [customExts, setCustomExts] = useState<string[]>([]);
  const [text, setText] = useState("");
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const load = async () => {
    setLinks(null);
    try {
      const res = await sendAction<"collectLinks">({ action: "collectLinks", tabId: props.tabId });
      setLinks(res?.links ?? []);
    } catch {
      setLinks([]);
    }
    setExcluded(new Set());
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.tabId, props.pageUrl]);

  const filtered = useMemo(() => {
    if (!links) return [];
    const needle = text.trim().toLowerCase();
    return links.filter((l) => {
      if (needle && !l.url.toLowerCase().includes(needle) && !l.text.toLowerCase().includes(needle)) return false;
      const ext = extractExtension(l.url, []);
      if (customExts.length > 0 && ext && customExts.includes(ext)) return true;
      return groups.has(groupOf(l.type));
    });
  }, [links, groups, customExts, text]);

  const selected = filtered.filter((l) => !excluded.has(l.url));

  const send = async () => {
    if (selected.length === 0) return;
    setBusy(true);
    try {
      const urls = selected.map((l) => l.url);
      const res = await sendAction<"downloadUrls">({
        action: "downloadUrls",
        urls,
        referrer: props.pageUrl,
        tabId: props.tabId,
      });
      if (res?.cancelled) showToast(t("remote.manualCancelled"));
      else if (res?.success) showToast(t("links.sent", { count: String(urls.length) }), "success");
      else showToast(res?.message || t("list.sendFailed"), "error");
    } catch {
      showToast(t("list.sendFailed"), "error");
    } finally {
      setBusy(false);
    }
  };

  const toggleGroup = (g: Group, on: boolean) =>
    setGroups((prev) => {
      const next = new Set(prev);
      if (on) next.add(g);
      else next.delete(g);
      return next;
    });

  return (
    <div class="fd-links">
      <div class="fd-links-groups">
        {GROUPS.map((g) => (
          <label class="fd-chip" key={g}>
            <input type="checkbox" checked={groups.has(g)} onChange={(e) => toggleGroup(g, (e.currentTarget as HTMLInputElement).checked)} />
            {t(`links.group.${g}`)}
          </label>
        ))}
      </div>
      <TagInput
        tags={customExts}
        onChange={setCustomExts}
        placeholder={t("links.customExt")}
        normalize={(raw) => raw.replace(/^\./, "").toLowerCase()}
      />
      <div class="fd-links-bar">
        <input
          class="fd-input"
          type="search"
          placeholder={t("links.filter")}
          value={text}
          onInput={(e) => setText((e.currentTarget as HTMLInputElement).value)}
        />
        <Button size="sm" onClick={() => void load()}>
          {t("links.refresh")}
        </Button>
      </div>
      <div class="fd-links-bar">
        <label class="fd-check-all">
          <input
            type="checkbox"
            checked={filtered.length > 0 && selected.length === filtered.length}
            onChange={(e) => setExcluded((e.currentTarget as HTMLInputElement).checked ? new Set() : new Set(filtered.map((l) => l.url)))}
          />
          {t("panel.selectAll")} ({selected.length}/{filtered.length})
        </label>
        <Button size="sm" variant="primary" disabled={busy || selected.length === 0} onClick={() => void send()}>
          {t("links.sendSelected")}
        </Button>
      </div>
      {links === null ? (
        <div class="fd-empty">{t("header.checking")}</div>
      ) : filtered.length === 0 ? (
        <EmptyState icon="link" title={t("links.empty")} />
      ) : (
        <div class="fd-links-list">
          {filtered.map((l) => (
            <label class="fd-link-row" key={l.url} title={l.url}>
              <input
                type="checkbox"
                checked={!excluded.has(l.url)}
                onChange={(e) =>
                  setExcluded((prev) => {
                    const next = new Set(prev);
                    if ((e.currentTarget as HTMLInputElement).checked) next.delete(l.url);
                    else next.add(l.url);
                    return next;
                  })
                }
              />
              <span class="fd-link-main">
                <span class="fd-link-text">{l.text || l.url}</span>
                <span class="fd-link-url">{l.url}</span>
              </span>
            </label>
          ))}
        </div>
      )}
    </div>
  );
}
