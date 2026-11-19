//! 链接预探测（#402）：链接框里恰好一条 http(s) 链接时，文本稳定后经 `daemon.link.probe`
//! （daemon 只发一次 HEAD，不消耗一次性令牌）提前显示文件名与大小。
//!
//! 自动模式和未修改的输入框显示当前探测名，但不把它作为覆盖名提交。手动草稿在切回自动时
//! 暂存，下次关闭自动时恢复。失败 / 超时 / 非下载资源一律只显示中性提示，不阻断提交。

use std::{collections::HashMap, time::Duration};

use fluxdown_protocol::{LinkProbeResult, LinkProbeVerdict, PendingCaptureDto};
use fluxdown_ui_components::field_hint;
use gpui::{App, Context, Div, SharedString, Task, Window};

use super::NewDownloadView;
use crate::{
    controller::{DownloadsCommand, DownloadsResult},
    model::new_download::UrlEntry,
};

/// 链接停止变化多久后才发探测（逐键输入时不逐字符发请求）。
const PROBE_DEBOUNCE: Duration = Duration::from_millis(500);
/// 请求发出后的兜底超时（daemon 自身 4s 总超时，这里防止通道异常时一直停在「探测中」）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// 单个表单内最多缓存的探测结果数；超过则整体清空（只在连续换链接时才会触及）。
const PROBE_CACHE_LIMIT: usize = 32;

/// 探测到（或捕获方提供）的文件信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProbeInfo {
    pub(super) file_name: String,
    /// 0 = 未知。
    pub(super) total_bytes: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum ProbeStatus {
    /// 无探测（或正在防抖等待）：不显示任何提示。
    #[default]
    Idle,
    Probing,
    Found(ProbeInfo),
    Unavailable,
}

/// 当前链接框内容对应的探测动作。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProbePlan {
    /// 不探测：条目数 ≠ 1，或不是 http(s) 链接（magnet / ed2k / thunder / ftp / 种子）。
    None,
    /// 外部捕获已带文件名 / 大小：沿用，不发请求。
    Capture(ProbeInfo),
    /// 探测该链接。
    Probe(String),
}

