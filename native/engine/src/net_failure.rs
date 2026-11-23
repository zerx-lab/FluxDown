//! 网络传输失败的类型化分类：`ProxyMode::Auto` 换路、通用自动重试与
//! 多路径回收重派共用的唯一判据源。
//!
//! # 为什么不用关键词
//!
//! reqwest 0.12 的 `Display` 只输出「error sending request for url (…)」，
//! 根因全在 `source()` 链里；而链末端的 OS 错误描述是**本地化文本**——中文
//! Windows 上「连接被拒绝」是「由于目标计算机积极拒绝，无法连接。 (os error
//! 10061)」，英文关键词必然漏判。本模块只读与语言、平台无关的信号：
//!
//! - `std::io::ErrorKind`（Windows 的 WSAE* 由 std 映射到同一 kind）；
//! - reqwest 的 `is_timeout` / `is_connect` 标志；
//! - Windows SChannel 的 `SEC_E_*` / `CERT_E_*` HRESULT（TLS 失败，`raw_os_error`）；
//! - 依赖库自身写死的英文前缀（hyper-util `dns error`、rustls `invalid peer
//!   certificate` 等，非 OS 文本）。
//!
//! 判定走一遍 source 链（深度个位数）、零堆分配：库文本前缀写进栈上定长
//! 缓冲比较，满即截断。
//!
//! # 落库契约
//!
//! 任务错误文案落库后才由 manager 判定 failover / 自动重试，分类结果以
//! `[net:<code>]` 标签追加在文案末尾（[`append_tag`]），消费端只解析标签
//! （[`tag_of`]）。标签码是持久化契约，只增不改。

use std::error::Error as StdError;
use std::fmt::{self, Write as _};
use std::io;

/// 传输层失败类别。全部是「换一条链路可能修复」的失败；HTTP 状态、校验、
/// 磁盘 IO 等永远不归入此类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetFailure {
    /// 域名解析失败（含 DNS 污染导致的无结果）。
    Dns,
    /// 建连被拒（RST / ECONNREFUSED）。
    Refused,
    /// 建连超时（SYN 无响应、黑洞）。
    ConnectTimeout,
    /// 网络 / 主机不可达。
    Unreachable,
    /// 已建立的连接被重置 / 中止。
    Reset,
    /// TLS 握手失败（证书不符、致命告警——DNS 污染与中间人劫持的典型表现）。
    Tls,
    /// 请求 / 读超时（连接已建立）。
    Timeout,
    /// 响应体中途断流（EOF / 连接在报文完成前关闭）。
    Interrupted,
    /// 引擎自身的停滞判定（窗口内零字节）。
    Stalled,
    /// 建连失败但底层原因不可辨识（含代理 CONNECT 隧道被拒）。
    Connect,
}

impl NetFailure {
    /// 持久化标签码（`[net:<code>]`）。只增不改。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Refused => "refused",
            Self::ConnectTimeout => "connect-timeout",
            Self::Unreachable => "unreachable",
            Self::Reset => "reset",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Interrupted => "interrupted",
            Self::Stalled => "stalled",
            Self::Connect => "connect",
        }
    }

    /// [`Self::code`] 的逆映射；未知码 → `None`（前向兼容：新版本写入的
    /// 码被旧逻辑读到时按「无标签」回退）。
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "dns" => Self::Dns,
            "refused" => Self::Refused,
            "connect-timeout" => Self::ConnectTimeout,
            "unreachable" => Self::Unreachable,
            "reset" => Self::Reset,
            "tls" => Self::Tls,
            "timeout" => Self::Timeout,
            "interrupted" => Self::Interrupted,
            "stalled" => Self::Stalled,
            "connect" => Self::Connect,
            _ => return None,
        })
    }

    /// 同一路径原样重试是否可能成功。TLS 失败（证书不符 / 被劫持）在同一
    /// 路径上是确定性的，只有换路径才有意义。
    #[must_use]
    pub const fn retriable_same_path(self) -> bool {
        !matches!(self, Self::Tls)
    }
}

impl fmt::Display for NetFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

const TAG_OPEN: &str = "[net:";

/// 解析文案末尾（或任意位置）首个 `[net:<code>]` 标签。
#[must_use]
pub fn tag_of(msg: &str) -> Option<NetFailure> {
    let start = msg.find(TAG_OPEN)? + TAG_OPEN.len();
    let rest = msg.get(start..)?;
    let end = rest.find(']')?;
    NetFailure::from_code(rest.get(..end)?)
}

/// 给文案追加分类标签；已带标签则原样返回（幂等）。
#[must_use]
pub fn append_tag(msg: String, failure: Option<NetFailure>) -> String {
    match failure {
        Some(failure) if tag_of(&msg).is_none() => {
            let mut msg = msg;
            msg.push_str(" [net:");
            msg.push_str(failure.code());
            msg.push(']');
            msg
        }
        _ => msg,
    }
}

