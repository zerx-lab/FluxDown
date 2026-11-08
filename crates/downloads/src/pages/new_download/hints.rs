//! aria2c 命令头部信息回填（#299）：链接框里的 aria2c 命令带 `Cookie` / `User-Agent` /
//! `Referer` / 其他 `--header` 时，填进「高级」里对应的字段。
//!
//! 只在字段为空、或仍是上次自动回填的值（用户没碰过，命令编辑中随之更新）时才写，
//! 用户填过的内容绝不覆盖；命令里不再带该项时只清掉我们回填过的值。`Referer`
//! 回填「来源页」专门字段，与捕获的来源页共用一份回填记录（见
//! `NewDownloadView::sync_referrer`），不在此处理，也不作自定义请求头。判定规则见
//! [`crate::model::aria2c::plan_field_fill`]。

use gpui::{AppContext as _, Context, Window};
use gpui_component::input::InputState;

use super::NewDownloadView;
use crate::model::{
    aria2c::{FieldFill, RequestHints, plan_field_fill},
    new_download::{UA_PRESET_DEFAULT, detect_ua_preset},
};

/// 我们回填到某个自定义请求头行里的值。
struct FilledHeader {
    row_id: usize,
    name: String,
    value: String,
}

/// 头部回填状态。
#[derive(Default)]
pub(super) struct HintAutofill {
    /// 上一次处理过的头部信息：不变（如编辑其他行）时不重复处理，
    /// 用户删掉 / 清空回填值后不会在无关输入时被重新填回。
    applied: RequestHints,
    cookie: Option<String>,
    user_agent: Option<String>,
    headers: Vec<FilledHeader>,
}

impl NewDownloadView {
    /// 链接框变化后同步命令头部信息到表单字段。
    pub(super) fn sync_hints(
        &mut self,
        hints: RequestHints,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hints == self.hint_fill.applied {
            return;
        }
        let mut touched = false;

        let current = self.cookie.read(cx).value().to_string();
        match plan_field_fill(&current, self.hint_fill.cookie.as_deref(), &hints.cookie) {
            FieldFill::Keep => {}
            FieldFill::Forget => self.hint_fill.cookie = None,
            FieldFill::Set(value) => {
                self.cookie
                    .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                self.hint_fill.cookie = Some(value);
                touched = true;
            }
            FieldFill::Clear => {
                self.cookie
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.hint_fill.cookie = None;
            }
        }

        let current = self.user_agent.read(cx).value().to_string();
        match plan_field_fill(
            &current,
            self.hint_fill.user_agent.as_deref(),
            &hints.user_agent,
        ) {
            FieldFill::Keep => {}
            FieldFill::Forget => self.hint_fill.user_agent = None,
            FieldFill::Set(value) => {
                // `set_value` 不发 Change：预设下拉要手动对齐。
                self.ua_preset = detect_ua_preset(&value);
                self.user_agent
                    .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                self.hint_fill.user_agent = Some(value);
                touched = true;
            }
            FieldFill::Clear => {
                self.ua_preset = UA_PRESET_DEFAULT;
                self.user_agent
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.hint_fill.user_agent = None;
            }
        }

        touched |= self.sync_hint_headers(&hints, window, cx);
        self.hint_fill.applied = hints;
        if touched {
            // 回填的字段在折叠的「高级」里：展开让用户看得到发出去的内容。
            self.advanced_open = true;
        }
    }

    /// 自定义请求头行：已跟踪的行随命令更新 / 命令不再带时移除（值仍是回填值才动）；
    /// 未跟踪且用户没有同名行时新增一行；用户已有同名行不覆盖。返回是否写入了内容。
    fn sync_hint_headers(
        &mut self,
        hints: &RequestHints,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut touched = false;
        let mut tracked = std::mem::take(&mut self.hint_fill.headers);
        let mut next = Vec::with_capacity(hints.headers.len());
        for (name, value) in &hints.headers {
            if let Some(position) = tracked
                .iter()
                .position(|filled| filled.name.eq_ignore_ascii_case(name))
            {
                let mut filled = tracked.remove(position);
                let Some(row) = self.headers.iter().find(|row| row.id == filled.row_id) else {
                    // 用户删掉了回填的行：不重建，也不再认领。
                    next.push(filled);
                    continue;
                };
                let current = row.value.read(cx).value().to_string();
                match plan_field_fill(&current, Some(&filled.value), value) {
                    FieldFill::Keep => next.push(filled),
                    FieldFill::Forget | FieldFill::Clear => {}
                    FieldFill::Set(new_value) => {
                        row.value.update(cx, |input, cx| {
                            input.set_value(new_value.clone(), window, cx)
                        });
                        filled.value = new_value;
                        next.push(filled);
                        touched = true;
                    }
                }
                continue;
            }
            let exists = self
                .headers
                .iter()
                .any(|row| row.key.read(cx).value().trim().eq_ignore_ascii_case(name));
            if exists {
                continue;
            }
            let row_id = self.push_filled_header_row(name, value, window, cx);
            next.push(FilledHeader {
                row_id,
                name: name.clone(),
                value: value.clone(),
            });
            touched = true;
        }
        // 命令里已不再带的头：回填的行仍是原值才删，用户改过的保留。
        for filled in tracked {
            let Some(position) = self.headers.iter().position(|row| row.id == filled.row_id) else {
                continue;
            };
            let untouched = self.headers[position].value.read(cx).value().trim() == filled.value;
            if untouched {
                self.headers.remove(position);
            }
        }
        self.hint_fill.headers = next;
        touched
    }

    fn push_filled_header_row(
        &mut self,
        name: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        self.header_seq += 1;
        let id = self.header_seq;
        let key_hint = self.strings.header_name.clone();
        let value_hint = self.strings.header_value.clone();
        let (name, value) = (name.to_owned(), value.to_owned());
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(key_hint)
                .default_value(name)
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(value_hint)
                .default_value(value)
        });
        self.headers.push(super::HeaderRow { id, key, value });
        id
    }
}