fn is_http_url(url: &str) -> bool {
    ["http://", "https://"].into_iter().any(|scheme| {
        url.len() >= scheme.len()
            && url.as_bytes()[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
    })
}

pub(super) fn probe_plan(entries: &[UrlEntry], captures: &[PendingCaptureDto]) -> ProbePlan {
    let [entry] = entries else {
        return ProbePlan::None;
    };
    if !is_http_url(&entry.url) {
        return ProbePlan::None;
    }
    if let Some(capture) = captures.iter().find(|capture| capture.url == entry.url) {
        let file_name = capture.file_name.trim();
        if !file_name.is_empty() || capture.file_size > 0 {
            return ProbePlan::Capture(ProbeInfo {
                file_name: file_name.to_owned(),
                total_bytes: capture.file_size.max(0),
            });
        }
    }
    ProbePlan::Probe(entry.url.clone())
}

/// daemon 探测结果 → 展示状态：只有确认是下载资源且至少拿到名称或大小才算成功。
fn status_from(result: &LinkProbeResult) -> ProbeStatus {
    if result.verdict != LinkProbeVerdict::Resource {
        return ProbeStatus::Unavailable;
    }
    let file_name = result.file_name.trim();
    let total_bytes = result.total_bytes.max(0);
    if file_name.is_empty() && total_bytes == 0 {
        ProbeStatus::Unavailable
    } else {
        ProbeStatus::Found(ProbeInfo {
            file_name: file_name.to_owned(),
            total_bytes,
        })
    }
}

/// 预探测状态。`generation` 每次目标变化递增，过期的异步结果据此丢弃。
pub(super) struct LinkProbe {
    generation: u64,
    /// 当前探测目标（仅 [`ProbePlan::Probe`]）；同一链接重复刷新不重复探测。
    target: Option<String>,
    status: ProbeStatus,
    /// 链接 → 已完成的探测结果（失败 / 通道错误不缓存，换回该链接会重试）。
    cache: HashMap<String, ProbeStatus>,
    /// 防抖 + 请求任务与超时任务；替换 / 清空即取消。
    tasks: Vec<Task<()>>,
    /// 「文件名」输入框当前生效的占位提示。
    placeholder: SharedString,
}

impl LinkProbe {
    pub(super) fn new(placeholder: SharedString) -> Self {
        Self {
            generation: 0,
            target: None,
            status: ProbeStatus::Idle,
            cache: HashMap::new(),
            tasks: Vec::new(),
            placeholder,
        }
    }

    pub(super) fn set_placeholder(&mut self, placeholder: SharedString) {
        self.placeholder = placeholder;
    }

    /// 作废进行中的探测并切到新状态。
    fn retarget(&mut self, target: Option<String>, status: ProbeStatus) {
        self.generation += 1;
        self.tasks.clear();
        self.target = target;
        self.status = status;
    }

    /// 链接自带的 `out=`（含浏览器捕获名）优先于探测名；不为批量链接预填名称。
    fn file_name<'a>(&'a self, entries: &'a [UrlEntry]) -> &'a str {
        let [entry] = entries else {
            return "";
        };
        if !entry.file_name.is_empty() {
            return &entry.file_name;
        }
        match &self.status {
            ProbeStatus::Found(info) => &info.file_name,
            _ => "",
        }
    }
}

impl NewDownloadView {
    /// 链接 / 捕获变化后同步探测：满足条件才（防抖后）发请求，其余情况清除状态。
    pub(super) fn sync_probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match probe_plan(&self.entries, &self.captures) {
            ProbePlan::None => {
                if self.probe.target.is_some() || self.probe.status != ProbeStatus::Idle {
                    self.probe.retarget(None, ProbeStatus::Idle);
                }
            }
            ProbePlan::Capture(info) => {
                if self.probe.target.is_some()
                    || self.probe.status != ProbeStatus::Found(info.clone())
                {
                    self.probe.retarget(None, ProbeStatus::Found(info));
                }
            }
            ProbePlan::Probe(url) => {
                if self.probe.target.as_deref() != Some(url.as_str()) {
                    match self.probe.cache.get(&url).cloned() {
                        Some(cached) => self.probe.retarget(Some(url), cached),
                        None => {
                            self.probe.retarget(Some(url.clone()), ProbeStatus::Idle);
                            self.spawn_probe(url, window, cx);
                        }
                    }
                }
            }
        }
        self.sync_rename(window, cx);
    }

    fn spawn_probe(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.probe.generation;
        let port = self.port.clone();
        let request_url = url.clone();
        let request = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PROBE_DEBOUNCE).await;
            let Ok(current) = this.update_in(cx, |this, _, cx| {
                let current = this.probe.generation == generation;
                if current {
                    this.probe.status = ProbeStatus::Probing;
                    cx.notify();
                }
                current
            }) else {
                // 表单已释放，不再回写。
                return;
            };
            if !current {
                return;
            }
            let outcome = match port
                .execute(DownloadsCommand::ProbeLink {
                    url: request_url.clone(),
                })
                .await
            {
                Ok(DownloadsResult::Value(value)) => {
                    serde_json::from_value::<LinkProbeResult>(value)
                        .ok()
                        .map(|result| status_from(&result))
                }
                _ => None,
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.finish_probe(generation, request_url, outcome, window, cx);
            }) else {
                return;
            };
        });
        let timeout = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(PROBE_DEBOUNCE + PROBE_TIMEOUT)
                .await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.probe.generation == generation && this.probe.status == ProbeStatus::Probing
                {
                    this.probe.status = ProbeStatus::Unavailable;
                    this.sync_rename(window, cx);
                    cx.notify();
                }
            }) else {
                return;
            };
        });
        self.probe.tasks = vec![request, timeout];
    }

    /// `outcome` 为 `None` = 请求失败（通道错误 / 响应无法解析）：显示中性提示但不缓存。
    fn finish_probe(
        &mut self,
        generation: u64,
        url: String,
        outcome: Option<ProbeStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.probe.generation != generation {
            return;
        }
        let status = match outcome {
            Some(status) => {
                if self.probe.cache.len() >= PROBE_CACHE_LIMIT {
                    self.probe.cache.clear();
                }
                self.probe.cache.insert(url, status.clone());
                status
            }
            None => ProbeStatus::Unavailable,
        };
        self.probe.status = status;
        self.sync_rename(window, cx);
        cx.notify();
    }

    /// 「文件名」输入框的占位：有探测名时显示探测名（用户清空预填值后仍能看到自动识别结果），
    /// 否则回到默认的「自动识别文件名」。
    pub(super) fn rename_placeholder(&self) -> SharedString {
        let detected = self.download_name.detected();
        if detected.is_empty() {
            self.strings.rename_placeholder.clone()
        } else {
            detected.clone()
        }
    }

    /// 提交用的文件名：仍是未改动的预填探测名时视为空（交给引擎定名，与不预填时行为一致）。
    pub(super) fn rename_value(&self, cx: &App) -> String {
        self.download_name
            .override_name(self.rename.read(cx))
            .unwrap_or_default()
    }

    pub(super) fn set_auto_name(
        &mut self,
        auto: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.download_name.set_auto(auto, &self.rename, window, cx);
        self.sync_category(window, cx);
        cx.notify();
    }

    /// 探测结果变化后同步「文件名」输入框：占位提示 + 预填可编辑的探测名。
    pub(super) fn sync_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detected = self.probe.file_name(&self.entries);
        self.download_name
            .set_detected(detected, &self.rename, window, cx);
        let placeholder = self.rename_placeholder();
        if placeholder != self.probe.placeholder {
            self.probe.set_placeholder(placeholder.clone());
            self.rename.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        // 预填 / 清空探测名会改变自动分类的预测输入。
        self.sync_category(window, cx);
    }

    /// 链接输入区下方的一行状态：探测中 / 已识别（文件名 + 大小，0 = 未知）/ 无法获取。
    pub(super) fn render_probe_status(&self, cx: &App) -> Option<Div> {
        let text = match &self.probe.status {
            ProbeStatus::Idle => return None,
            ProbeStatus::Probing => self.strings.probe_checking.clone(),
            ProbeStatus::Found(info) => self
                .strings
                .format_probe_found(&info.file_name, info.total_bytes),
            ProbeStatus::Unavailable => self.strings.probe_unavailable.clone(),
        };
        Some(field_hint(text, cx))
    }
}