/// 沿 source 链分类一个错误。链上越深的具体信号优先（`io::Error` 的 kind
/// 比外层 reqwest 标志更精确）；全链都无具体信号时回退 reqwest 的
/// `is_timeout` / `is_connect`。非传输失败 → `None`。
#[must_use]
pub fn classify(err: &(dyn StdError + 'static)) -> Option<NetFailure> {
    let mut specific = None;
    let mut connect = false;
    let mut timeout = false;
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(r) = e.downcast_ref::<reqwest::Error>() {
            connect |= r.is_connect();
            timeout |= r.is_timeout();
        }
        let found = match e.downcast_ref::<io::Error>() {
            Some(io) => from_io(io),
            None => from_library_text(e),
        };
        if found.is_some() {
            specific = found;
        }
        cur = e.source();
    }
    match specific {
        Some(NetFailure::Timeout) if connect => Some(NetFailure::ConnectTimeout),
        Some(found) => Some(found),
        None if timeout && connect => Some(NetFailure::ConnectTimeout),
        None if timeout => Some(NetFailure::Timeout),
        None if connect => Some(NetFailure::Connect),
        None => None,
    }
}

/// 下载错误的分类。`Io` 是本地磁盘 / 文件错误，永不视为传输失败；字符串型
/// 错误只认已有标签与引擎自身的停滞文案（引擎写死的英文，非 OS 文本）。
#[must_use]
pub fn classify_download_error(e: &crate::downloader::DownloadError) -> Option<NetFailure> {
    use crate::downloader::DownloadError;
    match e {
        DownloadError::Request(r) => classify(r),
        DownloadError::Other(msg) | DownloadError::CdnNodeFailed(msg) => {
            tag_of(msg).or_else(|| is_engine_stall(msg).then_some(NetFailure::Stalled))
        }
        DownloadError::RangeRestartFailed(inner) => classify_download_error(inner),
        _ => None,
    }
}

/// 引擎自身停滞判定的固定文案（`download stalled: …` / `segment N stalled: …`
/// / `HLS segment N stalled: …` 等）。
fn is_engine_stall(msg: &str) -> bool {
    msg.contains(" stalled: ")
}

fn from_io(io: &io::Error) -> Option<NetFailure> {
    use io::ErrorKind as K;
    let kind = match io.kind() {
        K::ConnectionRefused => NetFailure::Refused,
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::NotConnected => {
            NetFailure::Reset
        }
        K::TimedOut => NetFailure::Timeout,
        K::NetworkUnreachable | K::HostUnreachable | K::NetworkDown | K::AddrNotAvailable => {
            NetFailure::Unreachable
        }
        K::UnexpectedEof => NetFailure::Interrupted,
        _ => {
            if is_schannel_tls_error(io) {
                return Some(NetFailure::Tls);
            }
            // io::Error 常包裹库错误（tokio-rustls 以 InvalidData 包 rustls::Error）。
            return io.get_ref().and_then(from_library_text);
        }
    };
    Some(kind)
}

/// Windows SChannel 的 TLS 失败以 HRESULT 形式出现在 `raw_os_error`：
/// `SEC_E_*`（0x8009_03xx）与证书链 `CERT_E_*`（0x800B_01xx）。
#[cfg(windows)]
fn is_schannel_tls_error(io: &io::Error) -> bool {
    io.raw_os_error().is_some_and(|code| {
        let code = u32::from_ne_bytes(code.to_ne_bytes());
        (0x8009_0300..=0x8009_03FF).contains(&code) || (0x800B_0100..=0x800B_01FF).contains(&code)
    })
}

#[cfg(not(windows))]
fn is_schannel_tls_error(_io: &io::Error) -> bool {
    false
}

/// 依赖库写死的英文前缀（非 OS 本地化文本），按小写比较。
const LIBRARY_PREFIXES: &[(&str, NetFailure)] = &[
    // hyper-util HttpConnector：DNS 阶段失败的包装文案。
    ("dns error", NetFailure::Dns),
    // std `lookup_host` / getaddrinfo 失败的固定前缀。
    ("failed to lookup address", NetFailure::Dns),
    // rustls::Error 的 Display 前缀。
    ("invalid peer certificate", NetFailure::Tls),
    ("received fatal alert", NetFailure::Tls),
    ("peer is incompatible", NetFailure::Tls),
    ("peer misbehaved", NetFailure::Tls),
    ("invalid certificate", NetFailure::Tls),
    // hyper：报文完成前连接关闭（body 中途断流）。
    (
        "connection closed before message completed",
        NetFailure::Interrupted,
    ),
    // reqwest：代理 CONNECT 隧道被拒 / SOCKS 握手失败。
    ("unsuccessful tunnel", NetFailure::Connect),
    ("socks connect error", NetFailure::Connect),
];

/// 最长前缀的字节数（栈缓冲容量）。
const PREFIX_CAP: usize = 48;

