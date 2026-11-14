//! 首次请求时才构建的外网 HTTPS 客户端。
//!
//! `reqwest` 构建客户端时同步加载系统根证书（macOS 走钥匙串，release 实测约 70ms；非 Windows
//! 经 [`crate::tls`] 全进程只加载一次，首个构建付这笔代价）。装配期逐个构建会把 UI Gateway
//! 开始服务整体推迟，冷启动时界面一直等着；这里把构建推迟到首个请求、放进阻塞线程池，同时到达的
//! 首批请求共享同一次构建，失败的构建下次请求重试。
//! 代理一律跟随系统设置（含 macOS 例外列表，见 [`crate::system_proxy`]），`configure` 不得自设代理。

use std::sync::Arc;

use tokio::sync::OnceCell;

/// 客户端构建失败。
#[derive(Debug, thiserror::Error)]
pub enum HttpClientError {
    #[error(transparent)]
    Build(#[from] reqwest::Error),
    #[error("HTTP client construction was interrupted: {0}")]
    Interrupted(#[from] tokio::task::JoinError),
}

type Configure = Arc<dyn Fn() -> reqwest::ClientBuilder + Send + Sync>;

/// 延迟构建的 `reqwest::Client`；`configure` 给出完整的构建参数。
pub struct LazyHttpClient {
    configure: Configure,
    client: OnceCell<reqwest::Client>,
}

impl LazyHttpClient {
    pub fn new(configure: impl Fn() -> reqwest::ClientBuilder + Send + Sync + 'static) -> Self {
        Self {
            configure: Arc::new(configure),
            client: OnceCell::new(),
        }
    }

    /// 已构建的客户端；首次调用在阻塞线程池里构建。
    pub async fn get(&self) -> Result<&reqwest::Client, HttpClientError> {
        self.client
            .get_or_try_init(|| async {
                let configure = Arc::clone(&self.configure);
                Ok(tokio::task::spawn_blocking(move || {
                    crate::system_proxy::follow_system(configure()).build()
                })
                .await??)
            })
            .await
    }
}
