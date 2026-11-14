//! 可移植视图宿主：一个视图会话对应一个 `PluginViewHost` 实体。
//!
//! 会话由 daemon 持有；本实体只缓存最新的树与序号，帧不连续时用 snapshot 追平。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use fluxdown_protocol::{
    PluginViewEventParams, PluginViewFrameDto, PluginViewOpenParams, PluginViewOpenedDto,
    PluginViewSnapshotDto, VIEW_CLOSE_REASON_REQUESTED, ViewEffect, ViewEventDto, ViewNode,
    apply_view_ops,
};
use fluxdown_ui_i18n::Translator;
use gpui::{
    AnyElement, App, Context, Entity, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};
use gpui_component::{
    Sizable as _,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    spinner::Spinner,
};
use serde_json::{Value, json};

use super::{PortableSessions, render};
use crate::{PluginUiPort, PortFuture, ViewContainer};

/// 图片失败后的最大自动重试次数（首次请求之外）。
const MAX_IMAGE_RETRIES: u8 = 3;
const IMAGE_RETRY_BASE: Duration = Duration::from_secs(2);
/// 输入框 `change` 事件的去抖间隔（与 schema `debounceMs` 一致）。
pub(super) const INPUT_DEBOUNCE: Duration = Duration::from_millis(300);
/// 一次帧后仍保留「已发送但未被插件确认」的输入回显的最大帧数。
const SENT_INPUT_GRACE_FRAMES: u8 = 2;

/// 用户已输入、尚未被插件回显确认的输入框内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingInput {
    pub value: String,
    /// 去抖代数：只有最新一次输入的定时器才会发送。
    generation: u64,
    sent: bool,
    frames_since_sent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 已创建，尚未 `activate`。
    Idle,
    /// `view.open` 进行中。
    Opening,
    Open,
    /// 会话已结束（daemon 关闭 / 插件卸载 / 丢失），等待用户重新打开。
    Closed,
}

pub struct PluginViewHost {
    identity: String,
    view: String,
    task_id: Option<String>,
    container: ViewContainer,
    port: Arc<dyn PluginUiPort>,
    translator: Entity<Translator>,
    phase: Phase,
    session: Option<String>,
    seq: u64,
    tree: Option<ViewNode>,
    /// 每次树变化 +1，供渲染层缓存大表数据。
    revision: u64,
    error: Option<String>,
    images: HashMap<String, Arc<Image>>,
    image_loads: HashMap<(String, String), (u64, u8)>,
    /// 已尝试的重试次数与定时器代数；次数超过上限表示终止失败。
    image_retries: HashMap<String, (u8, u64)>,
    image_generation: u64,
    pending_inputs: HashMap<String, PendingInput>,
    input_generation: u64,
    resyncing: bool,
}

impl PluginViewHost {
    /// 只创建实体，不开会话；需要显示时调用 [`Self::activate`]。
    pub fn new(
        identity: String,
        view: String,
        task_id: Option<String>,
        container: ViewContainer,
        port: Arc<dyn PluginUiPort>,
        translator: Entity<Translator>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|host: &mut Self, cx: &mut App| host.release(cx))
            .detach();
        Self {
            container,
            identity,
            view,
            task_id,
            port,
            translator,
            phase: Phase::Idle,
            session: None,
            seq: 0,
            tree: None,
            revision: 0,
            error: None,
            images: HashMap::new(),
            image_loads: HashMap::new(),
            image_retries: HashMap::new(),
            image_generation: 0,
            pending_inputs: HashMap::new(),
            input_generation: 0,
            resyncing: false,
        }
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn view_id(&self) -> &str {
        &self.view
    }
    fn load_image(&mut self, id: String, cx: &mut Context<Self>) {
        self.load_image_attempt(id, 0, cx);
    }

