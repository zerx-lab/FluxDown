//! 「推介有奖」设置页：说明 / 我的推荐码 / 收益记录三个分段标签。
//!
//! 视图由 app 作为设置页插槽注入；是否出现在设置侧栏由 app 依「已登录 + 设备本地偏好
//! `ui.show_settings_referral`」决定（见 `AccountHost::referral_page_visible`）。
//! 数据全部经 `agent.referral.*` RPC 拉取，状态都在本 entity 内并 notify。

mod dialogs;
mod render;

use std::time::Duration;

use fluxdown_protocol::{
    CloudReferralCodesResult, CloudReferralRecordsResult, CloudReferralSummary, RpcErrorData,
    method,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_i18n::Translator;
use gpui::{AppContext as _, ClipboardItem, Context, Entity, SharedString, Subscription, Window};
use gpui_component::{
    WindowExt as _,
    input::{InputEvent, InputState},
};

use crate::billing::{self, MAX_REFERRAL_CODES};
use crate::errors::{ErrorContext, error_text};
use crate::host::AccountHost;
use crate::rpc::decode;
use crate::{AccountCommand, t};

/// 推荐码 / 收益记录每页条数。
pub(crate) const PAGE_SIZE: u32 = 20;
/// 收益记录搜索防抖。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Tab {
    Intro,
    Codes,
    Records,
}

impl Tab {
    pub(crate) const ALL: [Self; 3] = [Self::Intro, Self::Codes, Self::Records];

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Intro => "accountReferralTabIntro",
            Self::Codes => "accountReferralTabCode",
            Self::Records => "accountReferralTabRecords",
        }
    }
}

/// 一段远程数据的加载状态。
pub(crate) enum Load<T> {
    /// 尚未请求（页面首次可见时触发）。
    Idle,
    Loading,
    Failed(SharedString),
    Ready(T),
}