fn from_library_text<E: fmt::Display + ?Sized>(e: &E) -> Option<NetFailure> {
    let mut buf = PrefixBuf::default();
    write!(buf, "{e}").ok()?;
    let head = buf.as_lower();
    LIBRARY_PREFIXES
        .iter()
        .find(|(prefix, _)| head.starts_with(prefix.as_bytes()))
        .map(|(_, failure)| *failure)
}

/// 定长栈缓冲：只保留 Display 输出的前 [`PREFIX_CAP`] 字节（ASCII 小写化），
/// 超出部分丢弃——分类只需要前缀。
struct PrefixBuf {
    bytes: [u8; PREFIX_CAP],
    len: usize,
}

impl Default for PrefixBuf {
    fn default() -> Self {
        Self {
            bytes: [0; PREFIX_CAP],
            len: 0,
        }
    }
}

impl PrefixBuf {
    fn as_lower(&self) -> &[u8] {
        match self.bytes.get(..self.len) {
            Some(head) => head,
            None => &[],
        }
    }
}

impl fmt::Write for PrefixBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            let Some(slot) = self.bytes.get_mut(self.len) else {
                break;
            };
            *slot = b.to_ascii_lowercase();
            self.len += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::DownloadError;

    /// 绑定后立即释放的回环端口：连接必被拒绝（确定性，不依赖外网）。
    fn closed_port() -> std::io::Result<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(listener.local_addr()?.port())
    }

    #[tokio::test]
    async fn real_reqwest_connect_refusal_is_classified_without_keywords()
    -> Result<(), Box<dyn std::error::Error>> {
        let port = closed_port()?;
        let client = reqwest::Client::builder().no_proxy().build()?;
        let Err(err) = client
            .get(format!("http://127.0.0.1:{port}/f.bin"))
            .send()
            .await
        else {
            return Err("closed port unexpectedly answered".into());
        };
        // reqwest 0.12 的 Display 不含根因：关键词匹配在这里注定失败。
        assert!(!err.to_string().to_lowercase().contains("refused"), "{err}");
        assert_eq!(classify(&err), Some(NetFailure::Refused));
        assert_eq!(
            classify_download_error(&DownloadError::Request(err)),
            Some(NetFailure::Refused)
        );
        Ok(())
    }

    #[test]
    fn io_kinds_map_independent_of_os_message_language() {
        // 中文 Windows 的本地化描述不含任何英文关键词，kind 才是判据。
        let localized = |kind| std::io::Error::new(kind, "由于目标计算机积极拒绝，无法连接。");
        assert_eq!(
            classify(&localized(std::io::ErrorKind::ConnectionRefused)),
            Some(NetFailure::Refused)
        );
        assert_eq!(
            classify(&localized(std::io::ErrorKind::ConnectionReset)),
            Some(NetFailure::Reset)
        );
        assert_eq!(
            classify(&localized(std::io::ErrorKind::HostUnreachable)),
            Some(NetFailure::Unreachable)
        );
        assert_eq!(
            classify(&localized(std::io::ErrorKind::UnexpectedEof)),
            Some(NetFailure::Interrupted)
        );
        // 非网络 kind 不归类（磁盘满、权限等）。
        assert_eq!(
            classify(&localized(std::io::ErrorKind::PermissionDenied)),
            None
        );
    }

    #[test]
    fn wrapped_library_errors_are_recognised_by_fixed_prefix() {
        let tls = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid peer certificate: NotValidForName",
        );
        assert_eq!(classify(&tls), Some(NetFailure::Tls));
        let dns = std::io::Error::other(
            "failed to lookup address information: nodename nor servname provided",
        );
        assert_eq!(classify(&dns), Some(NetFailure::Dns));
    }

    #[test]
    fn download_errors_never_classify_local_io_or_http_status() {
        let disk = DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "misleading kind on a file error",
        ));
        assert_eq!(classify_download_error(&disk), None);
        assert_eq!(
            classify_download_error(&DownloadError::Other("HTTP 404 Not Found".into())),
            None
        );
        assert_eq!(
            classify_download_error(&DownloadError::Other(
                "segment 3 stalled: no data received for 10s".into()
            )),
            Some(NetFailure::Stalled)
        );
    }

    #[test]
    fn tag_round_trips_and_is_idempotent() {
        for failure in [
            NetFailure::Dns,
            NetFailure::Refused,
            NetFailure::ConnectTimeout,
            NetFailure::Unreachable,
            NetFailure::Reset,
            NetFailure::Tls,
            NetFailure::Timeout,
            NetFailure::Interrupted,
            NetFailure::Stalled,
            NetFailure::Connect,
        ] {
            let msg = append_tag(
                "probes failed: plain GET=network-error".into(),
                Some(failure),
            );
            assert_eq!(tag_of(&msg), Some(failure), "{msg}");
            assert_eq!(append_tag(msg.clone(), Some(NetFailure::Dns)), msg);
        }
        assert_eq!(tag_of("[net:quantum-tunnel]"), None);
        assert_eq!(tag_of("[net:refused"), None);
    }
}
