//! Proxy configuration module.
//!
//! Provides the [`ProxyConfig`] type that holds user/system proxy settings and
//! helper functions for:
//! - Building proxy URLs for reqwest (`to_proxy_url`)
//! - Detecting the system proxy: Windows registry, macOS `scutil --proxy`,
//!   Linux `https_proxy`/`http_proxy`/`all_proxy` env vars falling back to
//!   GNOME `gsettings org.gnome.system.proxy`
//! - Parsing a Windows `ProxyServer` registry value (multi-protocol format)

use std::collections::HashMap;

use crate::downloader::DownloadError;
use crate::logger::log_info;

// ---------------------------------------------------------------------------
// Proxy mode / type enums
// ---------------------------------------------------------------------------

/// How the application resolves proxy settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyMode {
    /// No proxy — direct connection (default).
    None,
    /// Use OS-level proxy (Windows registry, environment variables).
    System,
    /// User-specified proxy address.
    Manual,
    /// 自动决策：任务直连启动，运行中确认慢且候选代理（系统代理，
    /// 缺省回退手动字段）经采样证明显著更快时才按 host 热切换。
    /// 决策粒度是**任务级**（见 [`crate::auto_proxy`]）；本模式值本身
    /// 绝不直接进 client 构建——[`ProxyConfig::resolve`] 与
    /// `build_client_inner` 都把它折算为直连，具体代理由 auto_proxy
    /// 的决策路径显式给出 Manual 配置。
    Auto,
}

impl ProxyMode {
    /// 与标准库 `FromStr::from_str` 同名会引发 clippy `should_implement_trait`——
    /// 此函数语义不同(无法识别的输入回退到默认值,而非返回 `Err`),故用
    /// `parse_str` 命名以示区分。
    pub fn parse_str(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "manual" => Self::Manual,
            "auto" => Self::Auto,
            _ => Self::None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::System => "system",
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

/// Supported proxy protocol types.
///
/// Every variant names the **proxy endpoint's own transport**, never the
/// destination protocol — the same axis `Socks4`/`Socks5` sit on, matching the
/// single-select control in the settings UI:
///
/// - [`Self::Http`] — plaintext endpoint. Reaching an HTTPS destination through
///   it uses `CONNECT` tunnelling, so this is the correct choice for mixed
///   HTTP/SOCKS ports such as Clash's 7897 or a plain Squid.
/// - [`Self::Https`] — the endpoint itself is wrapped in TLS, so the client
///   performs a TLS handshake *with the proxy* before issuing `CONNECT`. Only a
///   proxy explicitly configured to serve TLS accepts this.
///
/// Configuration sources whose protocol keys instead describe the destination
/// (the Windows `ProxyServer` registry value, aria2's `--https-proxy`) must map
/// those keys onto the transport axis before constructing a `ProxyType`; see
/// [`parse_windows_proxy_server`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyType {
    Http,
    Https,
    /// SOCKS4,目标地址由客户端本地解析后以 IP 下发。
    Socks4,
    /// SOCKS4a——SOCKS4 的远端解析扩展,目标 hostname 原样交给代理。设置页
    /// 下拉不提供这一项(纯 SOCKS4 服务端会拒绝 4a 请求,不能当默认值);
    /// 只有用户在自定义代理 URL 字段显式写 `socks4a://` 时才会出现——那是
    /// 用户对自己代理能力的声明,必须原样兑现而不是悄悄降级成 SOCKS4。
    Socks4a,
    /// SOCKS5。恒为代理侧 DNS 解析,见 [`Self::scheme`]。
    Socks5,
}

impl ProxyType {
    /// 识别代理类型字符串(配置 wire 值或 URL scheme)。
    ///
    /// 与标准库 `FromStr::from_str` 同名会引发 clippy `should_implement_trait`,
    /// 故用 `parse_str` 命名。**无法识别返回 `None`,绝不静默折算**:曾经的
    /// `_ => Http` 通配兜底会把任何拼错或未覆盖的 scheme 变成 HTTP 代理,
    /// 对 SOCKS 端口发出 HTTP CONNECT,表现为无从归因的连接失败。识别集覆盖
    /// reqwest 接受的全部代理 scheme,兜底策略由各调用方按自身语境显式选择。
    ///
    /// `socks5h`/`socks4a` 是同一枚举值的远端解析别名与独立变体,见
    /// [`Self::scheme`]。
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "socks4" => Some(Self::Socks4),
            "socks4a" => Some(Self::Socks4a),
            // `socks5h` 与 `socks5` 同义:本类型的 SOCKS5 恒走代理侧解析。
            "socks5" | "socks5h" => Some(Self::Socks5),
            _ => None,
        }
    }

    /// 持久化 wire 值(DB `proxy_type` 列、设置页下拉、Dart/前端协议)。
    /// **与 [`Self::scheme`] 是两条独立通道**,不随 reqwest 的 scheme 变化。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks4a => "socks4a",
            Self::Socks5 => "socks5",
        }
    }

    /// URL scheme used by reqwest's `Proxy::all(url)`.
    ///
    /// SOCKS5 用 `socks5h` 而非 `socks5`:reqwest 对 `socks5` 的语义是**客户端
    /// 本地解析目标 hostname**、只把解析出的 IP 经隧道转发,`socks5h` 才把
    /// hostname 原样交给代理解析。目标域名在 DNS 层被投毒/封锁时,本地解析
    /// 拿到的是错误地址,请求在发出前目标就已经错了——代理能连通真实主机也
    /// 无济于事。代理侧解析是 SOCKS5 的通用预期(与 curl 的 `socks5h` 一致),
    /// 代价是代理必须支持 domain ATYP(RFC 1928 §4 ATYP=0x03),仅接受 IP 的
    /// 实现会失败。
    ///
    /// SOCKS4 保持 `socks4`(本地解析):远端解析需要 SOCKS4a,而 SOCKS4a 请求
    /// 会被纯 SOCKS4 服务端拒绝,不能当默认值。用户显式选 [`Self::Socks4a`]
    /// 时按 `socks4a` 下发,由用户自己为代理能力背书。
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks4a => "socks4a",
            Self::Socks5 => "socks5h",
        }
    }

    /// Whether this is a SOCKS variant (4 / 4a / 5).
    #[allow(dead_code)]
    pub fn is_socks(&self) -> bool {
        matches!(self, Self::Socks4 | Self::Socks4a | Self::Socks5)
    }
}

// ---------------------------------------------------------------------------
// ProxyConfig
// ---------------------------------------------------------------------------

/// Complete proxy configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub proxy_type: ProxyType,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Comma-separated list of hosts/domains to bypass the proxy.
    /// Supports wildcards like `*.local`.
    pub no_proxy_list: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            mode: ProxyMode::None,
            proxy_type: ProxyType::Http,
            host: String::new(),
            port: 0,
            username: String::new(),
            password: String::new(),
            no_proxy_list: String::new(),
        }
    }
}

impl ProxyConfig {
    /// Build from a HashMap of DB config entries.
    pub fn from_config_map(map: &HashMap<String, String>) -> Self {
        let mode = map
            .get("proxy_mode")
            .map(|v| ProxyMode::parse_str(v))
            .unwrap_or(ProxyMode::None);
        // 缺键或值损坏 → Http:这是设置页从未保存过代理类型时的历史默认值,
        // 且 `proxy_mode` 通常同时为 `none`,不会真的去连一个 HTTP 代理。
        let proxy_type = map
            .get("proxy_type")
            .and_then(|v| ProxyType::parse_str(v))
            .unwrap_or(ProxyType::Http);
        let host = map.get("proxy_host").cloned().unwrap_or_default();
        let port = map
            .get("proxy_port")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(0);
        let username = map.get("proxy_username").cloned().unwrap_or_default();
        let password = map.get("proxy_password").cloned().unwrap_or_default();
        let no_proxy_list = map.get("proxy_no_list").cloned().unwrap_or_default();

        Self {
            mode,
            proxy_type,
            host,
            port,
            username,
            password,
            no_proxy_list,
        }
    }

    /// Whether this config represents an active (non-None) proxy.
    pub fn is_active(&self) -> bool {
        self.mode != ProxyMode::None
    }

    /// Whether the configured proxy type is SOCKS (4 or 5).
    #[allow(dead_code)]
    pub fn is_socks(&self) -> bool {
        self.proxy_type.is_socks()
    }

    /// Build the full proxy URL string, e.g. `socks5h://user:pass@host:port`.
    ///
    /// scheme 由 [`ProxyType::scheme`] 给出(SOCKS5 → `socks5h`),**与持久化的
    /// `proxy_type` 值([`ProxyType::as_str`],恒为 `socks5`)是两条独立通道**。
    /// Used by reqwest's `Proxy::all(url)`.
    /// Returns `None` if mode is `None` or host is empty.
    pub fn to_proxy_url(&self) -> Option<String> {
        match self.mode {
            ProxyMode::None => None,
            ProxyMode::System => {
                // System proxy is resolved at call time via detect_system_proxy()
                None
            }
            // Auto 的具体代理由 auto_proxy 决策路径以 Manual 配置给出，
            // 原始 Auto 配置本身没有可直接使用的代理 URL。
            ProxyMode::Auto => None,
            ProxyMode::Manual => {
                if self.host.is_empty() || self.port == 0 {
                    return None;
                }
                let scheme = self.proxy_type.scheme();
                if !self.username.is_empty() {
                    let enc_user = percent_encode_userinfo(&self.username);
                    let enc_pass = percent_encode_userinfo(&self.password);
                    Some(format!(
                        "{}://{}:{}@{}:{}",
                        scheme, enc_user, enc_pass, self.host, self.port
                    ))
                } else {
                    Some(format!("{}://{}:{}", scheme, self.host, self.port))
                }
            }
        }
    }

    /// Resolve a `System` proxy config into a concrete `Manual` config by
    /// reading the OS-level proxy settings (Windows registry / env vars).
    ///
    /// - `None` mode → returned as-is.
    /// - `Manual` mode → returned as-is.
    /// - `System` mode → calls `detect_system_proxy()` and returns the resolved
    ///   config with `mode = Manual` and populated host/port fields.
    ///   If system proxy is disabled or detection fails, falls back to `None`.
    ///
    /// This is needed for FTP downloads because `ftp_connect_sync_with_proxy`
    /// reads `host`/`port` directly (unlike HTTP which uses `build_client()`
    /// where system proxy resolution already happens inside reqwest).
    pub fn resolve(&self) -> Self {
        match self.mode {
            ProxyMode::System => {
                match detect_system_proxy() {
                    Ok(Some(resolved)) => resolved,
                    Ok(None) => {
                        // System proxy not configured → direct connection
                        Self::default()
                    }
                    Err(e) => {
                        log_info!("[proxy] system proxy detection failed: {}", e);
                        Self::default()
                    }
                }
            }
            // Auto：调用点未经决策路径时的安全折算 = 直连（FTP/RSS 等
            // 非 HTTP-coordinator 路径拿到的 Auto 就是直连语义）。具体
            // 代理只能由 auto_proxy 的缓存/采样决策显式给出。
            ProxyMode::Auto => Self::default(),
            _ => self.clone(),
        }
    }

    /// Return the `host:port` string for direct socket connections (FTP SOCKS proxy).
    #[allow(dead_code)]
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// 单任务代理字段（`tasks.proxy_url`）的哨兵值：**强制直连**。
    /// 与空串（跟随全局）语义不同——全局配了代理时本任务仍然直连，
    /// 且不参与 `ProxyMode::Auto` 的采样/切换（用户显式选择压过一切）。
    pub const DIRECT_SENTINEL: &'static str = "direct://";

    /// 单任务代理字段的哨兵值：**跟随系统代理**。每次启动/恢复时经
    /// [`Self::resolve`] 现场解析（注册表/环境变量），系统代理未配置时
    /// 安全回退直连——引用语义，系统代理地址变了任务自动跟随。
    pub const SYSTEM_SENTINEL: &'static str = "system://";