    fn load_image_attempt(&mut self, id: String, prior_attempts: u8, cx: &mut Context<Self>) {
        if self.images.contains_key(&id)
            || (prior_attempts == 0
                && self
                    .image_retries
                    .get(&id)
                    .is_some_and(|(attempts, _)| *attempts > 0))
            || self
                .image_retries
                .get(&id)
                .is_some_and(|(attempts, _)| *attempts > MAX_IMAGE_RETRIES)
        {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let key = (session.clone(), id.clone());
        if self.images.len() + self.image_loads.len() >= 128 || self.image_loads.contains_key(&key)
        {
            return;
        }
        self.image_generation = self.image_generation.wrapping_add(1);
        let request_generation = self.image_generation;
        self.image_loads
            .insert(key.clone(), (request_generation, prior_attempts));
        let request_id = id.clone();
        let future = self.port.image(session.clone(), id);
        cx.spawn(async move |this, cx| {
            let result = future.await;
            if this
                .update(cx, |host, cx| {
                    let Some((generation, prior_attempts)) = host.image_loads.get(&key).copied()
                    else {
                        return;
                    };
                    if generation != request_generation {
                        return;
                    }
                    host.image_loads.remove(&key);
                    if host.session.as_deref() != Some(session.as_str())
                        || !host
                            .tree
                            .as_ref()
                            .is_some_and(|tree| tree_has_image(tree, &request_id))
                    {
                        return;
                    }
                    match result {
                        Ok(bytes)
                            if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                                && bytes.len() <= 4 * 1024 * 1024 =>
                        {
                            host.images.insert(
                                request_id.clone(),
                                Arc::new(Image::from_bytes(ImageFormat::Png, bytes)),
                            );
                            host.image_retries.remove(&request_id);
                        }
                        Ok(_) => {
                            host.image_retries
                                .insert(request_id, (MAX_IMAGE_RETRIES + 1, request_generation));
                        }
                        // The image port collapses transport and response failures into
                        // RpcErrorData; retry all of them, but with a strict exponential cap.
                        Err(_) => host.schedule_image_retry(
                            request_id.clone(),
                            session.clone(),
                            prior_attempts,
                            cx,
                        ),
                    }
                    cx.notify();
                })
                .is_err()
            {
                log::trace!("plugin image host released before fetched resource was applied");
            }
        })
        .detach();
    }
    fn schedule_image_retry(
        &mut self,
        id: String,
        session: String,
        prior_attempts: u8,
        cx: &mut Context<Self>,
    ) {
        let attempts = prior_attempts.saturating_add(1);
        self.image_generation = self.image_generation.wrapping_add(1);
        let retry_generation = self.image_generation;
        self.image_retries
            .insert(id.clone(), (attempts, retry_generation));
        if attempts > MAX_IMAGE_RETRIES {
            return;
        }
        let delay = IMAGE_RETRY_BASE.saturating_mul(1 << (attempts - 1));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            if this
                .update(cx, |host, cx| {
                    let retry_is_current =
                        host.image_retries.get(&id) == Some(&(attempts, retry_generation));
                    let still_in_tree = host.session.as_deref() == Some(session.as_str())
                        && host
                            .tree
                            .as_ref()
                            .is_some_and(|tree| tree_has_image(tree, &id));
                    if retry_is_current && still_in_tree {
                        host.load_image_attempt(id, attempts, cx);
                    }
                })
                .is_err()
            {
                log::trace!("plugin image retry host released before timer fired");
            }
        })
        .detach();
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn tree(&self) -> Option<&ViewNode> {
        self.tree.as_ref()
    }
    #[cfg(test)]
    pub(super) fn has_loaded_image(&self, id: &str) -> bool {
        self.images.contains_key(id)
    }

    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// 打开（或重新打开）会话：`daemon.plugin.view.open`。
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Opening) {
            return;
        }
        self.forget_session(cx);
        self.phase = Phase::Opening;
        self.error = None;
        self.tree = None;
        self.seq = 0;
        let locale = self.translator.read(cx).locale().to_owned();
        let params = PluginViewOpenParams {
            identity: self.identity.clone(),
            view: self.view.clone(),
            props: Value::Object(serde_json::Map::new()),
            task_id: self.task_id.clone(),
            locale: (!locale.is_empty()).then_some(locale),
        };
        let call = self.call("daemon.plugin.view.open", serde_json::to_value(&params));
        // open 进行中缓存未知会话的动作：插件在 init 里 close / navigate 的事件可能先于应答到达。
        PortableSessions::begin_open(cx);
        cx.spawn(async move |this, cx| {
            let result = decode!(call.await, PluginViewOpenedDto);
            if this
                .update(cx, |host, cx| host.on_opened(result, cx))
                .is_err()
            {
                log::debug!("plugin view host released before open finished");
            }
            cx.update(PortableSessions::end_open);
        })
        .detach();
        cx.notify();
    }

    fn on_opened(&mut self, result: Result<PluginViewOpenedDto, String>, cx: &mut Context<Self>) {
        match result {
            Ok(opened) => {
                self.phase = Phase::Open;
                self.session = Some(opened.session.clone());
                self.seq = opened.seq;
                self.tree = Some(opened.tree);
                self.after_tree_change(cx);
                let early = PortableSessions::register(opened.session, cx.weak_entity(), cx);
                if let Some(early) = early {
                    self.apply_early(early, cx);
                }
            }
            Err(message) => {
                self.phase = Phase::Closed;
                self.error = Some(message);
            }
        }
        cx.notify();
    }

    /// 执行 open 应答前已到达的动作：先动作、后关闭（与 daemon 发出的先后一致）。
    fn apply_early(&mut self, early: super::EarlyActions, cx: &mut Context<Self>) {
        for effect in &early.effects {
            self.run_effect(effect, cx);
        }
        if let Some(reason) = early.closed {
            self.mark_closed(&reason, cx);
        }
    }

    /// 应用 daemon 推来的帧。
    pub(super) fn apply_frame(&mut self, frame: PluginViewFrameDto, cx: &mut Context<Self>) {
        if self.session.as_deref() != Some(frame.session.as_str()) || self.tree.is_none() {
            return;
        }
        // 动作先于 ops 执行，且不受序号判定影响：动作帧沿用会话当前序号。
        for effect in &frame.effects {
            self.run_effect(effect, cx);
        }
        if frame.seq <= self.seq {
            return;
        }
        if frame.seq != self.seq + 1 {
            self.resync(false, cx);
            return;
        }
        let applied = match self.tree.as_mut() {
            Some(tree) => apply_view_ops(tree, &frame.ops),
            None => return,
        };
        match applied {
            Ok(()) => {
                self.seq = frame.seq;
                self.after_tree_change(cx);
                cx.notify();
            }
            Err(error) => {
                log::warn!("plugin view frame rejected: {error}");
                self.resync(false, cx);
            }
        }
    }

    /// 执行 daemon 随帧下发的宿主动作。
    fn run_effect(&self, effect: &ViewEffect, cx: &mut App) {
        match effect {
            ViewEffect::Navigate { target } => {
                // 插件那边 `ctx.navigate()` 已成功返回，这里失败只能留在日志里，用 warn 便于诊断。
                if let Err(message) = crate::navigate(cx, &self.identity, target) {
                    log::warn!(
                        "plugin view {}/{} navigate to {target} ignored: {message}",
                        self.identity,
                        self.view
                    );
                }
            }
            ViewEffect::Unknown => log::debug!(
                "plugin view {}/{} sent an action this client does not support",
                self.identity,
                self.view
            ),
        }
    }

    /// 重取 snapshot。`reopen_if_missing`：会话已不存在时重新 open（agent 重连后）。
    pub(super) fn resync(&mut self, reopen_if_missing: bool, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.resyncing {
            return;
        }
        self.resyncing = true;
        let call = self.call(
            "daemon.plugin.view.snapshot",
            Ok(json!({ "session": session })),
        );
        cx.spawn(async move |this, cx| {
            let result = decode!(call.await, PluginViewSnapshotDto);
            if this
                .update(cx, |host, cx| {
                    host.resyncing = false;
                    match result {
                        Ok(snapshot) => {
                            host.seq = snapshot.seq;
                            host.tree = Some(snapshot.tree);
                            host.after_tree_change(cx);
                            cx.notify();
                        }
                        Err(message) if reopen_if_missing => {
                            log::debug!("plugin view session lost, reopening: {message}");
                            host.phase = Phase::Closed;
                            host.activate(cx);
                        }
                        Err(message) => host.on_session_lost(message, cx),
                    }
                })
                .is_err()
            {
                log::debug!("plugin view host released before resync finished");
            }
        })
        .detach();
    }

    fn on_session_lost(&mut self, message: String, cx: &mut Context<Self>) {
        self.forget_session(cx);
        self.phase = Phase::Closed;
        self.error = Some(message);
        cx.notify();
    }

    /// daemon 宣布会话结束：显示「已结束」与重新打开按钮。
    pub(super) fn mark_closed(&mut self, reason: &str, cx: &mut Context<Self>) {
        self.forget_session(cx);
        log::debug!(
            "plugin view {}/{} closed: {reason}",
            self.identity,
            self.view
        );
        // 插件主动请求关闭（`ctx.close()`）且承载容器可关闭：直接关容器，不显示「已结束」。
        if reason == VIEW_CLOSE_REASON_REQUESTED && self.container.close(cx) {
            return;
        }
        self.phase = Phase::Closed;
        self.error = None;
        cx.notify();
    }

    fn forget_session(&mut self, cx: &mut App) {
        if let Some(session) = self.session.take() {
            PortableSessions::unregister(&session, cx);
        }
        self.images.clear();
        self.image_loads.clear();
        self.image_retries.clear();
        self.pending_inputs.clear();
        self.resyncing = false;
    }

    /// 实体释放：注销并通知 daemon 关闭会话。
    fn release(&mut self, cx: &mut App) {
        if let Some(session) = self.session.take() {
            PortableSessions::unregister(&session, cx);
            let call = self.call(
                "daemon.plugin.view.close",
                Ok(json!({ "session": session })),
            );
            cx.spawn(async move |_| {
                if let Err(error) = call.await {
                    log::debug!("plugin view close failed: {error:?}");
                }
            })
            .detach();
        }
    }

    fn after_tree_change(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.error = None;
        let Some(tree) = self.tree.as_ref() else {
            return;
        };
        let mut values = HashMap::new();
        let mut image_ids = HashSet::new();
        collect_values(tree, &mut values);
        collect_images(tree, &mut image_ids);
        self.images.retain(|id, _| image_ids.contains(id));
        self.image_retries.retain(|id, _| image_ids.contains(id));
        self.image_loads.retain(|(_, id), _| image_ids.contains(id));
        self.pending_inputs.retain(|node, pending| {
            if pending.sent {
                pending.frames_since_sent = pending.frames_since_sent.saturating_add(1);
            }
            let confirmed = values
                .get(node.as_str())
                .is_some_and(|v| *v == pending.value);
            !(confirmed || pending.frames_since_sent >= SENT_INPUT_GRACE_FRAMES)
        });
        for id in image_ids {
            self.load_image(id, cx);
        }
    }

    fn call(&self, method: &str, params: serde_json::Result<Value>) -> PortFuture<Value> {
        match params {
            Ok(value) => self.port.call(method.to_owned(), value),
            Err(error) => {
                log::warn!("plugin view params not serializable: {error}");
                Box::pin(async {
                    Err(fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::InvalidArgument,
                        false,
                    ))
                })
            }
        }
    }

    /// 用户操作 → `daemon.plugin.view.event`。
    pub(super) fn send_event(&mut self, node: &str, event: ViewEventDto, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let params = PluginViewEventParams {
            session,
            node: node.to_owned(),
            event,
        };
        let call = self.call("daemon.plugin.view.event", serde_json::to_value(&params));
        cx.spawn(async move |this, cx| {
            if let Err(error) = call.await
                && this
                    .update(cx, |host, cx| {
                        host.error = Some(error_text(&error));
                        cx.notify();
                    })
                    .is_err()
            {
                log::debug!("plugin view host released before event finished");
            }
        })
        .detach();
    }

    /// 输入框内容变化：记入 pending 并去抖发送 `change`。
    pub(super) fn input_changed(&mut self, node: &str, value: String, cx: &mut Context<Self>) {
        self.input_generation += 1;
        let generation = self.input_generation;
        self.pending_inputs.insert(
            node.to_owned(),
            PendingInput {
                value,
                generation,
                sent: false,
                frames_since_sent: 0,
            },
        );
        let node = node.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(INPUT_DEBOUNCE).await;
            if this
                .update(cx, |host, cx| host.flush_input(&node, Some(generation), cx))
                .is_err()
            {
                log::debug!("plugin view host released before input debounce fired");
            }
        })
        .detach();
    }

    /// 立即发送待发的输入（失焦 / 回车）。`generation` 为 `Some` 时只发送该代数。
    pub(super) fn flush_input(
        &mut self,
        node: &str,
        generation: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_inputs.get_mut(node) else {
            return;
        };
        if pending.sent || generation.is_some_and(|g| g != pending.generation) {
            return;
        }
        pending.sent = true;
        let value = pending.value.clone();
        self.send_event(
            node,
            ViewEventDto {
                kind: "change".to_owned(),
                value: Some(Value::String(value)),
                key: None,
                action: None,
            },
            cx,
        );
    }

    fn text(&self, key: &str, cx: &App) -> String {
        self.translator.read(cx).text(key).to_owned()
    }
}
fn collect_images(node: &ViewNode, out: &mut HashSet<String>) {
    if let Some(id) = node
        .p
        .get("src")
        .and_then(Value::as_str)
        .and_then(|src| src.strip_prefix("flux-image:"))
        .filter(|id| render::canonical_uuid(id))
    {
        out.insert(id.to_owned());
    }
    for child in &node.c {
        collect_images(child, out);
    }
}
fn tree_has_image(node: &ViewNode, id: &str) -> bool {
    node.p
        .get("src")
        .and_then(Value::as_str)
        .and_then(|src| src.strip_prefix("flux-image:"))
        == Some(id)
        || node.c.iter().any(|child| tree_has_image(child, id))
}