#[cfg(test)]
mod tests {
    use std::{rc::Rc, sync::Arc};

    use fluxdown_protocol::{
        ApplicationErrorCode, CategoryRouteDto, CategoryRuleDto, LinkProbeResult, LinkProbeVerdict,
        PendingCaptureDto, RpcErrorData,
    };
    use fluxdown_ui_i18n::I18nCatalog;
    use gpui::{App, AppContext as _, Context, TestApp, Window};

    use super::{NewDownloadView, ProbeInfo, ProbePlan, ProbeStatus, probe_plan, status_from};
    use crate::{
        controller::{DownloadsCommand, DownloadsPort, DownloadsResult, PortFuture},
        model::new_download::{UrlEntry, build_requests},
    };

    fn entry(url: &str) -> UrlEntry {
        UrlEntry {
            url: url.to_owned(),
            ..UrlEntry::default()
        }
    }

    fn capture(url: &str, file_name: &str, file_size: i64) -> PendingCaptureDto {
        PendingCaptureDto {
            transaction_id: "t".to_owned(),
            url: url.to_owned(),
            file_name: file_name.to_owned(),
            created_at_unix_ms: 0,
            file_size,
            referrer: String::new(),
            save_dir: String::new(),
            has_cookies: false,
            header_names: Vec::new(),
        }
    }