    /// Parse a proxy URL string like `socks5://user:pass@host:port` into a ProxyConfig.
    ///
    /// Used for per-task proxy override where the user provides a single URL.
    /// 两个哨兵值见 [`Self::DIRECT_SENTINEL`]（→ 直连）与
    /// [`Self::SYSTEM_SENTINEL`]（→ `System` 模式，调用方应随后 `resolve()`）。
    ///
    /// scheme 无法识别(用户拼错)时按 [`ProxyType::Http`] 处理并打日志。
    /// 这里刻意**不**回退直连:静默直连会把本该走代理的流量放到明网上,
    /// 用户还以为代理生效了;按 HTTP 处理则在建连阶段就报错,可归因。
    pub fn from_proxy_url(url: &str) -> Self {
        if url.is_empty() || url == Self::DIRECT_SENTINEL {
            return Self::default();
        }
        if url == Self::SYSTEM_SENTINEL {
            return Self {
                mode: ProxyMode::System,
                ..Self::default()
            };
        }

        // Extract scheme
        let (scheme, rest) = if let Some(idx) = url.find("://") {
            (&url[..idx], &url[idx + 3..])
        } else {
            ("http", url)
        };

        let proxy_type = ProxyType::parse_str(scheme).unwrap_or_else(|| {
            log_info!(
                "[proxy] unrecognized proxy URL scheme {:?}, treating as http",
                scheme
            );
            ProxyType::Http
        });

        // Extract auth (user:pass@) if present
        let (auth, host_port) = if let Some(at_idx) = rest.rfind('@') {
            (&rest[..at_idx], &rest[at_idx + 1..])
        } else {
            ("", rest)
        };

        let (username, password) = if auth.is_empty() {
            (String::new(), String::new())
        } else if let Some(colon) = auth.find(':') {
            (
                percent_decode(&auth[..colon]),
                percent_decode(&auth[colon + 1..]),
            )
        } else {
            (percent_decode(auth), String::new())
        };

        // Extract host and port
        let (_scheme_type, host, port) = parse_host_port(host_port);

        Self {
            mode: ProxyMode::Manual,
            proxy_type,
            host,
            port,
            username,
            password,
            no_proxy_list: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// System proxy detection (Windows)
// ---------------------------------------------------------------------------

/// Detect the system-level proxy from Windows registry.
///
/// Reads `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`:
/// - `ProxyEnable` (DWORD): 0 = disabled, 1 = enabled
/// - `ProxyServer` (SZ): proxy address, possibly multi-protocol format
/// - `ProxyOverride` (SZ): semicolon-separated bypass list
///
/// The `ProxyServer` value can be:
/// - Simple: `host:port` (applies to all protocols)
/// - Multi-protocol: `http=host:port;https=host:port;ftp=host:port;socks=host:port`
///
/// Returns a `ProxyConfig` in `Manual` mode on success, or `None` if disabled/unavailable.
#[cfg(target_os = "windows")]
pub fn detect_system_proxy() -> Result<Option<ProxyConfig>, DownloadError> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let inet = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .map_err(|e| DownloadError::Other(format!("failed to open Internet Settings: {}", e)))?;

    let enabled: u32 = inet.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return Ok(None);
    }

    let server: String = inet.get_value("ProxyServer").unwrap_or_default();
    if server.is_empty() {
        return Ok(None);
    }

    // Read bypass list (optional)
    let bypass: String = inet.get_value("ProxyOverride").unwrap_or_default();
    // Convert semicolons to commas for our internal format
    let no_proxy = bypass.replace(';', ",").replace("<local>", "localhost");

    // Parse the ProxyServer value
    let (proxy_type, host, port) = parse_windows_proxy_server(&server);

    Ok(Some(ProxyConfig {
        mode: ProxyMode::Manual, // system proxy behaves like manual for reqwest
        proxy_type,
        host,
        port,
        username: String::new(),
        password: String::new(),
        no_proxy_list: no_proxy,
    }))
}

/// macOS 系统代理检测：调用 `scutil --proxy` 读取 SystemConfiguration 的
/// 代理字典（GUI 应用不继承终端 shell 的环境变量，而 `networksetup`/
/// 系统偏好设置写入的代理只存在于 SystemConfiguration，读环境变量拿不到）。
/// 子进程失败、超时或未启用任何协议均返回 `None`，从不向上抛错——系统代理
/// 探测永远是尽力而为，绝不能因为它失败而拖垮整个下载流程。
#[cfg(target_os = "macos")]
pub fn detect_system_proxy() -> Result<Option<ProxyConfig>, DownloadError> {
    Ok(cached_system_proxy_probe(|| {
        let output = run_command_with_timeout("scutil", &["--proxy"], SYSTEM_PROXY_PROBE_TIMEOUT);
        let text = match output {
            Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            _ => return None,
        };
        parse_scutil_proxy_output(&text)
    }))
}

/// Linux 系统代理检测：先看 `https_proxy`/`http_proxy`/`all_proxy` 环境变量
/// （桌面终端启动的进程、systemd 用户服务常见配置方式），环境变量都没有
/// 时再退而求其次读 GNOME `gsettings org.gnome.system.proxy`（KDE/Clash 等
/// 通过 V2rayN 之类客户端只写这里，不写环境变量，GUI 进程更拿不到父 shell
/// 的环境变量）。子进程/命令缺失一律静默回退，不影响非 GNOME 桌面环境。
#[cfg(target_os = "linux")]
pub fn detect_system_proxy() -> Result<Option<ProxyConfig>, DownloadError> {
    Ok(cached_system_proxy_probe(|| {
        detect_proxy_from_env().or_else(detect_proxy_from_gsettings)
    }))
}

/// macOS/Linux 探测结果的短时缓存 TTL。`ProxyConfig::resolve()` 在 actor 线程
/// 上同步调用，每次都拉起 `scutil`/`gsettings` 子进程既慢又会在密集建
/// client 时（RSS 批量抓取、插件 bridge 构造）反复阻塞；系统代理配置本身
/// 分钟级才会变，30s 内复用上次结果足够新鲜。
#[cfg(any(target_os = "macos", target_os = "linux"))]
const SYSTEM_PROXY_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(any(target_os = "macos", target_os = "linux"))]
static SYSTEM_PROXY_CACHE: std::sync::Mutex<Option<(std::time::Instant, Option<ProxyConfig>)>> =
    std::sync::Mutex::new(None);

/// 带 TTL 的探测缓存：命中未过期结果直接返回，否则执行 `probe` 并回填。
/// 锁中毒直接取内部值——缓存只是一个可重算的快照。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cached_system_proxy_probe(probe: impl FnOnce() -> Option<ProxyConfig>) -> Option<ProxyConfig> {
    let mut guard = SYSTEM_PROXY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, cached)) = guard.as_ref()
        && at.elapsed() < SYSTEM_PROXY_CACHE_TTL
    {
        return cached.clone();
    }
    let fresh = probe();
    *guard = Some((std::time::Instant::now(), fresh.clone()));
    fresh
}

/// 其余平台（Android/iOS 等）：沙箱环境不支持任意子进程与桌面配置源，保持
/// 原有行为直接返回 `None`。
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub fn detect_system_proxy() -> Result<Option<ProxyConfig>, DownloadError> {
    Ok(None)
}

/// 单个外部命令探测的超时预算。`scutil`/`gsettings` 都是读取本地配置的
/// 轻量命令，正常应在数十毫秒内返回；给到 1.2s 是为异常慢的系统（杀毒软件
/// hook、容器/沙箱下的 IPC 延迟）留余量，同时仍满足"快速、尽力而为"的
/// 探测语义——绝不能让一次系统代理探测拖住调用方数秒。
#[allow(dead_code)] // 仅 macOS/Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
const SYSTEM_PROXY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1200);

/// 运行外部命令并在超时后杀掉子进程放弃等待，而不是无限阻塞调用线程。
/// 标准库没有内建的进程超时原语，这里用 `try_wait` 轮询代替：命令通常在
/// 一两个轮询周期内就已退出，超时只在命令挂起/被沙箱拦截时触发。
/// stdout 用管道读取；探测命令的输出量极小（不到 1KB），不会撞上管道满导致
/// 子进程阻塞写入、我们又没在读的死锁。
#[allow(dead_code)] // 仅 macOS/Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let _ = out.read_to_end(&mut stdout);
                }
                return Some(std::process::Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// 把 `scutil --proxy` 的字典输出（`Key : Value` 逐行，嵌套 `<array>` 块
/// 用不到的 key 混进来也无妨）压成一个扁平 map。只有顶层标量字段
/// （`HTTPEnable`/`HTTPProxy`/`HTTPPort` 等）会被后续查询用到。
#[allow(dead_code)] // 仅 macOS 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn parse_scutil_dict(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            if !key.is_empty() {
                map.insert(key.to_string(), value.trim().to_string());
            }
        }
    }
    map
}

/// 从 `scutil --proxy` 字典中选出一个已启用的代理协议，按 socks > https >
/// http 优先（与 [`parse_windows_proxy_server`] 一致）。`HTTPSProxy` 字段
/// 命名的是【流量目标】协议而非代理端点自身的传输层——该端点仍然只会
/// 应答明文 HTTP `CONNECT`，因此映射到 [`ProxyType::Http`]，理由与
/// Windows `ProxyServer` 的 `https=` 键完全相同（见该函数文档）。
#[allow(dead_code)] // 仅 macOS 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn parse_scutil_proxy_output(text: &str) -> Option<ProxyConfig> {
    let map = parse_scutil_dict(text);
    let is_enabled = |key: &str| map.get(key).map(|v| v.trim() == "1").unwrap_or(false);
    let host_port = |host_key: &str, port_key: &str| -> Option<(String, u16)> {
        let host = map.get(host_key)?.trim();
        if host.is_empty() {
            return None;
        }
        let port = map
            .get(port_key)
            .and_then(|p| p.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)?;
        Some((host.to_string(), port))
    };
    let candidates: [(&str, &str, &str, ProxyType); 3] = [
        ("SOCKSEnable", "SOCKSProxy", "SOCKSPort", ProxyType::Socks5),
        ("HTTPSEnable", "HTTPSProxy", "HTTPSPort", ProxyType::Http),
        ("HTTPEnable", "HTTPProxy", "HTTPPort", ProxyType::Http),
    ];
    for (enable_key, host_key, port_key, proxy_type) in candidates {
        if is_enabled(enable_key)
            && let Some((host, port)) = host_port(host_key, port_key)
        {
            return Some(ProxyConfig {
                mode: ProxyMode::Manual,
                proxy_type,
                host,
                port,
                ..Default::default()
            });
        }
    }
    None
}

/// 按 `https_proxy` > `http_proxy` > `all_proxy` 优先读取代理环境变量
/// （同时兼容大小写变体），用 `url` crate 解析出协议/host/port/凭据。
/// 纯读取无子进程，Windows/macOS 上调用也无害（多数情况下这些变量本就
/// 未设置），因此不做平台 cfg 限制，方便跨平台单测覆盖解析逻辑。
#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn detect_proxy_from_env() -> Option<ProxyConfig> {
    let raw = first_nonempty_env(&["https_proxy", "HTTPS_PROXY"])
        .or_else(|| first_nonempty_env(&["http_proxy", "HTTP_PROXY"]))
        .or_else(|| first_nonempty_env(&["all_proxy", "ALL_PROXY"]))?;
    parse_env_proxy_url(&raw)
}

