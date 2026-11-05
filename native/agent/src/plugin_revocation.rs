//! 已装插件的撤回定期触发。
//!
//! - 公开市场：daemon 在 `daemon.plugin.marketList` 拉到索引时会把被标记 vulnerable / malicious 的
//!   已装插件版本强制禁用；本模块只负责在后台低频触发一次这条链路。
//! - 私有商店：存在 `source=private` 的已装插件且已登录时，经 [`PrivatePluginService::sync_revocations`]
//!   拉 FluxCloud 私有目录，把 malicious 撤回清单交 daemon。
//!
//! 触发：启动后一次 + 固定周期（`interval` 是**持久**计时器，会话事件不会重置它）+ 登录身份变化。
//! 云客户端的会话 watch 只在登录（持久化新会话）与会话清除（退出 / 刷新令牌被拒 / 远端撤销）时递增，
//! 令牌刷新不通知；即便如此这里仍记录上次登录的用户 id，只有「未登录 → 已登录」或换号才补一次私有同步。
//! 无私有插件或未登录时不发任何请求；daemon 只在撤回实际生效时才写盘。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{ApplicationErrorCode, PluginDto, RpcErrorData, method};
use serde_json::Value;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::daemon_client::DaemonClient;
use crate::private_plugin::PrivatePluginService;

/// 启动后的首次等待：避开启动高峰。
const INITIAL_DELAY: Duration = Duration::from_secs(120);
/// 之后的检查间隔。
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 循环依赖的外部动作（真实实现 = daemon + 私有商店服务；测试用假实现）。
#[async_trait::async_trait]
trait Revoker: Send + Sync {
    async fn installed(&self) -> Result<Vec<PluginDto>, RpcErrorData>;
    async fn market_list(&self) -> Result<(), RpcErrorData>;
    async fn private_sync(&self) -> Result<(), RpcErrorData>;
    /// 当前登录用户 id；未登录 `None`。
    async fn user_id(&self) -> Option<String>;
    fn session_changes(&self) -> watch::Receiver<u64>;
}

struct Live {
    daemon: Arc<DaemonClient>,
    private: Arc<PrivatePluginService>,
}

#[async_trait::async_trait]
impl Revoker for Live {
    async fn installed(&self) -> Result<Vec<PluginDto>, RpcErrorData> {
        self.daemon
            .call::<Value, Vec<PluginDto>>(method::DAEMON_PLUGIN_LIST, None)
            .await
    }
    async fn market_list(&self) -> Result<(), RpcErrorData> {
        self.daemon
            .call::<Value, Value>(method::DAEMON_PLUGIN_MARKET_LIST, None)
            .await
            .map(|_| ())
    }
    async fn private_sync(&self) -> Result<(), RpcErrorData> {
        self.private.sync_revocations().await
    }
    async fn user_id(&self) -> Option<String> {
        self.private.current_user_id().await
    }
    fn session_changes(&self) -> watch::Receiver<u64> {
        self.private.session_changes()
    }
}

/// 启动后台撤回检查任务（桌面 / server 宿主；embedded 不调用）。
pub fn spawn(
    daemon: Arc<DaemonClient>,
    private: Arc<PrivatePluginService>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let live = Live { daemon, private };
        run(&live, INITIAL_DELAY, CHECK_INTERVAL, cancel).await;
    });
}

async fn run(
    revoker: &dyn Revoker,
    initial: Duration,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut session = Some(revoker.session_changes());
    if let Some(receiver) = session.as_mut() {
        receiver.borrow_and_update();
    }
    if !sleep_or_cancel(initial, &cancel).await {
        return;
    }
    // 先标记「已看到」再取身份：此后的登录变化一定会唤醒下面的分支。首轮完整检查覆盖启动期间的登录 / 恢复。
    if let Some(receiver) = session.as_mut() {
        receiver.borrow_and_update();
    }
    let mut last_user = revoker.user_id().await;
    if !check_once(revoker, Scope::All).await {
        return;
    }
    // 持久计时器：会话事件只让 select 的另一支运行，不会重建 / 重置这里的 deadline。
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {
                if !check_once(revoker, Scope::All).await {
                    return;
                }
            }
            changed = session_changed(&mut session) => {
                if changed.is_err() {
                    // 云客户端已释放：会话不会再变化，摘掉该分支，退化为纯周期。
                    session = None;
                    continue;
                }
                let user = revoker.user_id().await;
                let new_identity = user.is_some() && user != last_user;
                last_user = user;
                if new_identity && !check_once(revoker, Scope::PrivateOnly).await {
                    return;
                }
            }
        }
    }
}