    #[test]
    fn probe_refills_a_blank_manual_name_and_preserves_a_custom_draft() {
        const URL: &str = "https://example.com/download.zip";
        const DRAFT: &str = " My file.zip ";

        struct Offline;

        impl DownloadsPort for Offline {
            fn execute(&self, _: DownloadsCommand) -> PortFuture<DownloadsResult> {
                Box::pin(async { Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)) })
            }
        }

        fn assert_name(view: &NewDownloadView, cx: &App, visible: &str, submitted: &str) {
            assert_eq!(&*view.rename.read(cx).value(), visible);
            let built = build_requests(&view.entries, &view.draft_options(false, None, cx));
            let item = built.items.first().expect("single download request");
            assert_eq!(item.file_name, submitted);
        }

        fn apply_probe(
            view: &mut NewDownloadView,
            name: &str,
            window: &mut Window,
            cx: &mut Context<NewDownloadView>,
        ) {
            view.probe
                .retarget(Some(URL.to_owned()), ProbeStatus::Probing);
            view.finish_probe(
                view.probe.generation,
                URL.to_owned(),
                Some(ProbeStatus::Found(ProbeInfo {
                    file_name: name.to_owned(),
                    total_bytes: 100,
                })),
                window,
                cx,
            );
        }

        let mut app = TestApp::new();
        let translator = app.update(|cx| {
            gpui_component::init(cx);
            fluxdown_ui_theme::init(cx);
            let catalog = I18nCatalog::load_embedded().expect("catalog");
            cx.new(|_| catalog.translator("en"))
        });
        let mut win = app.open_window(|window, cx| {
            NewDownloadView::new(
                translator,
                Default::default(),
                Arc::new(Offline),
                Rc::new(|_, _, _| {}),
                window,
                cx,
            )
        });
        win.update(|view, window, cx| {
            view.context.category_route = Some(CategoryRouteDto {
                rules: vec![CategoryRuleDto {
                    match_mode: "extension".to_owned(),
                    extensions: vec!["pdf".to_owned()],
                    save_dir: "/downloads/documents".to_owned(),
                    ..Default::default()
                }],
                other_dir: "/downloads/other".to_owned(),
                ..Default::default()
            });
            view.entries = vec![entry(URL)];
            view.set_auto_name(false, window, cx);
            view.rename
                .update(cx, |input, cx| input.set_value("Report.pdf", window, cx));
            apply_probe(view, "Report.pdf", window, cx);
            assert_name(view, cx, "Report.pdf", "Report.pdf");

            view.entries = vec![entry("https://example.com/other.zip")];
            view.probe.cache.insert(
                "https://example.com/other.zip".to_owned(),
                ProbeStatus::Unavailable,
            );
            view.sync_probe(window, cx);
            assert_name(view, cx, "Report.pdf", "Report.pdf");

            view.set_auto_name(true, window, cx);
            view.entries = vec![entry(URL)];
            apply_probe(view, "Detected.zip", window, cx);
            assert_name(view, cx, "Detected.zip", "");

            view.set_auto_name(false, window, cx);
            assert_name(view, cx, "Detected.zip", "");
            view.rename
                .update(cx, |input, cx| input.set_value(DRAFT, window, cx));
            assert_name(view, cx, DRAFT, "My file.zip");

            view.set_auto_name(true, window, cx);
            assert_name(view, cx, "Detected.zip", "");
            apply_probe(view, "Latest.zip", window, cx);
            assert_name(view, cx, "Latest.zip", "");

            view.set_auto_name(false, window, cx);
            assert_name(view, cx, DRAFT, "My file.zip");
            apply_probe(view, "New.pdf", window, cx);
            assert_name(view, cx, DRAFT, "My file.zip");
            assert_eq!(&*view.save_dir.read(cx).value(), "/downloads/other");

            for blank in ["", " \t "] {
                view.rename
                    .update(cx, |input, cx| input.set_value(blank, window, cx));
                apply_probe(view, "Updated.pdf", window, cx);
                assert_name(view, cx, "Updated.pdf", "");
                assert_eq!(&*view.save_dir.read(cx).value(), "/downloads/documents");
            }
        });
    }

    #[test]
    fn plan_probes_only_a_single_http_link() {
        let one = [entry("https://a.example/x.zip")];
        assert_eq!(
            probe_plan(&one, &[]),
            ProbePlan::Probe("https://a.example/x.zip".to_owned())
        );
        assert_eq!(
            probe_plan(&[entry("HTTP://a.example/x")], &[]),
            ProbePlan::Probe("HTTP://a.example/x".to_owned())
        );
        assert_eq!(probe_plan(&[], &[]), ProbePlan::None);
        assert_eq!(
            probe_plan(
                &[entry("https://a.example/1"), entry("https://a.example/2")],
                &[]
            ),
            ProbePlan::None
        );
        for url in [
            "magnet:?xt=urn:btih:aa",
            "ed2k://|file|x|1|ff|/",
            "thunder://QUFodHRw",
            "ftp://a.example/x",
        ] {
            assert_eq!(probe_plan(&[entry(url)], &[]), ProbePlan::None, "{url}");
        }
    }

    #[test]
    fn plan_reuses_capture_hints_instead_of_probing() {
        let url = "https://a.example/x.zip";
        assert_eq!(
            probe_plan(&[entry(url)], &[capture(url, " x.zip ", 0)]),
            ProbePlan::Capture(ProbeInfo {
                file_name: "x.zip".to_owned(),
                total_bytes: 0
            })
        );
        assert_eq!(
            probe_plan(&[entry(url)], &[capture(url, "", 1024)]),
            ProbePlan::Capture(ProbeInfo {
                file_name: String::new(),
                total_bytes: 1024
            })
        );
        // 捕获没带任何提示 / 是别的链接：照常探测。
        assert_eq!(
            probe_plan(&[entry(url)], &[capture(url, "", 0)]),
            ProbePlan::Probe(url.to_owned())
        );
        assert_eq!(
            probe_plan(&[entry(url)], &[capture("https://b.example/y", "y.zip", 5)]),
            ProbePlan::Probe(url.to_owned())
        );
    }

    #[test]
    fn only_resources_with_name_or_size_count_as_found() {
        let result = |verdict, file_name: &str, total_bytes| LinkProbeResult {
            verdict,
            final_url: String::new(),
            file_name: file_name.to_owned(),
            mime: String::new(),
            total_bytes,
        };
        assert_eq!(
            status_from(&result(LinkProbeVerdict::Resource, " a.iso ", 2048)),
            ProbeStatus::Found(ProbeInfo {
                file_name: "a.iso".to_owned(),
                total_bytes: 2048
            })
        );
        assert_eq!(
            status_from(&result(LinkProbeVerdict::Resource, "", 0)),
            ProbeStatus::Unavailable
        );
        assert_eq!(
            status_from(&result(LinkProbeVerdict::Resource, "a.iso", -1)),
            ProbeStatus::Found(ProbeInfo {
                file_name: "a.iso".to_owned(),
                total_bytes: 0
            })
        );
        for verdict in [LinkProbeVerdict::NotResource, LinkProbeVerdict::Unknown] {
            assert_eq!(
                status_from(&result(verdict, "a.iso", 9)),
                ProbeStatus::Unavailable
            );
        }
    }
}