#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// 解析形如 `http://user:pass@host:port` 的代理环境变量取值。这里的
/// scheme 描述的是代理端点自身的传输层（标准 `http_proxy` 约定），与
/// Windows 注册表/`scutil` 字典里【目标协议】语义的键名不同，因此按字面
/// scheme 直接映射，不做目标→传输层的转换。
#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn parse_env_proxy_url(raw: &str) -> Option<ProxyConfig> {
    let parsed = url::Url::parse(raw).ok()?;
    let host = parsed.host_str()?.to_string();
    let proxy_type = match parsed.scheme().to_ascii_lowercase().as_str() {
        "socks5" | "socks5h" => ProxyType::Socks5,
        "socks4" => ProxyType::Socks4,
        "socks4a" => ProxyType::Socks4a,
        "https" => ProxyType::Https,
        _ => ProxyType::Http,
    };
    let port = parsed.port().unwrap_or(if proxy_type == ProxyType::Https {
        443
    } else {
        8080
    });
    let username = percent_decode(parsed.username());
    let password = parsed.password().map(percent_decode).unwrap_or_default();
    Some(ProxyConfig {
        mode: ProxyMode::Manual,
        proxy_type,
        host,
        port,
        username,
        password,
        no_proxy_list: first_nonempty_env(&["no_proxy", "NO_PROXY"]).unwrap_or_default(),
    })
}

/// GNOME `gsettings` 代理配置：仅在 `org.gnome.system.proxy mode` 为
/// `manual` 时生效（`none`/`auto` 均不该覆盖直连或 PAC 行为）。按 socks >
/// https > http 优先，`https` schema 同样只声明目标协议而非端点传输层，
/// 理由与 [`parse_scutil_proxy_output`] 相同。
#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn detect_proxy_from_gsettings() -> Option<ProxyConfig> {
    let mode = gsettings_get("org.gnome.system.proxy", "mode")?;
    if mode != "manual" {
        return None;
    }
    let candidates: [(&str, ProxyType); 3] = [
        ("socks", ProxyType::Socks5),
        ("https", ProxyType::Http),
        ("http", ProxyType::Http),
    ];
    for (proto, proxy_type) in candidates {
        let schema = format!("org.gnome.system.proxy.{proto}");
        let Some(host) = gsettings_get(&schema, "host").filter(|h| !h.is_empty()) else {
            continue;
        };
        let Some(port) = gsettings_get(&schema, "port")
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
        else {
            continue;
        };
        return Some(ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type,
            host,
            port,
            ..Default::default()
        });
    }
    None
}

/// 剥掉 GNOME dconf 字符串取值外层的单引号（固定输出 `'value'`）；整数
/// 取值不带引号，`trim_matches` 对其是无副作用的 no-op。
#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn strip_gsettings_quotes(raw: &str) -> String {
    raw.trim().trim_matches('\'').to_string()
}

/// 执行 `gsettings get <schema> <key>`。命令缺失、超时、非零退出码或剥引号
/// 后为空一律返回 `None`。
#[allow(dead_code)] // 仅 Linux 探测路径实际调用；其余平台保留供跨平台单测覆盖解析逻辑
fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let out = run_command_with_timeout(
        "gsettings",
        &["get", schema, key],
        SYSTEM_PROXY_PROBE_TIMEOUT,
    )?;
    if !out.status.success() {
        return None;
    }
    let value = strip_gsettings_quotes(&String::from_utf8_lossy(&out.stdout));
    if value.is_empty() { None } else { Some(value) }
}

/// Parse the Windows `ProxyServer` registry value.
///
/// Handles both formats:
/// - Simple: `host:port` → (Http, host, port)
/// - Multi-protocol: `http=host:port;https=host:port;socks=host:port` → prefer https > socks > http
///
/// Note the scheme asymmetry with our own [`ProxyType`]: in this registry value
/// the `https=` key names the **destination** protocol whose traffic the proxy
/// handles, not the proxy endpoint's own transport. Such an endpoint speaks
/// plaintext HTTP `CONNECT`, so it maps to [`ProxyType::Http`] — mapping it to
/// [`ProxyType::Https`] would make reqwest attempt a TLS handshake *with the
/// proxy*, which these endpoints do not accept (issue #183).
#[allow(dead_code)] // only called from #[cfg(windows)] detect_system_proxy; kept for cross-platform test coverage
pub fn parse_windows_proxy_server(server: &str) -> (ProxyType, String, u16) {
    // Check if it's multi-protocol format (contains '=')
    if server.contains('=') {
        let entries = parse_multi_protocol_proxy(server);

        // Priority: socks > https > http
        if let Some((host, port)) = entries.get("socks") {
            return (ProxyType::Socks5, host.clone(), *port);
        }
        if let Some((host, port)) = entries.get("https") {
            // `https=` describes the destination, so the transport stays HTTP.
            return (ProxyType::Http, host.clone(), *port);
        }
        if let Some((host, port)) = entries.get("http") {
            return (ProxyType::Http, host.clone(), *port);
        }
        // Fallback: take first entry
        if let Some((_key, (host, port))) = entries.into_iter().next() {
            return (ProxyType::Http, host, port);
        }
    }

    // Simple format: "host:port" (may itself carry a scheme prefix, see
    // parse_host_port's BUG-PROXY-SCHEME-PREFIX doc).
    let (scheme_type, host, port) = parse_host_port(server);
    (scheme_type.unwrap_or(ProxyType::Http), host, port)
}

/// Parse multi-protocol proxy string like `http=host:port;https=host2:port2;socks=host3:port3`.
#[allow(dead_code)] // only called from #[cfg(windows)] detect_system_proxy; kept for cross-platform test coverage
fn parse_multi_protocol_proxy(server: &str) -> HashMap<String, (String, u16)> {
    let mut result = HashMap::new();
    for entry in server.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((protocol, addr)) = entry.split_once('=') {
            let protocol = protocol.trim().to_ascii_lowercase();
            let (_scheme_type, host, port) = parse_host_port(addr.trim());
            if !host.is_empty() {
                result.insert(protocol, (host, port));
            }
        }
    }
    result
}

/// Parse `host:port` string, defaulting port to 8080 if missing/invalid.
///
/// BUG-PROXY-SCHEME-PREFIX：一些环境（第三方代理工具改写的 Windows 注册表
/// `ProxyServer` 值、手工填写的系统代理字符串）把完整 URL（如
/// `http://127.0.0.1:20122`）而非文档规定的裸 `host:port` 写入本应是简单
/// 格式的字段。若不剥离 scheme，`rfind(':')` 会把 `http://127.0.0.1` 整体
/// 当作 host，调用方再叠加一次 scheme 前缀，拼出 `http://http://host:port`
/// 这种畸形代理 URL，reqwest 直接连接失败。
///
/// 剥离出的 scheme 若为 `socks5`/`socks5h`，以 `Some(ProxyType::Socks5)`
/// 返回，供默认按 HTTP 处理的调用方据此改判；其余 scheme（含无 scheme）
/// 一律返回 `None`，调用方保留自己的默认类型。
fn parse_host_port(addr: &str) -> (Option<ProxyType>, String, u16) {
    let (scheme_type, addr) = match addr.find("://") {
        Some(idx) => {
            let scheme = addr[..idx].to_ascii_lowercase();
            let ty = match scheme.as_str() {
                "socks5" | "socks5h" => Some(ProxyType::Socks5),
                _ => None,
            };
            (ty, &addr[idx + 3..])
        }
        None => (None, addr),
    };

    // Handle IPv6: [::1]:port
    if let Some(bracket_end) = addr.find(']') {
        let host = addr[..=bracket_end].to_string();
        let rest = &addr[bracket_end + 1..];
        let port = rest
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8080);
        return (scheme_type, host, port);
    }

    // Standard host:port
    if let Some(colon) = addr.rfind(':') {
        let host = addr[..colon].to_string();
        let port = addr[colon + 1..].parse::<u16>().unwrap_or(8080);
        if !host.is_empty() {
            return (scheme_type, host, port);
        }
    }

    // No port specified
    if !addr.is_empty() {
        return (scheme_type, addr.to_string(), 8080);
    }

    (scheme_type, String::new(), 0)
}

// ---------------------------------------------------------------------------
// URL percent-encoding helpers (for proxy credentials)
// ---------------------------------------------------------------------------

/// 把单个 ASCII 十六进制数字字节转成 nibble 值（0-15）；非 hex 返回 `None`。
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode percent-encoded strings (e.g. `p%40ss` → `p@ss`).
/// Used to decode usernames/passwords from proxy URLs.
fn percent_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 按字节解析 %XX，绝不对 `&str` 切片：`%` 后紧跟原始（未编码）多字节
        // UTF-8 字符时（如 `user%中:pass` / `%😀`，用户在代理 URL 凭据里输入
        // 非 ASCII 即可触发），`&s[i+1..i+3]` 的切点会落在多字节序列内部，
        // 触发 char-boundary panic。此处只对 `bytes` 按下标取值，安全。
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            result.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

/// Percent-encode characters that are not allowed in the userinfo component
/// of a URI (RFC 3986 §3.2.1).  Encodes everything except unreserved chars
/// and sub-delimiters that are safe in userinfo.
fn percent_encode_userinfo(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            // unreserved: ALPHA / DIGIT / "-" / "." / "_" / "~"
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                result.push(b as char);
            }
            // sub-delimiters safe in userinfo (except '@' '/' '?' ':')
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' => {
                result.push(b as char);
            }
            _ => {
                result.push('%');
                result.push_str(&format!("{:02X}", b));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// SOCKS5 synchronous TCP helper (for FTP proxy)
// ---------------------------------------------------------------------------

/// Establish a TCP connection through a SOCKS5 proxy (synchronous, for spawn_blocking).
///
/// Implements the SOCKS5 handshake (RFC 1928) manually to avoid external
/// dependencies. This is intentionally synchronous because suppaftp's FTP
/// stream requires a `std::net::TcpStream`.
///
/// Supports:
/// - No authentication (method 0x00)
/// - Username/password authentication (method 0x02, RFC 1929)
pub fn socks5_connect_sync(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    username: &str,
    password: &str,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    use std::net::TcpStream;

    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);

    // Resolve and connect to proxy
    let sock_addr: std::net::SocketAddr = proxy_addr.parse().or_else(|_| {
        use std::net::ToSocketAddrs;
        proxy_addr
            .to_socket_addrs()
            .map_err(|e| DownloadError::Other(format!("proxy DNS resolve error: {}", e)))?
            .next()
            .ok_or_else(|| DownloadError::Other("proxy DNS returned no addresses".to_string()))
    })?;

    let stream = TcpStream::connect_timeout(&sock_addr, timeout)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 proxy connect error: {}", e)))?;

    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| DownloadError::Other(format!("set_read_timeout error: {}", e)))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| DownloadError::Other(format!("set_write_timeout error: {}", e)))?;

    socks5_handshake(stream, target_host, target_port, username, password)
}