/// 会话 watch 的下一次变化；分支已摘掉（`None`）时永远挂起。
async fn session_changed(
    session: &mut Option<watch::Receiver<u64>>,
) -> Result<(), watch::error::RecvError> {
    match session {
        Some(receiver) => receiver.changed().await,
        None => std::future::pending().await,
    }
}

/// 返回 `false` 表示被取消。
async fn sleep_or_cancel(duration: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(duration) => true,
    }
}

/// 一轮检查的范围：周期检查两路都看；登录身份变化后只补私有撤回（公开路径不受登录影响）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    PrivateOnly,
}

/// 执行一轮检查；返回 `false` 表示 daemon 不支持插件，应结束循环。
async fn check_once(revoker: &dyn Revoker, scope: Scope) -> bool {
    let plugins = match revoker.installed().await {
        Ok(plugins) => plugins,
        Err(error) if error.code == ApplicationErrorCode::Unsupported => return false,
        Err(error) => {
            tracing::debug!(code = ?error.code, "plugin revocation check: list failed");
            return true;
        }
    };
    if scope == Scope::All
        && has_marketable_plugin(&plugins)
        && let Err(error) = revoker.market_list().await
    {
        tracing::debug!(code = ?error.code, "plugin revocation check: market list failed");
    }
    if has_private_plugin(&plugins)
        && let Err(error) = revoker.private_sync().await
    {
        tracing::debug!(code = ?error.code, "plugin revocation check: private sync failed");
    }
    true
}

/// 存在非 dev 的私有商店来源插件时才值得联网（未登录时 `sync_revocations` 自己静默）。
fn has_private_plugin(plugins: &[PluginDto]) -> bool {
    plugins
        .iter()
        .any(|plugin| !plugin.dev_mode && plugin.source == "private")
}