/// 节点 id → 其 `p.value` 字符串（input / textarea）。
fn collect_values<'a>(node: &'a ViewNode, out: &mut HashMap<&'a str, String>) {
    if matches!(node.k.as_str(), "input" | "textarea") {
        let value = node.p.get("value").and_then(Value::as_str).unwrap_or("");
        out.insert(node.id.as_str(), value.to_owned());
    }
    for child in &node.c {
        collect_values(child, out);
    }
}

pub(super) fn error_text(error: &fluxdown_protocol::RpcErrorData) -> String {
    serde_json::to_string(error).unwrap_or_else(|_| format!("{:?}", error.code))
}

/// RPC 结果 → 目标类型；失败为可显示的文本。
macro_rules! decode {
    ($result:expr, $ty:ty) => {{
        match $result {
            Ok(value) => serde_json::from_value::<$ty>(value).map_err(|error| error.to_string()),
            Err(error) => Err(error_text(&error)),
        }
    }};
}
use decode;

impl Render for PluginViewHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().id("plugin-view-root").size_full().min_w_0();
        let gap = fluxdown_ui_theme::active_theme(cx).tokens().spacing.sm;
        match self.phase {
            Phase::Idle | Phase::Opening => root
                .flex()
                .items_center()
                .justify_center()
                .gap(gap)
                .child(Spinner::new().small())
                .child(self.text("pluginViewLoading", cx))
                .into_any_element(),
            Phase::Closed => {
                let mut column = root.flex().flex_col().items_start().p(gap).gap(gap);
                let message = match &self.error {
                    Some(message) => self
                        .translator
                        .read(cx)
                        .text_with("pluginViewError", &[("message", message.as_str())]),
                    None => self.text("pluginViewClosed", cx),
                };
                column = column.child(message);
                let reopen = Button::new("plugin-view-reopen")
                    .label(self.text("pluginViewReopen", cx))
                    .primary()
                    .on_click(cx.listener(|host, _, _, cx| host.activate(cx)));
                column.child(reopen).into_any_element()
            }
            Phase::Open => self.render_open(root, window, cx),
        }
    }
}

impl PluginViewHost {
    fn render_open(
        &mut self,
        root: gpui::Stateful<gpui::Div>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tree) = self.tree.as_ref() else {
            return root.into_any_element();
        };
        let session = self.session.clone().unwrap_or_default();
        let weak = cx.weak_entity();
        let env = render::RenderEnv {
            host: &weak,
            session: &session,
            revision: self.revision,
            pending: &self.pending_inputs,
            images: &self.images,
        };
        let content = render::render_tree(tree, &env, window, cx);
        let spacing = fluxdown_ui_theme::active_theme(cx).tokens().spacing;
        let mut column = root.flex().flex_col().gap(spacing.sm).overflow_y_scroll();
        let tree_error = tree
            .p
            .get("__error")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty());
        let banner = tree_error.or(self.error.as_deref());
        if let Some(message) = banner {
            let text = self
                .translator
                .read(cx)
                .text_with("pluginViewError", &[("message", message)]);
            column = column.child(
                Alert::error(SharedString::from("plugin-view-error"), text)
                    .w_full()
                    .flex_none(),
            );
        }
        column
            .child(div().w_full().min_w_0().child(content))
            .into_any_element()
    }
}