/// Perform the SOCKS5 handshake on an already-connected TCP stream.
fn socks5_handshake(
    mut stream: std::net::TcpStream,
    target_host: &str,
    target_port: u16,
    username: &str,
    password: &str,
) -> Result<std::net::TcpStream, DownloadError> {
    use std::io::{Read, Write};

    let need_auth = !username.is_empty();

    // Step 1: Greeting — tell proxy which auth methods we support
    let greeting = if need_auth {
        vec![0x05, 0x02, 0x00, 0x02] // VER=5, NMETHODS=2, NO_AUTH + USER_PASS
    } else {
        vec![0x05, 0x01, 0x00] // VER=5, NMETHODS=1, NO_AUTH
    };
    stream
        .write_all(&greeting)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 greeting write error: {}", e)))?;

    // Step 2: Read method selection
    let mut method_resp = [0u8; 2];
    stream
        .read_exact(&mut method_resp)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 method response read error: {}", e)))?;

    if method_resp[0] != 0x05 {
        return Err(DownloadError::Other(format!(
            "SOCKS5 protocol error: unexpected version {}",
            method_resp[0]
        )));
    }

    match method_resp[1] {
        0x00 => {} // No authentication required — proceed to connect
        0x02 => {
            // Username/password authentication (RFC 1929)
            if !need_auth {
                return Err(DownloadError::Other(
                    "SOCKS5 proxy requires authentication but no credentials provided".to_string(),
                ));
            }
            socks5_auth(&mut stream, username, password)?;
        }
        0xFF => {
            return Err(DownloadError::Other(
                "SOCKS5 proxy rejected all authentication methods".to_string(),
            ));
        }
        other => {
            return Err(DownloadError::Other(format!(
                "SOCKS5 unsupported auth method: 0x{:02x}",
                other
            )));
        }
    }

    // Step 3: CONNECT request
    // RFC 1928 §5: DOMAINNAME field is also one byte length.
    if target_host.len() > 255 {
        return Err(DownloadError::Other(format!(
            "SOCKS5 target hostname too long: {} bytes (max 255)",
            target_host.len()
        )));
    }
    let mut connect_req = vec![
        0x05, // VER
        0x01, // CMD = CONNECT
        0x00, // RSV
        0x03, // ATYP = DOMAINNAME
        target_host.len() as u8,
    ];
    connect_req.extend_from_slice(target_host.as_bytes());
    connect_req.push((target_port >> 8) as u8);
    connect_req.push((target_port & 0xFF) as u8);

    stream
        .write_all(&connect_req)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 connect write error: {}", e)))?;

    // Step 4: Read CONNECT response
    let mut resp_header = [0u8; 4];
    stream
        .read_exact(&mut resp_header)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 connect response read error: {}", e)))?;

    if resp_header[0] != 0x05 {
        return Err(DownloadError::Other(format!(
            "SOCKS5 response version error: {}",
            resp_header[0]
        )));
    }

    if resp_header[1] != 0x00 {
        let err_msg = match resp_header[1] {
            0x01 => "general SOCKS server failure",
            0x02 => "connection not allowed by ruleset",
            0x03 => "network unreachable",
            0x04 => "host unreachable",
            0x05 => "connection refused",
            0x06 => "TTL expired",
            0x07 => "command not supported",
            0x08 => "address type not supported",
            _ => "unknown error",
        };
        return Err(DownloadError::Other(format!(
            "SOCKS5 connect failed: {} (0x{:02x})",
            err_msg, resp_header[1]
        )));
    }

    // Read and discard the BND.ADDR and BND.PORT
    match resp_header[3] {
        0x01 => {
            // IPv4: 4 bytes + 2 port
            let mut buf = [0u8; 6];
            stream.read_exact(&mut buf).map_err(|e| {
                DownloadError::Other(format!("SOCKS5 read bound addr error: {}", e))
            })?;
        }
        0x03 => {
            // Domain: 1 byte len + domain + 2 port
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).map_err(|e| {
                DownloadError::Other(format!("SOCKS5 read domain len error: {}", e))
            })?;
            let mut buf = vec![0u8; len_buf[0] as usize + 2];
            stream.read_exact(&mut buf).map_err(|e| {
                DownloadError::Other(format!("SOCKS5 read bound domain error: {}", e))
            })?;
        }
        0x04 => {
            // IPv6: 16 bytes + 2 port
            let mut buf = [0u8; 18];
            stream.read_exact(&mut buf).map_err(|e| {
                DownloadError::Other(format!("SOCKS5 read bound addr6 error: {}", e))
            })?;
        }
        other => {
            return Err(DownloadError::Other(format!(
                "SOCKS5 unexpected address type: 0x{:02x}",
                other
            )));
        }
    }

    // Clear timeouts for the tunneled connection (FTP will set its own)
    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();

    Ok(stream)
}