/// 只有存在非 dev 且非私有商店来源的已装插件时才值得联网拉公开索引（私有包的撤回走
/// `agent.privatePlugin.list`，公开撤回不处理它们）。
fn has_marketable_plugin(plugins: &[PluginDto]) -> bool {
    plugins
        .iter()
        .any(|plugin| !plugin.dev_mode && plugin.source.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{has_marketable_plugin, has_private_plugin};
    use fluxdown_protocol::PluginDto;

    fn dto(dev_mode: bool) -> PluginDto {
        PluginDto {
            identity: "a@b".into(),
            name: "a".into(),
            version: "1.0.0".into(),
            description: String::new(),
            homepage: String::new(),
            enabled: true,
            dev_mode,
            disabled_reason: "None".into(),
            settings: Vec::new(),
            settings_values: std::collections::HashMap::new(),
            permissions: Vec::new(),
            auth_supported: false,
            subscription_provider_ids: Vec::new(),
            resolver_urls: Vec::new(),
            load_status: "Loaded".into(),
            load_error: String::new(),
            contributions: fluxdown_protocol::PluginContributionsDto::default(),
            risk: "standard".to_owned(),
            consent_pending: Vec::new(),
            consent_pending_hosts: Vec::new(),
            network_hosts: Vec::new(),
            package_dir: None,
            package_revision: 0,
            source: String::new(),
        }
    }

    #[test]
    fn only_dev_plugins_skip_market_fetch() {
        assert!(!has_marketable_plugin(&[]));
        assert!(!has_marketable_plugin(&[dto(true)]));
        assert!(has_marketable_plugin(&[dto(true), dto(false)]));
    }

    #[test]
    fn private_source_plugins_do_not_trigger_the_public_fetch() {
        let mut private = dto(false);
        private.source = "private".into();
        assert!(!has_marketable_plugin(&[private.clone()]));
        assert!(has_marketable_plugin(&[private, dto(false)]));
    }

    #[test]
    fn only_installed_private_plugins_trigger_the_private_sync() {
        let mut private = dto(false);
        private.source = "private".into();
        assert!(!has_private_plugin(&[]));
        assert!(!has_private_plugin(&[dto(false)]));
        assert!(has_private_plugin(&[private.clone()]));
        private.dev_mode = true;
        assert!(!has_private_plugin(&[private]));
    }

    // ---- 循环行为（真实时钟、毫秒级间隔；未启用 tokio test-util，不用暂停时钟）----

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use fluxdown_protocol::RpcErrorData;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use super::{Revoker, run};

    struct Fake {
        plugins: Vec<PluginDto>,
        user: Mutex<Option<String>>,
        session: Mutex<Option<watch::Sender<u64>>>,
        market_calls: AtomicUsize,
        private_calls: AtomicUsize,
    }

    impl Fake {
        fn new(user: Option<&str>) -> Arc<Self> {
            let mut private = dto(false);
            private.source = "private".into();
            Arc::new(Self {
                plugins: vec![dto(false), private],
                user: Mutex::new(user.map(str::to_owned)),
                session: Mutex::new(Some(watch::channel(0).0)),
                market_calls: AtomicUsize::new(0),
                private_calls: AtomicUsize::new(0),
            })
        }

        /// 模拟云客户端的会话 watch 通知（登录 / 清除时递增）。
        fn notify(&self) {
            if let Some(sender) = self.session.lock().unwrap().as_ref() {
                sender.send_modify(|generation| *generation += 1);
            }
        }

        fn drop_sender(&self) {
            self.session.lock().unwrap().take();
        }

        fn set_user(&self, user: Option<&str>) {
            *self.user.lock().unwrap() = user.map(str::to_owned);
        }

        fn market(&self) -> usize {
            self.market_calls.load(Ordering::SeqCst)
        }

        fn private(&self) -> usize {
            self.private_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Revoker for Fake {
        async fn installed(&self) -> Result<Vec<PluginDto>, RpcErrorData> {
            Ok(self.plugins.clone())
        }
        async fn market_list(&self) -> Result<(), RpcErrorData> {
            self.market_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn private_sync(&self) -> Result<(), RpcErrorData> {
            self.private_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn user_id(&self) -> Option<String> {
            self.user.lock().unwrap().clone()
        }
        fn session_changes(&self) -> watch::Receiver<u64> {
            match self.session.lock().unwrap().as_ref() {
                Some(sender) => sender.subscribe(),
                None => watch::channel(0).1,
            }
        }
    }

    fn start(
        fake: &Arc<Fake>,
        interval: Duration,
    ) -> (CancellationToken, tokio::task::JoinHandle<()>) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let fake = Arc::clone(fake);
            let cancel = cancel.clone();
            async move { run(fake.as_ref(), Duration::ZERO, interval, cancel).await }
        });
        (cancel, task)
    }

    async fn until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn session_notifications_never_postpone_the_periodic_check() {
        let fake = Fake::new(Some("a"));
        let (cancel, task) = start(&fake, Duration::from_millis(120));
        // 比周期密得多的同用户会话通知（如 token 刷新类通知）：周期计时器不得被重置。
        for _ in 0..14 {
            fake.notify();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        cancel.cancel();
        task.await.unwrap();
        assert!(
            fake.market() >= 3,
            "periodic checks starved: {}",
            fake.market()
        );
        // 同一用户的通知不触发额外的私有同步：私有同步次数 = 完整检查次数。
        assert_eq!(fake.private(), fake.market());
    }

    #[tokio::test]
    async fn only_a_new_login_identity_triggers_the_private_sync() {
        let fake = Fake::new(Some("a"));
        let (cancel, task) = start(&fake, Duration::from_secs(3600));
        until("initial check", || fake.private() == 1).await;

        // 同用户重复通知：忽略。
        fake.notify();
        fake.notify();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(fake.private(), 1);

        // 换号：触发恰好一次。
        fake.set_user(Some("b"));
        fake.notify();
        until("account switch", || fake.private() == 2).await;
        fake.notify();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(fake.private(), 2);

        // 登出不触发；再次登录触发。
        fake.set_user(None);
        fake.notify();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(fake.private(), 2);
        fake.set_user(Some("b"));
        fake.notify();
        until("login after logout", || fake.private() == 3).await;

        // 登录后的补同步不碰公开索引。
        assert_eq!(fake.market(), 1);
        cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_dropped_session_sender_degrades_to_the_periodic_loop() {
        let fake = Fake::new(Some("a"));
        let (cancel, task) = start(&fake, Duration::from_millis(40));
        until("initial check", || fake.market() >= 1).await;
        // 发送端释放：receiver.changed() 立即返回 Err，循环必须摘掉该分支而不是空转。
        fake.drop_sender();
        until("periodic checks continue", || fake.market() >= 4).await;
        // 若 Err 分支被反复命中会在一轮里刷出大量私有同步；退化为纯周期时两者同步增长。
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(fake.private(), fake.market());
    }
}