impl<T> Load<T> {
    fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

pub(crate) struct CodesState {
    pub page: u32,
    pub load: Load<CloudReferralCodesResult>,
    /// 正在删除的推荐码 id（禁用其按钮）。
    pub deleting: Option<String>,
}

pub(crate) struct RecordsState {
    pub page: u32,
    /// 已生效的搜索词（防抖之后）。
    pub term: String,
    pub load: Load<CloudReferralRecordsResult>,
}

pub struct ReferralView {
    pub(crate) host: Entity<AccountHost>,
    pub(crate) tab: Tab,
    pub(crate) summary: Load<CloudReferralSummary>,
    pub(crate) codes: CodesState,
    pub(crate) records: RecordsState,
    pub(crate) search_input: Entity<InputState>,
    /// 当前账号 id：切换账号 / 登出后丢弃旧数据。
    session_key: Option<String>,
    summary_gen: u64,
    codes_gen: u64,
    records_gen: u64,
    search_gen: u64,
    _search_subscription: Subscription,
}

impl ReferralView {
    pub fn new(host: Entity<AccountHost>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t(
            host.read(cx).translator().read(cx),
            "accountReferralSearchPlaceholder",
        );
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()));
        let _search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this: &mut Self, _, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_search(window, cx);
                }
            },
        );
        cx.observe_in(&host, window, |this: &mut Self, host, window, cx| {
            let key = host
                .read(cx)
                .controller
                .session()
                .map(|session| session.user.id.clone());
            if key != this.session_key {
                this.reset(key, window, cx);
            }
            cx.notify();
        })
        .detach();
        // 翻译切换（含搜索框占位）。
        let translator = host.read(cx).translator().clone();
        cx.observe_in(
            &translator,
            window,
            |this: &mut Self, translator, window, cx| {
                let placeholder = translator
                    .read(cx)
                    .text("accountReferralSearchPlaceholder")
                    .to_owned();
                this.search_input.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
                cx.notify();
            },
        )
        .detach();
        let session_key = host
            .read(cx)
            .controller
            .session()
            .map(|session| session.user.id.clone());
        Self {
            host,
            tab: Tab::Intro,
            summary: Load::Idle,
            codes: CodesState {
                page: 1,
                load: Load::Idle,
                deleting: None,
            },
            records: RecordsState {
                page: 1,
                term: String::new(),
                load: Load::Idle,
            },
            search_input,
            session_key,
            summary_gen: 0,
            codes_gen: 0,
            records_gen: 0,
            search_gen: 0,
            _search_subscription,
        }
    }

    /// 账号变化：丢弃全部数据与在途请求，回到「说明」标签。
    fn reset(&mut self, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.session_key = key;
        self.tab = Tab::Intro;
        self.summary = Load::Idle;
        self.codes = CodesState {
            page: 1,
            load: Load::Idle,
            deleting: None,
        };
        self.records = RecordsState {
            page: 1,
            term: String::new(),
            load: Load::Idle,
        };
        self.summary_gen += 1;
        self.codes_gen += 1;
        self.records_gen += 1;
        self.search_gen += 1;
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(crate) fn translator(&self, cx: &gpui::App) -> Translator {
        self.host.read(cx).translator().read(cx).clone()
    }

    pub(crate) fn text(&self, key: &str, cx: &gpui::App) -> SharedString {
        t(self.host.read(cx).translator().read(cx), key)
    }

    pub(crate) fn error_text(&self, error: &RpcErrorData, cx: &gpui::App) -> SharedString {
        error_text(
            self.host.read(cx).translator().read(cx),
            error,
            ErrorContext::General,
        )
    }

    pub(crate) fn disconnected(&self, cx: &gpui::App) -> bool {
        self.host.read(cx).controller.is_stale()
    }

    /// 推介活动是否开启（摘要未就绪时按开启处理，避免加载期闪现禁用提示）。
    pub(crate) fn enabled(&self) -> bool {
        match &self.summary {
            Load::Ready(summary) => summary.enabled,
            _ => true,
        }
    }

    /// 创建推荐码是否允许：活动开启、已加载且未满额、在线。
    pub(crate) fn can_create_code(&self, cx: &gpui::App) -> bool {
        self.enabled()
            && !self.disconnected(cx)
            && matches!(&self.codes.load, Load::Ready(codes) if !codes_full(codes))
    }

    /// 页面可见时首次触发加载：摘要 + 当前标签所需数据。
    pub(crate) fn ensure_loaded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_key.is_none() {
            return;
        }
        if self.summary.is_idle() {
            self.load_summary(window, cx);
        }
        match self.tab {
            Tab::Codes if self.codes.load.is_idle() => self.load_codes(window, cx),
            Tab::Records if self.records.load.is_idle() => self.load_records(window, cx),
            _ => {}
        }
    }

    pub(crate) fn set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.ensure_loaded(window, cx);
            cx.notify();
        }
    }

    // ───────────────────────── 加载 ─────────────────────────

    pub(crate) fn load_summary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.summary_gen += 1;
        let generation = self.summary_gen;
        self.summary = Load::Loading;
        cx.notify();
        let future = self.host.read(cx).port().execute(AccountCommand::Referral {
            method: method::AGENT_REFERRAL_SUMMARY,
            params: serde_json::json!({}),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match future.await {
                Ok(value) => decode!(value => CloudReferralSummary),
                Err(error) => Err(error),
            };
            let Ok(()) = this.update(cx, |this, cx| {
                if this.summary_gen != generation {
                    return;
                }
                this.summary = match result {
                    Ok(summary) => Load::Ready(summary),
                    Err(error) => Load::Failed(this.error_text(&error, cx)),
                };
                cx.notify();
            }) else {
                // 页面已释放，停止回写。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn load_codes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.codes_gen += 1;
        let generation = self.codes_gen;
        let page = self.codes.page;
        self.codes.load = Load::Loading;
        cx.notify();
        let future = self.host.read(cx).port().execute(AccountCommand::Referral {
            method: method::AGENT_REFERRAL_LIST_CODES,
            params: serde_json::json!({ "page": page, "pageSize": PAGE_SIZE }),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match future.await {
                Ok(value) => decode!(value => CloudReferralCodesResult),
                Err(error) => Err(error),
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.codes_gen != generation {
                    return;
                }
                match result {
                    // 删除最后一页的最后一项后回退一页。
                    Ok(codes) if codes.items.is_empty() && this.codes.page > 1 => {
                        this.codes.page -= 1;
                        this.load_codes(window, cx);
                        return;
                    }
                    Ok(codes) => this.codes.load = Load::Ready(codes),
                    Err(error) => this.codes.load = Load::Failed(this.error_text(&error, cx)),
                }
                cx.notify();
            }) else {
                // 页面已释放，停止回写。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn load_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.records_gen += 1;
        let generation = self.records_gen;
        self.records.load = Load::Loading;
        cx.notify();
        let mut params = serde_json::json!({
            "page": self.records.page,
            "pageSize": PAGE_SIZE,
        });
        if !self.records.term.is_empty() {
            params["search"] = serde_json::Value::String(self.records.term.clone());
        }
        let future = self.host.read(cx).port().execute(AccountCommand::Referral {
            method: method::AGENT_REFERRAL_LIST_RECORDS,
            params,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match future.await {
                Ok(value) => decode!(value => CloudReferralRecordsResult),
                Err(error) => Err(error),
            };
            let Ok(()) = this.update(cx, |this, cx| {
                if this.records_gen != generation {
                    return;
                }
                this.records.load = match result {
                    Ok(records) => Load::Ready(records),
                    Err(error) => Load::Failed(this.error_text(&error, cx)),
                };
                cx.notify();
            }) else {
                // 页面已释放，停止回写。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn go_codes_page(&mut self, page: u32, window: &mut Window, cx: &mut Context<Self>) {
        if page >= 1 && page != self.codes.page {
            self.codes.page = page;
            self.load_codes(window, cx);
        }
    }

    pub(crate) fn go_records_page(
        &mut self,
        page: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if page >= 1 && page != self.records.page {
            self.records.page = page;
            self.load_records(window, cx);
        }
    }

    /// 搜索框输入防抖：300ms 内无新输入才以新词回到第 1 页重查。
    fn schedule_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_gen += 1;
        let generation = self.search_gen;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.search_gen != generation {
                    return;
                }
                let term = this.search_input.read(cx).value().trim().to_owned();
                if term == this.records.term {
                    return;
                }
                this.records.term = term;
                this.records.page = 1;
                this.load_records(window, cx);
            }) else {
                // 页面已释放，停止防抖回调。
                return;
            };
        })
        .detach();
    }

    // ───────────────────────── 动作 ─────────────────────────

    pub(crate) fn copy_code(&self, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(code.to_owned()));
        window.push_notification(
            toast::success(self.text("accountReferralCodeCopied", cx)),
            cx,
        );
    }

    pub(crate) fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_create_code(cx) {
            dialogs::open_create(&self.host, cx.entity().downgrade(), window, cx);
        }
    }

    pub(crate) fn confirm_delete(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disconnected(cx) || self.codes.deleting.is_some() {
            return;
        }
        dialogs::confirm_delete(&self.host, cx.entity().downgrade(), id, window, cx);
    }

    pub(crate) fn set_deleting(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.codes.deleting = id;
        cx.notify();
    }
}

/// 推荐码是否已达每人上限（按服务端总数判断）。
pub(crate) fn codes_full(codes: &CloudReferralCodesResult) -> bool {
    usize::try_from(codes.total).is_ok_and(|total| total >= MAX_REFERRAL_CODES)
}

/// 总页数（至少 1）。
pub(crate) fn page_count(total: i64) -> u32 {
    billing::page_count(total, PAGE_SIZE)
}