/// SOCKS5 username/password sub-negotiation (RFC 1929).
fn socks5_auth(
    stream: &mut std::net::TcpStream,
    username: &str,
    password: &str,
) -> Result<(), DownloadError> {
    use std::io::{Read, Write};

    // RFC 1929 §2: both username and password must fit in one byte length field.
    if username.len() > 255 {
        return Err(DownloadError::Other(format!(
            "SOCKS5 username must be ≤ 255 bytes, got {}",
            username.len()
        )));
    }
    if password.len() > 255 {
        return Err(DownloadError::Other(format!(
            "SOCKS5 password must be ≤ 255 bytes, got {}",
            password.len()
        )));
    }

    let mut auth_req = vec![0x01]; // VER = 1
    auth_req.push(username.len() as u8);
    auth_req.extend_from_slice(username.as_bytes());
    auth_req.push(password.len() as u8);
    auth_req.extend_from_slice(password.as_bytes());

    stream
        .write_all(&auth_req)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 auth write error: {}", e)))?;

    let mut auth_resp = [0u8; 2];
    stream
        .read_exact(&mut auth_resp)
        .map_err(|e| DownloadError::Other(format!("SOCKS5 auth response read error: {}", e)))?;

    if auth_resp[1] != 0x00 {
        return Err(DownloadError::Other(
            "SOCKS5 authentication failed: invalid username or password".to_string(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// SOCKS4 synchronous TCP helper (for FTP proxy)
// ---------------------------------------------------------------------------

/// Establish a TCP connection through a SOCKS4/SOCKS4a proxy (synchronous).
///
/// `remote_dns = false`（SOCKS4，RFC 无正式文本，见 SOCKS4 协议规范）：目标
/// 域名在本地解析成 IPv4 后下发，代理只看到 IP。
/// `remote_dns = true`（SOCKS4a）：DSTIP 填 `0.0.0.x`（x ≠ 0，这个非法 IP 就是
/// 4a 的信号），USERID 之后追加以 NUL 结尾的目标域名，由代理解析。纯 SOCKS4
/// 服务端不认这个扩展，因此只在用户显式选择 [`ProxyType::Socks4a`] 时启用。
pub fn socks4_connect_sync(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    remote_dns: bool,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    // 4a 的域名以裸字节 + NUL 终止上送:非 ASCII 会被代理按字节乱解,内嵌
    // NUL 会截断整条请求。在建连之前就拒绝,不发畸形包。
    if remote_dns
        && (target_host.is_empty()
            || !target_host.is_ascii()
            || target_host.as_bytes().contains(&0))
    {
        return Err(DownloadError::Other(format!(
            "SOCKS4a rejects non-ASCII target host: {}",
            target_host
        )));
    }

    let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
    let sock_addr: std::net::SocketAddr = proxy_addr.parse().or_else(|_| {
        proxy_addr
            .to_socket_addrs()
            .map_err(|e| DownloadError::Other(format!("proxy DNS resolve error: {}", e)))?
            .next()
            .ok_or_else(|| DownloadError::Other("proxy DNS returned no addresses".to_string()))
    })?;

    let mut stream = TcpStream::connect_timeout(&sock_addr, timeout)
        .map_err(|e| DownloadError::Other(format!("SOCKS4 proxy connect error: {}", e)))?;

    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    // DSTIP：4a 用 0.0.0.1 占位，否则本地解析出真实 IPv4。
    let ip_bytes: [u8; 4] = if remote_dns {
        [0, 0, 0, 1]
    } else {
        let target_addr = format!("{}:{}", target_host, target_port);
        let target_ip = target_addr
            .to_socket_addrs()
            .map_err(|e| DownloadError::Other(format!("target DNS resolve error: {}", e)))?
            .find(|a| a.is_ipv4())
            .ok_or_else(|| {
                DownloadError::Other(format!(
                    "SOCKS4 requires IPv4 but {} has no IPv4 address",
                    target_host
                ))
            })?;
        match target_ip.ip() {
            std::net::IpAddr::V4(ipv4) => ipv4.octets(),
            _ => {
                return Err(DownloadError::Other(
                    "SOCKS4 requires IPv4 address".to_string(),
                ));
            }
        }
    };

    // SOCKS4 CONNECT request
    let mut req = vec![
        0x04, // VN
        0x01, // CD = CONNECT
        (target_port >> 8) as u8,
        (target_port & 0xFF) as u8,
        ip_bytes[0],
        ip_bytes[1],
        ip_bytes[2],
        ip_bytes[3],
        0x00, // USERID (null-terminated empty string)
    ];
    if remote_dns {
        // 目标域名以 NUL 结尾追加在 USERID 之后，由代理解析（合法性已在建连
        // 之前校验）。
        req.extend_from_slice(target_host.as_bytes());
        req.push(0x00);
    }

    stream
        .write_all(&req)
        .map_err(|e| DownloadError::Other(format!("SOCKS4 request write error: {}", e)))?;

    // Read response (8 bytes)
    let mut resp = [0u8; 8];
    stream
        .read_exact(&mut resp)
        .map_err(|e| DownloadError::Other(format!("SOCKS4 response read error: {}", e)))?;

    // resp[0] = 0x00 (VN), resp[1] = status
    if resp[1] != 0x5A {
        let err_msg = match resp[1] {
            0x5B => "request rejected or failed",
            0x5C => "request failed because client is not running identd",
            0x5D => "request failed because identd could not confirm the user ID",
            _ => "unknown error",
        };
        return Err(DownloadError::Other(format!(
            "SOCKS4 connect failed: {} (0x{:02x})",
            err_msg, resp[1]
        )));
    }

    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();

    Ok(stream)
}

/// Convenience: connect through SOCKS4 / SOCKS4a / SOCKS5 based on proxy config.
pub fn socks_connect_sync(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    match proxy.proxy_type {
        ProxyType::Socks5 => socks5_connect_sync(
            &proxy.host,
            proxy.port,
            target_host,
            target_port,
            &proxy.username,
            &proxy.password,
            timeout,
        ),
        ProxyType::Socks4 | ProxyType::Socks4a => socks4_connect_sync(
            &proxy.host,
            proxy.port,
            target_host,
            target_port,
            proxy.proxy_type == ProxyType::Socks4a,
            timeout,
        ),
        _ => Err(DownloadError::Other(format!(
            "socks_connect_sync called with non-SOCKS proxy type: {}",
            proxy.proxy_type.as_str()
        ))),
    }
}

/// Connect through an HTTP CONNECT proxy (for tunneling FTP control connections).
///
/// Sends `CONNECT host:port HTTP/1.1` to the proxy and validates the 200 response.
pub fn http_connect_proxy_sync(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let proxy_addr = format!("{}:{}", proxy.host, proxy.port);
    let sock_addr: std::net::SocketAddr = proxy_addr.parse().or_else(|_| {
        proxy_addr
            .to_socket_addrs()
            .map_err(|e| DownloadError::Other(format!("proxy DNS resolve error: {}", e)))?
            .next()
            .ok_or_else(|| DownloadError::Other("proxy DNS returned no addresses".to_string()))
    })?;

    let stream = TcpStream::connect_timeout(&sock_addr, timeout)
        .map_err(|e| DownloadError::Other(format!("HTTP CONNECT proxy connect error: {}", e)))?;

    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let target = format!("{}:{}", target_host, target_port);

    // Build CONNECT request
    let mut req = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n", target, target);
    if !proxy.username.is_empty() {
        use std::fmt::Write as FmtWrite;
        let credentials = format!("{}:{}", proxy.username, proxy.password);
        let encoded = base64_encode(credentials.as_bytes());
        let _ = write!(req, "Proxy-Authorization: Basic {}\r\n", encoded);
    }
    req.push_str("\r\n");

    let mut stream = stream;
    stream
        .write_all(req.as_bytes())
        .map_err(|e| DownloadError::Other(format!("HTTP CONNECT write error: {}", e)))?;

    // Read the response header block byte-by-byte, stopping exactly at the
    // `\r\n\r\n` terminator.  We deliberately avoid `BufReader` here: it would
    // greedily read past the header terminator and buffer whatever bytes follow,
    // then `into_inner()` would silently discard that buffer.  When this tunnel
    // carries an FTP control connection, the server's `220` welcome banner can
    // arrive in the *same* TCP segment as the proxy's CONNECT response, so those
    // banner bytes would be lost — leaving suppaftp's handshake to hang until
    // the read timeout.  Reading one byte at a time guarantees the kernel socket
    // buffer keeps everything after the header for the next reader.
    //
    // Upper bound on the proxy response header size; generous enough for any
    // realistic CONNECT response while bounding memory and read iterations
    // against a misbehaving proxy that never sends the terminator.
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let mut header = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .map_err(|e| DownloadError::Other(format!("HTTP CONNECT read error: {}", e)))?;
        if n == 0 {
            return Err(DownloadError::Other(
                "HTTP CONNECT proxy closed connection before sending full response".to_string(),
            ));
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() >= MAX_HEADER_BYTES {
            return Err(DownloadError::Other(
                "HTTP CONNECT response header exceeded maximum size".to_string(),
            ));
        }
    }

    // Parse the status code from the first line
    // (e.g. "HTTP/1.1 200 Connection established").
    let (status_code, status_line) = parse_connect_status_line(&header);
    if status_code != 200 {
        return Err(DownloadError::Other(format!(
            "HTTP CONNECT failed: {}",
            status_line.trim()
        )));
    }

    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();

    Ok(stream)
}

/// Parse the HTTP status code from a CONNECT response header block.
///
/// Returns the numeric status code (0 if unparseable) and the first line of the
/// header (the status line) as a lossy UTF-8 string for diagnostics. Expects a
/// header such as `b"HTTP/1.1 200 Connection established\r\n...\r\n\r\n"`.
fn parse_connect_status_line(header: &[u8]) -> (u16, String) {
    let status_line: &[u8] = header.split(|&b| b == b'\n').next().unwrap_or(header);
    let status_line_str = String::from_utf8_lossy(status_line).into_owned();
    let status_code = status_line_str
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status_code, status_line_str)
}

/// Simple base64 encoder (avoids external dependency for a single use).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let chunks = data.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Connect to a target through the proxy for FTP control connection.
/// Dispatches to SOCKS or HTTP CONNECT based on proxy type.
pub fn proxy_connect_sync(
    proxy: &ProxyConfig,
    target_host: &str,
    target_port: u16,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    match proxy.proxy_type {
        ProxyType::Socks4 | ProxyType::Socks4a | ProxyType::Socks5 => {
            socks_connect_sync(proxy, target_host, target_port, timeout)
        }
        ProxyType::Http | ProxyType::Https => {
            http_connect_proxy_sync(proxy, target_host, target_port, timeout)
        }
    }
}

// ---------------------------------------------------------------------------
// Proxy connectivity test
// ---------------------------------------------------------------------------

/// Connectivity check endpoints — tried in order until one succeeds.
/// Using multiple providers avoids false negatives when a specific service
/// is unreachable (e.g. Google blocked in certain regions).
const CONNECTIVITY_CHECK_URLS: &[&str] = &[
    "http://www.msftconnecttest.com/connecttest.txt", // Microsoft — widely accessible
    "http://cp.cloudflare.com",                       // Cloudflare
    "http://connectivitycheck.gstatic.com/generate_204", // Google
];

/// Stable English wire message for "HTTPS selected, but the endpoint is not a
/// TLS proxy". The display layer maps it per locale (see `translateBackendMessage`).
pub const PROXY_TLS_ENDPOINT_HINT: &str = concat!(
    "the proxy endpoint did not accept a TLS handshake; ",
    "if this is a mixed HTTP/SOCKS port (Clash, V2Ray) or a plain HTTP proxy, ",
    "select the HTTP type instead of HTTPS",
);

/// Recognise a failed TLS handshake *with the proxy itself*.
///
/// Only meaningful for [`ProxyType::Https`], where the client must complete TLS
/// with the endpoint before it can issue `CONNECT`. Pointing that type at a
/// plaintext endpoint yields a transport-level TLS error rather than an HTTP
/// status, because the endpoint answers the ClientHello with plain bytes, a
/// `400`-ish response, or an immediate close (issue #183).
fn is_proxy_tls_handshake_failure(proxy_type: &ProxyType, error: &str) -> bool {
    if *proxy_type != ProxyType::Https {
        return false;
    }
    let lower = error.to_ascii_lowercase();
    // Covers native-tls (desktop) and rustls (server) phrasings alike.
    [
        "tls",
        "ssl",
        "handshake",
        "certificate",
        "corrupt message",
        "unexpected eof",
        "invalid or unsupported protocol version",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Test proxy connectivity by sending HTTP requests through the proxy.
///
/// Tries multiple connectivity check endpoints (Microsoft, Cloudflare, Google)
/// in order — the first successful response determines the latency measurement.
/// This avoids false negatives when a specific provider is blocked.
///
/// Returns the latency in milliseconds on success, or a `DownloadError` on failure.
pub async fn test_proxy_connection(
    proxy_type: &str,
    proxy_host: &str,
    proxy_port: &str,
    proxy_username: &str,
    proxy_password: &str,
) -> Result<i64, DownloadError> {
    use std::time::Instant;

    // 类型无法识别时必须报错,不能悄悄按另一种协议去测:设置页那个「测试」
    // 按钮的全部价值就在于它测的是用户真正会用的那条链路,测出来的「通过」
    // 如果来自别的协议,比不测更有害。
    let proxy_type = ProxyType::parse_str(proxy_type)
        .ok_or_else(|| DownloadError::Other(format!("unsupported proxy type: {}", proxy_type)))?;
    let config = ProxyConfig {
        mode: ProxyMode::Manual,
        proxy_type,
        host: proxy_host.to_string(),
        port: proxy_port.parse::<u16>().unwrap_or(0),
        username: proxy_username.to_string(),
        password: proxy_password.to_string(),
        no_proxy_list: String::new(),
    };

    let proxy_url = config.to_proxy_url().ok_or_else(|| {
        DownloadError::Other("incomplete proxy config (host or port missing)".to_string())
    })?;

    // 只记 scheme + host:port:`proxy_url` 的 userinfo 段带明文凭据,而日志
    // 是用户提 issue 时会整份贴出来的东西(`build_client_inner` 同理)。
    log_info!(
        "[proxy-test] testing proxy: {}://{}:{}",
        config.proxy_type.scheme(),
        config.host,
        config.port
    );

    let mut proxy = reqwest::Proxy::all(&proxy_url)
        .map_err(|e| DownloadError::Other(format!("invalid proxy URL: {}", e)))?;

    if !proxy_username.is_empty() {
        proxy = proxy.basic_auth(proxy_username, proxy_password);
    }

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| DownloadError::Other(format!("failed to build test client: {}", e)))?;

    let mut last_err = String::new();

    for url in CONNECTIVITY_CHECK_URLS {
        log_info!("[proxy-test] trying: {}", url);
        let start = Instant::now();

        match client.head(*url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as i64;
                let status = resp.status();

                log_info!(
                    "[proxy-test] {} → status={}, latency={}ms",
                    url,
                    status,
                    latency,
                );

                // Any non-server-error response proves the proxy works.
                // 200, 204, 301/302 are all acceptable.
                if !status.is_server_error() {
                    return Ok(latency);
                }
                last_err = format!("{}: HTTP {}", url, status);
            }
            Err(e) => {
                log_info!("[proxy-test] {} → error: {}", url, e);
                // Chained sources carry the actual TLS error; `{}` alone would
                // only show reqwest's generic "error sending request" wrapper.
                let mut detail = e.to_string();
                let mut source = std::error::Error::source(&e);
                while let Some(inner) = source {
                    detail.push_str(": ");
                    detail.push_str(&inner.to_string());
                    source = inner.source();
                }
                last_err = format!("{}: {}", url, detail);
            }
        }
    }

    // A TLS failure against the endpoint itself means the type selector is
    // wrong, not that the proxy is down — say so instead of surfacing a raw
    // handshake error the user cannot act on.
    if is_proxy_tls_handshake_failure(&config.proxy_type, &last_err) {
        return Err(DownloadError::Other(PROXY_TLS_ENDPOINT_HINT.to_string()));
    }

    Err(DownloadError::Other(format!(
        "all connectivity checks failed, last: {}",
        last_err,
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{
        ProxyConfig, ProxyMode, ProxyType, base64_encode, is_proxy_tls_handshake_failure,
        parse_connect_status_line, parse_env_proxy_url, parse_host_port,
        parse_multi_protocol_proxy, parse_scutil_dict, parse_scutil_proxy_output,
        parse_windows_proxy_server, percent_decode, percent_encode_userinfo, socks4_connect_sync,
        strip_gsettings_quotes,
    };
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // ProxyMode
    // -----------------------------------------------------------------------

    #[test]
    fn proxy_mode_parse_str_roundtrip() {
        assert_eq!(ProxyMode::parse_str("none"), ProxyMode::None);
        assert_eq!(ProxyMode::parse_str("system"), ProxyMode::System);
        assert_eq!(ProxyMode::parse_str("manual"), ProxyMode::Manual);
        assert_eq!(ProxyMode::parse_str("auto"), ProxyMode::Auto);
        assert_eq!(ProxyMode::parse_str("unknown"), ProxyMode::None);
        assert_eq!(ProxyMode::parse_str(""), ProxyMode::None);
    }

    #[test]
    fn proxy_mode_as_str() {
        assert_eq!(ProxyMode::None.as_str(), "none");
        assert_eq!(ProxyMode::System.as_str(), "system");
        assert_eq!(ProxyMode::Manual.as_str(), "manual");
        assert_eq!(ProxyMode::Auto.as_str(), "auto");
    }

    // -----------------------------------------------------------------------
    // ProxyType
    // -----------------------------------------------------------------------

    /// 识别集必须覆盖 reqwest 接受的每一个代理 scheme,识别不了的一律 `None`
    /// ——绝不静默折算成 Http(那会把 SOCKS 端口当 HTTP 代理用)。
    #[test]
    fn proxy_type_parse_str_recognizes_every_reqwest_scheme() {
        assert_eq!(ProxyType::parse_str("http"), Some(ProxyType::Http));
        assert_eq!(ProxyType::parse_str("https"), Some(ProxyType::Https));
        assert_eq!(ProxyType::parse_str("socks4"), Some(ProxyType::Socks4));
        assert_eq!(ProxyType::parse_str("socks4a"), Some(ProxyType::Socks4a));
        assert_eq!(ProxyType::parse_str("socks5"), Some(ProxyType::Socks5));
        assert_eq!(ProxyType::parse_str("socks5h"), Some(ProxyType::Socks5));
    }

    #[test]
    fn proxy_type_parse_str_rejects_instead_of_defaulting_to_http() {
        assert_eq!(ProxyType::parse_str("unknown"), None);
        assert_eq!(ProxyType::parse_str(""), None);
        assert_eq!(ProxyType::parse_str("SOCKS5"), None);
        assert_eq!(ProxyType::parse_str("socks"), None);
    }

    /// 每个变体的 wire 值都必须能被自己解析回来,否则往返一圈就换了协议。
    #[test]
    fn proxy_type_as_str_parses_back_to_itself() {
        for t in [
            ProxyType::Http,
            ProxyType::Https,
            ProxyType::Socks4,
            ProxyType::Socks4a,
            ProxyType::Socks5,
        ] {
            assert_eq!(ProxyType::parse_str(t.as_str()), Some(t.clone()), "{:?}", t);
            assert_eq!(ProxyType::parse_str(t.scheme()), Some(t.clone()), "{:?}", t);
        }
    }

    #[test]
    fn proxy_type_scheme() {
        assert_eq!(ProxyType::Http.scheme(), "http");
        assert_eq!(ProxyType::Https.scheme(), "https");
        assert_eq!(ProxyType::Socks4.scheme(), "socks4");
        assert_eq!(ProxyType::Socks4a.scheme(), "socks4a");
        // 代理侧 DNS 解析,见 `ProxyType::scheme` 文档。
        assert_eq!(ProxyType::Socks5.scheme(), "socks5h");
    }

    /// scheme(reqwest URL)与 as_str(持久化 wire 值)必须保持解耦:
    /// UI 下拉/DB/Dart 侧的 `proxy_type` 恒为 `socks5`,绝不能漂成 `socks5h`。
    #[test]
    fn proxy_type_as_str_is_not_the_reqwest_scheme() {
        assert_eq!(ProxyType::Socks5.as_str(), "socks5");
        assert_ne!(ProxyType::Socks5.as_str(), ProxyType::Socks5.scheme());
        for t in [
            ProxyType::Http,
            ProxyType::Https,
            ProxyType::Socks4,
            ProxyType::Socks4a,
        ] {
            assert_eq!(t.as_str(), t.scheme());
        }
    }

    #[test]
    fn proxy_type_is_socks() {
        assert!(!ProxyType::Http.is_socks());
        assert!(!ProxyType::Https.is_socks());
        assert!(ProxyType::Socks4.is_socks());
        assert!(ProxyType::Socks4a.is_socks());
        assert!(ProxyType::Socks5.is_socks());
    }

    // -----------------------------------------------------------------------
    // ProxyConfig
    // -----------------------------------------------------------------------

    #[test]
    fn proxy_config_default_is_none() {
        let config = ProxyConfig::default();
        assert_eq!(config.mode, ProxyMode::None);
        assert!(!config.is_active());
        assert!(config.to_proxy_url().is_none());
    }

    #[test]
    fn proxy_config_from_config_map_empty() {
        let map = HashMap::new();
        let config = ProxyConfig::from_config_map(&map);
        assert_eq!(config.mode, ProxyMode::None);
        assert_eq!(config.proxy_type, ProxyType::Http);
        assert!(config.host.is_empty());
        assert_eq!(config.port, 0);
    }

    #[test]
    fn proxy_config_from_config_map_full() {
        let mut map = HashMap::new();
        map.insert("proxy_mode".to_string(), "manual".to_string());
        map.insert("proxy_type".to_string(), "socks5".to_string());
        map.insert("proxy_host".to_string(), "127.0.0.1".to_string());
        map.insert("proxy_port".to_string(), "1080".to_string());
        map.insert("proxy_username".to_string(), "user".to_string());
        map.insert("proxy_password".to_string(), "pass".to_string());
        map.insert("proxy_no_list".to_string(), "localhost,*.local".to_string());

        let config = ProxyConfig::from_config_map(&map);
        assert_eq!(config.mode, ProxyMode::Manual);
        assert_eq!(config.proxy_type, ProxyType::Socks5);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 1080);
        assert_eq!(config.username, "user");
        assert_eq!(config.password, "pass");
        assert_eq!(config.no_proxy_list, "localhost,*.local");
        assert!(config.is_active());
        assert!(config.is_socks());
    }

    #[test]
    fn proxy_config_to_proxy_url_none() {
        let config = ProxyConfig::default();
        assert!(config.to_proxy_url().is_none());
    }

    #[test]
    fn proxy_config_to_proxy_url_system() {
        let config = ProxyConfig {
            mode: ProxyMode::System,
            ..ProxyConfig::default()
        };
        // System mode resolves URL at runtime, not statically
        assert!(config.to_proxy_url().is_none());
    }

    #[test]
    fn proxy_config_to_proxy_url_manual_no_auth() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "proxy.example.com".to_string(),
            port: 8080,
            ..ProxyConfig::default()
        };
        assert_eq!(
            config.to_proxy_url().as_deref(),
            Some("http://proxy.example.com:8080")
        );
    }

    #[test]
    fn proxy_config_to_proxy_url_manual_with_auth() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Socks5,
            host: "socks.example.com".to_string(),
            port: 1080,
            username: "admin".to_string(),
            password: "secret".to_string(),
            no_proxy_list: String::new(),
        };
        assert_eq!(
            config.to_proxy_url().as_deref(),
            Some("socks5h://admin:secret@socks.example.com:1080")
        );
    }

    /// 用户/外部 API 手写 `socks5h://` 时必须解析回 Socks5,再序列化仍是
    /// `socks5h`——否则 `tasks.proxy_url` 往返一圈会退化成 HTTP CONNECT。
    #[test]
    fn socks5h_url_roundtrips_through_from_proxy_url() {
        let c = ProxyConfig::from_proxy_url("socks5h://user:pass@127.0.0.1:1080");
        assert_eq!(c.mode, ProxyMode::Manual);
        assert_eq!(c.proxy_type, ProxyType::Socks5);
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 1080);
        assert_eq!(c.username, "user");
        assert_eq!(c.password, "pass");
        assert_eq!(
            c.to_proxy_url().as_deref(),
            Some("socks5h://user:pass@127.0.0.1:1080")
        );
    }

    /// UI 侧仍然只会写出 `socks5://`(下拉值 = `as_str`),引擎必须把它归一化
    /// 成代理侧解析——这条链路才是手动 SOCKS5 代理的实际生产路径。
    #[test]
    fn plain_socks5_task_url_is_normalized_to_remote_dns() {
        let c = ProxyConfig::from_proxy_url("socks5://127.0.0.1:3067");
        assert_eq!(c.proxy_type, ProxyType::Socks5);
        assert_eq!(
            c.to_proxy_url().as_deref(),
            Some("socks5h://127.0.0.1:3067")
        );
    }

    /// `socks4a://` 曾被 `_ => Http` 通配吞成 HTTP 代理——对着 SOCKS 端口发
    /// HTTP CONNECT,失败得毫无线索。现在必须原样兑现远端解析语义。
    #[test]
    fn socks4a_url_is_honored_not_downgraded() {
        let c = ProxyConfig::from_proxy_url("socks4a://127.0.0.1:9050");
        assert_eq!(c.proxy_type, ProxyType::Socks4a);
        assert!(c.is_socks());
        assert_eq!(
            c.to_proxy_url().as_deref(),
            Some("socks4a://127.0.0.1:9050")
        );
    }

    /// SOCKS4/SOCKS4a 请求的线格式(手写握手,没有库兜底):4a 用非法 DSTIP
    /// `0.0.0.1` 当信号、并在 USERID 的 NUL 之后追加以 NUL 结尾的目标域名;
    /// 非 4a 必须本地解析出真实 IPv4 且不追加任何东西。字节错一位代理就把
    /// 整条请求当垃圾丢掉。
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn socks4_request_wire_format_matches_the_dns_mode() {
        use std::io::{Read, Write};

        /// stub 代理捕获到的一条请求。
        struct Request {
            /// 定长部分:VN CD DSTPORT(2) DSTIP(4) USERID-NUL。
            head: [u8; 9],
            /// 定长部分之后的字节(4a 的域名 + NUL;非 4a 应为空)。
            tail: Vec<u8>,
        }
        struct Stub {
            port: u16,
            handle: std::thread::JoinHandle<Request>,
        }

        /// 收一条请求、回一个成功应答,把收到的字节交回来。头部 9 字节定长;
        /// 之后再单读一次——4a 的域名与头部同一个 write 发出,必然已在缓冲区,
        /// 非 4a 则读到超时返回空。
        fn stub_socks4() -> Stub {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub socks4");
            let port = listener.local_addr().expect("stub addr").port();
            let handle = std::thread::spawn(move || {
                let (mut sock, _) = listener.accept().expect("accept from client");
                sock.set_read_timeout(Some(std::time::Duration::from_millis(300)))
                    .ok();
                let mut head = [0u8; 9];
                sock.read_exact(&mut head).expect("read request head");
                let mut buf = [0u8; 256];
                let n = sock.read(&mut buf).unwrap_or(0);
                // 0x5A = request granted
                sock.write_all(&[0, 0x5A, 0, 0, 0, 0, 0, 0])
                    .expect("write reply");
                Request {
                    head,
                    tail: buf[..n].to_vec(),
                }
            });
            Stub { port, handle }
        }

        let timeout = std::time::Duration::from_secs(5);

        // SOCKS4a:域名原样上送,DSTIP 是 0.0.0.1 哨兵。
        let stub = stub_socks4();
        socks4_connect_sync(
            "127.0.0.1",
            stub.port,
            "blocked.example.invalid",
            8080,
            true,
            timeout,
        )
        .expect("socks4a connect");
        let req = stub.handle.join().expect("stub thread");
        assert_eq!(&req.head[..2], &[0x04, 0x01], "VN=4 CD=CONNECT");
        assert_eq!(&req.head[2..4], &8080u16.to_be_bytes(), "DSTPORT 网络序");
        assert_eq!(&req.head[4..8], &[0, 0, 0, 1], "4a 哨兵 DSTIP");
        assert_eq!(req.head[8], 0, "空 USERID 的终止 NUL");
        assert_eq!(req.tail, b"blocked.example.invalid\0", "域名交给代理解析");

        // SOCKS4:本地解析出 IPv4 下发,请求到 USERID 的 NUL 为止。
        let stub = stub_socks4();
        socks4_connect_sync("127.0.0.1", stub.port, "127.0.0.1", 8080, false, timeout)
            .expect("socks4 connect");
        let req = stub.handle.join().expect("stub thread");
        assert_eq!(&req.head[4..8], &[127, 0, 0, 1], "本地解析出的真实 IPv4");
        assert!(req.tail.is_empty(), "非 4a 不得追加域名");
    }

    /// 4a 的域名走裸字节 + NUL 终止,非 ASCII 会被代理按字节乱解、含 NUL 会
    /// 直接截断整条请求——两者都必须在发包前拒绝,而不是发一条畸形请求。
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn socks4a_rejects_hosts_it_cannot_encode() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let timeout = std::time::Duration::from_secs(5);
        for bad in ["测试.example.com", "a\0b.example.com", ""] {
            let err = socks4_connect_sync("127.0.0.1", port, bad, 80, true, timeout)
                .expect_err("必须拒绝");
            assert!(
                err.to_string().contains("SOCKS4a"),
                "错误要指明是 SOCKS4a 编码问题: {}",
                err
            );
        }
    }

    /// 手动 SOCKS5 代理的**端到端行为契约**:生产路径构建出来的 client 必须
    /// 把目标 hostname 原样交给代理解析(RFC 1928 §4 ATYP=0x03 DOMAINNAME),
    /// 而不是本地解析后只把 IP 送过去。DNS 被投毒/封锁的域名下,本地解析拿到
    /// 的地址是错的,代理再通也救不回来。
    ///
    /// stub 代理只做 RFC 1928 问候 + 读一条 CONNECT 请求,读完即断——断言只
    /// 看代理收到了什么,不需要真实出网。目标域名取不可解析的 `.invalid`
    /// (RFC 2606):本地解析路径根本到不了代理,accept 超时即判定回归。
    #[tokio::test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    async fn manual_socks5_client_hands_hostname_to_the_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub socks5 listener");
        let proxy_port = listener.local_addr().expect("stub listener addr").port();

        let stub = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept from client");
            // 问候:VER NMETHODS METHODS... → 选“无认证”。
            let mut head = [0u8; 2];
            sock.read_exact(&mut head).await.expect("read greeting");
            let mut methods = vec![0u8; head[1] as usize];
            sock.read_exact(&mut methods).await.expect("read methods");
            sock.write_all(&[0x05, 0x00]).await.expect("write choice");
            // 请求:VER CMD RSV ATYP DST.ADDR DST.PORT。
            let mut req = [0u8; 4];
            sock.read_exact(&mut req).await.expect("read request head");
            let atyp = req[3];
            let addr = match atyp {
                0x01 => {
                    let mut v4 = [0u8; 4];
                    sock.read_exact(&mut v4).await.expect("read ipv4");
                    std::net::Ipv4Addr::from(v4).to_string()
                }
                0x03 => {
                    let mut len = [0u8; 1];
                    sock.read_exact(&mut len).await.expect("read domain len");
                    let mut name = vec![0u8; len[0] as usize];
                    sock.read_exact(&mut name).await.expect("read domain");
                    String::from_utf8_lossy(&name).into_owned()
                }
                other => format!("unsupported atyp {}", other),
            };
            (atyp, addr)
        });

        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_string(),
            port: proxy_port,
            username: String::new(),
            password: String::new(),
            no_proxy_list: String::new(),
        };
        let client = crate::downloader::build_client_with_tls_policy(&config, "", false)
            .expect("build manual socks5 client");
        // 请求必然失败(stub 不回 reply),只用来驱动一次代理握手。
        let driver = tokio::spawn(async move {
            client
                .get("http://poisoned.example.invalid/seg0")
                .send()
                .await
        });

        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), stub)
            .await
            .expect("代理未收到任何连接:目标域名被本地解析吞掉了(socks5 而非 socks5h)")
            .expect("stub socks5 task panicked");
        driver.abort();

        assert_eq!(observed.0, 0x03, "SOCKS5 请求必须用 DOMAINNAME 地址类型");
        assert_eq!(observed.1, "poisoned.example.invalid");
    }

    #[test]
    fn proxy_config_to_proxy_url_manual_empty_host() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: String::new(),
            port: 8080,
            ..ProxyConfig::default()
        };
        assert!(config.to_proxy_url().is_none());
    }

    #[test]
    fn proxy_config_to_proxy_url_manual_zero_port() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "proxy.com".to_string(),
            port: 0,
            ..ProxyConfig::default()
        };
        assert!(config.to_proxy_url().is_none());
    }

    #[test]
    fn proxy_config_addr() {
        let config = ProxyConfig {
            host: "127.0.0.1".to_string(),
            port: 1080,
            ..ProxyConfig::default()
        };
        assert_eq!(config.addr(), "127.0.0.1:1080");
    }

    // -----------------------------------------------------------------------
    // from_proxy_url
    // -----------------------------------------------------------------------

    #[test]
    fn from_proxy_url_empty() {
        let c = ProxyConfig::from_proxy_url("");
        assert_eq!(c.mode, ProxyMode::None);
    }

    #[test]
    fn from_proxy_url_direct_sentinel_forces_plain_connection() {
        let c = ProxyConfig::from_proxy_url(ProxyConfig::DIRECT_SENTINEL);
        assert_eq!(c.mode, ProxyMode::None);
        assert!(c.host.is_empty());
        // resolve() 幂等：直连保持直连。
        assert_eq!(c.resolve().mode, ProxyMode::None);
    }

    #[test]
    fn from_proxy_url_system_sentinel_resolves_like_system_mode() {
        let c = ProxyConfig::from_proxy_url(ProxyConfig::SYSTEM_SENTINEL);
        assert_eq!(c.mode, ProxyMode::System);
        // resolve() 具体化：检测到 → Manual（真实地址），未检测到 → 直连；
        // 绝不把 System 原样漏给 FTP/CDN 门槛。
        let r = c.resolve();
        assert_ne!(r.mode, ProxyMode::System);
    }

    #[test]
    fn from_proxy_url_socks5_with_auth() {
        let c = ProxyConfig::from_proxy_url("socks5://user:pass@127.0.0.1:1080");
        assert_eq!(c.mode, ProxyMode::Manual);
        assert_eq!(c.proxy_type, ProxyType::Socks5);
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 1080);
        assert_eq!(c.username, "user");
        assert_eq!(c.password, "pass");
    }

    #[test]
    fn from_proxy_url_http_no_auth() {
        let c = ProxyConfig::from_proxy_url("http://proxy.example.com:8080");
        assert_eq!(c.mode, ProxyMode::Manual);
        assert_eq!(c.proxy_type, ProxyType::Http);
        assert_eq!(c.host, "proxy.example.com");
        assert_eq!(c.port, 8080);
        assert!(c.username.is_empty());
        assert!(c.password.is_empty());
    }

    #[test]
    fn from_proxy_url_no_scheme() {
        let c = ProxyConfig::from_proxy_url("10.0.0.1:3128");
        assert_eq!(c.proxy_type, ProxyType::Http);
        assert_eq!(c.host, "10.0.0.1");
        assert_eq!(c.port, 3128);
    }

    #[test]
    fn from_proxy_url_socks4() {
        let c = ProxyConfig::from_proxy_url("socks4://myproxy:9050");
        assert_eq!(c.proxy_type, ProxyType::Socks4);
        assert_eq!(c.host, "myproxy");
        assert_eq!(c.port, 9050);
    }

    // -----------------------------------------------------------------------
    // parse_host_port
    // -----------------------------------------------------------------------

    #[test]
    fn parse_host_port_standard() {
        let (scheme, h, p) = parse_host_port("proxy.com:8080");
        assert_eq!(scheme, None);
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_no_port_defaults_8080() {
        let (_scheme, h, p) = parse_host_port("proxy.com");
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_empty() {
        let (_scheme, h, p) = parse_host_port("");
        assert!(h.is_empty());
        assert_eq!(p, 0);
    }

    #[test]
    fn parse_host_port_ipv6() {
        let (_scheme, h, p) = parse_host_port("[::1]:8080");
        assert_eq!(h, "[::1]");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_ipv6_no_port() {
        let (_scheme, h, p) = parse_host_port("[::1]");
        assert_eq!(h, "[::1]");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_invalid_port() {
        let (_scheme, h, p) = parse_host_port("proxy.com:abc");
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080); // defaults to 8080
    }

    // BUG-PROXY-SCHEME-PREFIX：一个本应是裸 host:port 的字段实际携带了
    // `http://` scheme 前缀（如 Windows ProxyServer 注册表值被第三方工具
    // 改写）——必须剥离 scheme，不能让 `rfind(':')` 把它并入 host。
    #[test]
    fn parse_host_port_strips_http_scheme() {
        let (scheme, h, p) = parse_host_port("http://127.0.0.1:20122");
        assert_eq!(scheme, None);
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 20122);
    }

    #[test]
    fn parse_host_port_plain_unchanged() {
        let (scheme, h, p) = parse_host_port("127.0.0.1:20122");
        assert_eq!(scheme, None);
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 20122);
    }

    #[test]
    fn parse_host_port_strips_socks5_scheme_and_reports_type() {
        let (scheme, h, p) = parse_host_port("socks5://h:1080");
        assert_eq!(scheme, Some(ProxyType::Socks5));
        assert_eq!(h, "h");
        assert_eq!(p, 1080);
    }

    #[test]
    fn parse_windows_proxy_simple_with_scheme_prefix_does_not_double_scheme() {
        // BUG-PROXY-SCHEME-PREFIX 端到端：从 ProxyServer 值到最终代理 URL 全程
        // 不出现重复 scheme。
        let (ty, host, port) = parse_windows_proxy_server("http://127.0.0.1:20122");
        assert_eq!(ty, ProxyType::Http);
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 20122);
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ty,
            host,
            port,
            username: String::new(),
            password: String::new(),
            no_proxy_list: String::new(),
        };
        assert_eq!(
            config.to_proxy_url().as_deref(),
            Some("http://127.0.0.1:20122")
        );
    }

    // -----------------------------------------------------------------------
    // parse_multi_protocol_proxy
    // -----------------------------------------------------------------------

    #[test]
    fn parse_multi_protocol_basic() {
        let result = parse_multi_protocol_proxy("http=proxy.com:80;https=proxy.com:443");
        assert_eq!(result.get("http"), Some(&("proxy.com".to_string(), 80)));
        assert_eq!(result.get("https"), Some(&("proxy.com".to_string(), 443)));
    }

    #[test]
    fn parse_multi_protocol_with_socks() {
        let result = parse_multi_protocol_proxy("http=a:80;socks=b:1080");
        assert_eq!(result.get("socks"), Some(&("b".to_string(), 1080)));
    }

    #[test]
    fn parse_multi_protocol_empty() {
        let result = parse_multi_protocol_proxy("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_multi_protocol_with_spaces() {
        let result = parse_multi_protocol_proxy(" http = proxy.com:80 ; https = proxy.com:443 ");
        assert_eq!(result.get("http"), Some(&("proxy.com".to_string(), 80)));
    }

    // -----------------------------------------------------------------------
    // parse_windows_proxy_server
    // -----------------------------------------------------------------------

    #[test]
    fn parse_windows_proxy_simple() {
        let (ty, host, port) = parse_windows_proxy_server("proxy.com:8080");
        assert_eq!(ty, ProxyType::Http);
        assert_eq!(host, "proxy.com");
        assert_eq!(port, 8080);
    }

    #[test]
    fn parse_windows_proxy_multi_prefers_socks() {
        let (ty, host, port) = parse_windows_proxy_server("http=a:80;https=b:443;socks=c:1080");
        assert_eq!(ty, ProxyType::Socks5);
        assert_eq!(host, "c");
        assert_eq!(port, 1080);
    }

    /// Issue #183: the registry's `https=` entry is still selected by priority,
    /// but its transport is plaintext HTTP `CONNECT` — emitting
    /// [`ProxyType::Https`] here would make reqwest attempt TLS with the proxy.
    #[test]
    fn parse_windows_proxy_multi_prefers_https_over_http() {
        let (ty, host, port) = parse_windows_proxy_server("http=a:80;https=b:443");
        assert_eq!(ty, ProxyType::Http);
        assert_eq!(host, "b");
        assert_eq!(port, 443);
    }

    #[test]
    fn parse_windows_proxy_https_entry_builds_plaintext_transport_url() {
        let (proxy_type, host, port) = parse_windows_proxy_server("https=127.0.0.1:7897");
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type,
            host,
            port,
            ..ProxyConfig::default()
        };
        assert_eq!(
            config.to_proxy_url().as_deref(),
            Some("http://127.0.0.1:7897")
        );
    }

    // -----------------------------------------------------------------------
    // Proxy-endpoint TLS failure detection (issue #183)
    // -----------------------------------------------------------------------

    #[test]
    fn tls_handshake_failure_detected_for_https_type() {
        assert!(is_proxy_tls_handshake_failure(
            &ProxyType::Https,
            "error sending request: tls handshake eof",
        ));
        assert!(is_proxy_tls_handshake_failure(
            &ProxyType::Https,
            "received corrupt message of type Handshake",
        ));
    }

    /// A plaintext type never performs TLS with the endpoint, so an unrelated
    /// certificate error from the *destination* must not be relabelled.
    #[test]
    fn tls_handshake_failure_not_reported_for_plaintext_types() {
        assert!(!is_proxy_tls_handshake_failure(
            &ProxyType::Http,
            "error sending request: tls handshake eof",
        ));
        assert!(!is_proxy_tls_handshake_failure(
            &ProxyType::Socks5,
            "certificate verify failed",
        ));
    }

    #[test]
    fn non_tls_errors_are_left_alone() {
        assert!(!is_proxy_tls_handshake_failure(
            &ProxyType::Https,
            "connection refused",
        ));
    }

    #[test]
    fn parse_windows_proxy_multi_http_only() {
        let (ty, host, port) = parse_windows_proxy_server("http=a:80");
        assert_eq!(ty, ProxyType::Http);
        assert_eq!(host, "a");
        assert_eq!(port, 80);
    }

    // -----------------------------------------------------------------------
    // base64_encode
    // -----------------------------------------------------------------------

    #[test]
    fn base64_encode_basic() {
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_padding() {
        // "a" → 1 byte → needs 2 padding
        assert_eq!(base64_encode(b"a"), "YQ==");
        // "ab" → 2 bytes → needs 1 padding
        assert_eq!(base64_encode(b"ab"), "YWI=");
        // "abc" → 3 bytes → no padding
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    // -----------------------------------------------------------------------
    // percent_decode / percent_encode_userinfo
    // -----------------------------------------------------------------------

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("p%40ss"), "p@ss");
        assert_eq!(percent_decode("no%2Fslash"), "no/slash");
    }

    #[test]
    fn percent_decode_passthrough() {
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode(""), "");
        // Incomplete percent at end — passed through
        assert_eq!(percent_decode("test%"), "test%");
    }

    #[test]
    fn percent_encode_userinfo_special_chars() {
        assert_eq!(percent_encode_userinfo("user@host"), "user%40host");
        assert_eq!(percent_encode_userinfo("pass:word"), "pass%3Aword");
        assert_eq!(percent_encode_userinfo("a/b"), "a%2Fb");
        assert_eq!(percent_encode_userinfo("hello world"), "hello%20world");
    }

    #[test]
    fn percent_encode_userinfo_safe_chars() {
        // Unreserved chars should NOT be encoded
        assert_eq!(percent_encode_userinfo("abc-._~"), "abc-._~");
        assert_eq!(percent_encode_userinfo("ABC123"), "ABC123");
    }

    #[test]
    fn percent_encode_decode_roundtrip() {
        let original = "user@host:p@ss/w0rd";
        let encoded = percent_encode_userinfo(original);
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, original);
    }

    // -----------------------------------------------------------------------
    // from_proxy_url — URL-encoded credentials
    // -----------------------------------------------------------------------

    #[test]
    fn from_proxy_url_encoded_password_with_at() {
        // Password contains '@' which is percent-encoded
        let c = ProxyConfig::from_proxy_url("socks5://user:p%40ss@127.0.0.1:1080");
        assert_eq!(c.username, "user");
        assert_eq!(c.password, "p@ss");
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 1080);
    }

    #[test]
    fn from_proxy_url_encoded_username_and_password() {
        let c = ProxyConfig::from_proxy_url("http://u%40ser:p%3Ass@proxy.com:8080");
        assert_eq!(c.username, "u@ser");
        assert_eq!(c.password, "p:ss");
    }

    // -----------------------------------------------------------------------
    // to_proxy_url — encoding special characters
    // -----------------------------------------------------------------------

    #[test]
    fn to_proxy_url_encodes_special_chars_in_credentials() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Socks5,
            host: "proxy.com".to_string(),
            port: 1080,
            username: "user@domain".to_string(),
            password: "p@ss:word".to_string(),
            no_proxy_list: String::new(),
        };
        let url = config.to_proxy_url();
        assert!(url.is_some());
        let url = url.unwrap_or_default();
        // '@' and ':' in credentials must be percent-encoded
        assert!(url.contains("user%40domain"));
        assert!(url.contains("p%40ss%3Aword"));
        assert!(url.starts_with("socks5h://"));
        assert!(url.ends_with("@proxy.com:1080"));
    }

    #[test]
    fn to_proxy_url_from_proxy_url_roundtrip_with_special_chars() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "10.0.0.1".to_string(),
            port: 3128,
            username: "admin@corp".to_string(),
            password: "s3cr3t!".to_string(),
            no_proxy_list: String::new(),
        };
        let url = config.to_proxy_url().unwrap_or_default();
        let parsed = ProxyConfig::from_proxy_url(&url);
        assert_eq!(parsed.username, "admin@corp");
        assert_eq!(parsed.password, "s3cr3t!");
        assert_eq!(parsed.host, "10.0.0.1");
        assert_eq!(parsed.port, 3128);
    }

    // -----------------------------------------------------------------------
    // resolve()
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_none_mode_returns_self() {
        let config = ProxyConfig::default();
        let resolved = config.resolve();
        assert_eq!(resolved.mode, ProxyMode::None);
    }

    #[test]
    fn resolve_manual_mode_returns_self() {
        let config = ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_string(),
            port: 1080,
            ..ProxyConfig::default()
        };
        let resolved = config.resolve();
        assert_eq!(resolved.mode, ProxyMode::Manual);
        assert_eq!(resolved.host, "127.0.0.1");
        assert_eq!(resolved.port, 1080);
    }

    #[test]
    fn resolve_system_mode_does_not_panic() {
        let config = ProxyConfig {
            mode: ProxyMode::System,
            ..ProxyConfig::default()
        };
        // Should not panic regardless of system config
        let resolved = config.resolve();
        // Result depends on OS config — just verify it resolved to
        // either Manual (with populated fields) or None (system proxy disabled).
        assert!(resolved.mode == ProxyMode::Manual || resolved.mode == ProxyMode::None);
    }

    // -----------------------------------------------------------------------
    // System proxy detection
    // -----------------------------------------------------------------------

    #[cfg(target_os = "windows")]
    #[test]
    fn detect_system_proxy_does_not_panic() {
        // Just ensure it doesn't crash — result depends on user's system config
        let result = super::detect_system_proxy();
        assert!(result.is_ok());
    }

    // macOS/Linux 探测结果取决于当前测试机的真实代理配置（scutil / 环境
    // 变量 / gsettings），不能断言具体值——只验证探测流程本身不会 panic
    // 或返回 Err（子进程失败/命令缺失必须走 Ok(None) 兜底，而不是冒泡错误）。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn detect_system_proxy_does_not_panic_on_macos_linux() {
        let result = super::detect_system_proxy();
        assert!(result.is_ok());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    #[test]
    fn detect_system_proxy_returns_none_on_unsupported_platforms() {
        let result = super::detect_system_proxy();
        assert!(result.is_ok());
        assert!(result.unwrap_or(None).is_none());
    }

    // -----------------------------------------------------------------------
    // scutil --proxy 字典解析
    // -----------------------------------------------------------------------

    #[test]
    fn parse_scutil_dict_flattens_top_level_scalars() {
        let text = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n  }\n  HTTPEnable : 1\n  HTTPPort : 1087\n  HTTPProxy : 127.0.0.1\n}\n";
        let map = parse_scutil_dict(text);
        assert_eq!(map.get("HTTPEnable").map(String::as_str), Some("1"));
        assert_eq!(map.get("HTTPPort").map(String::as_str), Some("1087"));
        assert_eq!(map.get("HTTPProxy").map(String::as_str), Some("127.0.0.1"));
    }

    #[test]
    fn parse_scutil_proxy_output_all_disabled_returns_none() {
        let text = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 0\n  SOCKSEnable : 0\n}\n";
        assert!(parse_scutil_proxy_output(text).is_none());
    }

    #[test]
    fn parse_scutil_proxy_output_http_only() {
        let text =
            "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 1087\n  HTTPProxy : 127.0.0.1\n}\n";
        let cfg = parse_scutil_proxy_output(text).expect("http proxy should be detected");
        assert_eq!(cfg.proxy_type, ProxyType::Http);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 1087);
        assert_eq!(cfg.mode, ProxyMode::Manual);
    }

    /// `HTTPSProxy` 命名的是流量目标而非代理端点自身传输层——端点仍只说
    /// 明文 HTTP CONNECT，必须映射到 [`ProxyType::Http`]，与 Windows
    /// `ProxyServer` 的 `https=` 键同理（否则 reqwest 会对该端点误发 TLS
    /// ClientHello 导致连接失败）。
    #[test]
    fn parse_scutil_proxy_output_https_maps_to_http_transport() {
        let text =
            "<dictionary> {\n  HTTPSEnable : 1\n  HTTPSPort : 1087\n  HTTPSProxy : 127.0.0.1\n}\n";
        let cfg = parse_scutil_proxy_output(text).expect("https proxy should be detected");
        assert_eq!(cfg.proxy_type, ProxyType::Http);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 1087);
    }

    #[test]
    fn parse_scutil_proxy_output_prefers_socks_over_https_and_http() {
        let text = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 1087\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 1\n  HTTPSPort : 1087\n  HTTPSProxy : 127.0.0.1\n  SOCKSEnable : 1\n  SOCKSPort : 1086\n  SOCKSProxy : 127.0.0.1\n}\n";
        let cfg = parse_scutil_proxy_output(text).expect("socks proxy should win priority");
        assert_eq!(cfg.proxy_type, ProxyType::Socks5);
        assert_eq!(cfg.port, 1086);
    }

    #[test]
    fn parse_scutil_proxy_output_enabled_but_empty_host_returns_none() {
        // GUI 关掉某个协议但残留旧字段时常见：*Enable=1 但 host 字段是空串。
        let text = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 1087\n  HTTPProxy : \n}\n";
        assert!(parse_scutil_proxy_output(text).is_none());
    }

    // -----------------------------------------------------------------------
    // 代理环境变量解析（Linux GUI 进程常见，桌面终端会话导出）
    // -----------------------------------------------------------------------

    #[test]
    fn parse_env_proxy_url_plain_http() {
        let cfg = parse_env_proxy_url("http://127.0.0.1:7890").expect("should parse");
        assert_eq!(cfg.proxy_type, ProxyType::Http);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 7890);
        assert_eq!(cfg.mode, ProxyMode::Manual);
    }

    #[test]
    fn parse_env_proxy_url_socks5_with_credentials() {
        let cfg = parse_env_proxy_url("socks5://user:p%40ss@127.0.0.1:1080").expect("should parse");
        assert_eq!(cfg.proxy_type, ProxyType::Socks5);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 1080);
        assert_eq!(cfg.username, "user");
        assert_eq!(cfg.password, "p@ss");
    }

    #[test]
    fn parse_env_proxy_url_https_scheme_keeps_tls_transport() {
        // 少数工具（stunnel 包装的代理）会显式用 https:// 声明端点自身走
        // TLS；这与 Windows/scutil 目标语义的 `https=` 键不同，字面 scheme
        // 就是权威来源，必须原样映射到 Https 而不是降级为 Http。
        let cfg = parse_env_proxy_url("https://proxy.example.com").expect("should parse");
        assert_eq!(cfg.proxy_type, ProxyType::Https);
        assert_eq!(cfg.port, 443);
    }

    #[test]
    fn parse_env_proxy_url_invalid_returns_none() {
        assert!(parse_env_proxy_url("not a url").is_none());
    }

    // -----------------------------------------------------------------------
    // gsettings 取值解析
    // -----------------------------------------------------------------------

    #[test]
    fn strip_gsettings_quotes_removes_single_quotes() {
        assert_eq!(strip_gsettings_quotes("'127.0.0.1'"), "127.0.0.1");
    }

    #[test]
    fn strip_gsettings_quotes_leaves_bare_integer_unchanged() {
        assert_eq!(strip_gsettings_quotes("1087\n"), "1087");
    }

    #[test]
    fn strip_gsettings_quotes_empty_string_value_becomes_empty() {
        assert_eq!(strip_gsettings_quotes("''"), "");
    }

    // -----------------------------------------------------------------------
    // HTTP CONNECT response parsing (F006)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_connect_status_line_success() {
        let header = b"HTTP/1.1 200 Connection established\r\n\r\n";
        let (code, line) = parse_connect_status_line(header);
        assert_eq!(code, 200);
        assert_eq!(line.trim(), "HTTP/1.1 200 Connection established");
    }

    #[test]
    fn parse_connect_status_line_extracts_only_first_line() {
        // The buffer may include trailing headers; only the status line matters,
        // and the parser must not be confused by subsequent header lines.
        let header = b"HTTP/1.1 200 OK\r\nProxy-Agent: x\r\n\r\n";
        let (code, line) = parse_connect_status_line(header);
        assert_eq!(code, 200);
        assert_eq!(line.trim(), "HTTP/1.1 200 OK");
    }

    #[test]
    fn parse_connect_status_line_non_200() {
        let header = b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n";
        let (code, line) = parse_connect_status_line(header);
        assert_eq!(code, 407);
        assert_eq!(line.trim(), "HTTP/1.1 407 Proxy Authentication Required");
    }

    #[test]
    fn parse_connect_status_line_malformed_yields_zero() {
        let header = b"garbage-without-code\r\n\r\n";
        let (code, _line) = parse_connect_status_line(header);
        assert_eq!(code, 0);
    }
}
