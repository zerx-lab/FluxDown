//! 本机服务之间经数据目录交换的文件契约：端点发布文件与 agent 启动失败记录。
//!
//! 这些文件只在同一台机器、同一用户的数据目录内读写（daemon → agent、agent → 桌面界面），
//! 不经网络传输、不含任何凭据；内容为 camelCase JSON，写入方一律原子替换。
//!
//! - 端点文件让监听方在默认端口不可用（被占用 / 落在系统保留区间）时回退到系统分配的
//!   端口，读取方每次连接前重读，不必猜端口。
//! - 启动失败记录让拉起方在子进程反复退出时给出具体原因，而不只是「连不上」。
//! - 数据目录本身不可访问（例如目录 ACL 只授权给重装系统前的旧账户 SID）时，Windows 上双方改用
//!   [`local_fallback_dir`] 这一确定性的用户临时目录交换启动失败记录与桌面日志。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// agent UI Gateway 端点：发布在 `agent.token` 同目录。
pub const GATEWAY_ENDPOINT_FILE: &str = "gateway-endpoint.json";

/// daemon 控制面端点：发布在 `daemon.token` 同目录。daemon 取得进程租约后先删除旧文件，
/// 控制面绑定成功后写入实际地址，退出时删除；旧版 daemon 从不发布。
pub const DAEMON_ENDPOINT_FILE: &str = "daemon-endpoint.json";

/// agent 启动失败记录：写在 `agent.token` 同目录；agent 开始提供服务后删除。
pub const AGENT_STARTUP_FAILURE_FILE: &str = "agent-startup-failure.json";

/// agent UI Gateway 的本机 IPC 端点（Unix domain socket）文件名：位于 `agent.token` 同目录。
/// 目录本身只有当前用户可进入；监听方另把 socket 设为 0600 并校验对端 uid。
pub const GATEWAY_IPC_SOCKET_FILE: &str = "gateway.sock";

/// agent UI Gateway 的本机 IPC 端点（Windows Named Pipe）名前缀；完整名 = 前缀 + 32 位小写
/// 十六进制随机串。
///
/// `\\.\pipe\` 是全机共享命名空间：可推导的名字能被其他账户抢先创建，冒充 agent 收下桌面界面
/// 发出的 bearer。agent 每次启动生成随机名并写入 [`GATEWAY_ENDPOINT_FILE`] 的
/// [`LocalEndpoint::pipe`]（用户私有目录）；拨号方只连文件里、且通过 [`is_gateway_pipe_name`]
/// 校验的名字，缺字段（旧 agent / IPC 建不起来）时只走回环 TCP。
pub const GATEWAY_PIPE_PREFIX: &str = r"\\.\pipe\fluxdown-gateway-";

/// 随机后缀长度（UUID simple 形态）。
const GATEWAY_PIPE_SUFFIX_LEN: usize = 32;

/// `name` 是否为 agent 按本契约生成的网关管道名（前缀 + 32 位小写十六进制）；远端管道、
/// 文件路径与旧版按目录散列的名字都不接受。
///
/// ```
/// use fluxdown_protocol::local_service::is_gateway_pipe_name;
///
/// assert!(is_gateway_pipe_name(r"\\.\pipe\fluxdown-gateway-0123456789abcdef0123456789abcdef"));
/// assert!(!is_gateway_pipe_name(r"\\.\pipe\fluxdown-gateway-90507e7cb547adb8"));
/// assert!(!is_gateway_pipe_name(r"\\host\pipe\fluxdown-gateway-0123456789abcdef0123456789abcdef"));
/// ```
#[must_use]
pub fn is_gateway_pipe_name(name: &str) -> bool {
    name.strip_prefix(GATEWAY_PIPE_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == GATEWAY_PIPE_SUFFIX_LEN
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// 端点目录的稳定键：规范化（去掉 `\\?\` 前缀、`/` 换成 `\`、去掉结尾分隔符、转小写）后的
/// FNV-1a 64 散列，16 位小写十六进制。同一目录的不同写法得到同一键。
///
/// ```
/// use fluxdown_protocol::local_service::endpoint_dir_key;
///
/// assert_eq!(
///     endpoint_dir_key(r"\\?\C:\Users\Me\AppData\Roaming\FluxDown\agent\"),
///     endpoint_dir_key("c:/users/me/appdata/roaming/fluxdown/agent"),
/// );
/// ```
#[must_use]
pub fn endpoint_dir_key(endpoint_dir: &str) -> String {
    let trimmed = match endpoint_dir.strip_prefix(r"\\?\") {
        Some(rest) => rest,
        None => endpoint_dir,
    };
    let normalized = trimmed.replace('/', "\\").to_lowercase();
    let normalized = normalized.trim_end_matches('\\');
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Windows：端点目录（`agent.token` 所在目录）不可访问时的回退目录
/// `%TEMP%\FluxDown\<endpoint_dir_key>`。拉起方（桌面）与 agent 各自由同一目录推导，用来交换
/// 启动失败记录、放置桌面日志与单实例锁；`%TEMP%` 位于用户私有的 `%LOCALAPPDATA%`。其他平台的
/// 临时目录是多用户共享的 `/tmp`，不提供回退，返回 `None`。
#[must_use]
pub fn local_fallback_dir(endpoint_dir: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        Some(
            std::env::temp_dir()
                .join("FluxDown")
                .join(endpoint_dir_key(&endpoint_dir.to_string_lossy())),
        )
    } else {
        None
    }
}

/// 已发布的本机 RPC 端点（无凭据）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEndpoint {
    /// `ws://<回环地址>:<端口>/rpc`。
    pub rpc_url: String,
    /// Windows 网关 IPC 管道名（见 [`GATEWAY_PIPE_PREFIX`]）；daemon 端点、Unix 与 IPC 不可用时缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe: Option<String>,
}

/// agent 启动失败所处的环节。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStartupFailureKind {
    /// UI Gateway 无法监听：固定地址被占用 / 系统保留，或回退端口同样不可用。
    GatewayBind,
    /// agent 数据目录不可读写（权限、只读卷、磁盘已满）。
    DataDir,
    /// 其它启动错误；也承接新版本写入、本版本尚不认识的环节。
    #[serde(other)]
    Other,
}

/// agent 启动失败记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStartupFailure {
    pub kind: AgentStartupFailureKind,
    /// 完整错误链，仅供诊断展示。
    pub detail: String,
    /// 写入时刻（Unix 毫秒）：读取方据此排除早于本轮拉起的旧记录。
    pub at_ms: u64,
    /// 写入方 agent 版本。
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::{
        AgentStartupFailure, AgentStartupFailureKind, LocalEndpoint, endpoint_dir_key,
        is_gateway_pipe_name,
    };

    /// 回退目录键：同一目录的不同写法必须得到同一键，不同目录不得相同。
    #[test]
    fn endpoint_dir_key_is_stable_across_spellings_of_one_directory() {
        let canonical = endpoint_dir_key(r"C:\Users\Me\AppData\Roaming\FluxDown\agent");
        assert_eq!(canonical, "90507e7cb547adb8");
        for spelling in [
            r"\\?\C:\Users\Me\AppData\Roaming\FluxDown\agent",
            r"c:\users\me\appdata\roaming\fluxdown\agent\",
            "C:/Users/Me/AppData/Roaming/FluxDown/agent/",
        ] {
            assert_eq!(endpoint_dir_key(spelling), canonical, "{spelling}");
        }
        assert_ne!(
            endpoint_dir_key(r"C:\Users\Other\AppData\Roaming\FluxDown\agent"),
            canonical
        );
    }

    #[test]
    fn gateway_pipe_names_must_be_random_local_pipes() {
        assert!(is_gateway_pipe_name(
            r"\\.\pipe\fluxdown-gateway-0123456789abcdef0123456789abcdef"
        ));
        for invalid in [
            r"\\.\pipe\fluxdown-gateway-0123456789ABCDEF0123456789abcdef",
            r"\\.\pipe\fluxdown-gateway-90507e7cb547adb8",
            r"\\server\pipe\fluxdown-gateway-0123456789abcdef0123456789abcdef",
            r"C:\Users\Me\gateway",
            "",
        ] {
            assert!(!is_gateway_pipe_name(invalid), "{invalid}");
        }
    }

    /// 旧版端点文件没有 `pipe`；daemon 端点永不写出该字段。
    #[test]
    fn endpoint_pipe_is_optional_on_the_wire() {
        let legacy: LocalEndpoint =
            serde_json::from_str(r#"{"rpcUrl":"ws://127.0.0.1:17800/rpc"}"#).expect("legacy");
        assert_eq!(legacy.pipe, None);
        let encoded = serde_json::to_string(&LocalEndpoint {
            rpc_url: "ws://127.0.0.1:17801/rpc".to_owned(),
            pipe: None,
        })
        .expect("encode");
        assert_eq!(encoded, r#"{"rpcUrl":"ws://127.0.0.1:17801/rpc"}"#);
    }

    /// 新版 agent 写入的未知环节在旧版界面里按「其它」展示，而不是整条记录解析失败。
    #[test]
    fn unknown_failure_kind_reads_as_other() {
        let record: AgentStartupFailure = serde_json::from_str(
            r#"{"kind":"someFutureStage","detail":"boom","atMs":7,"version":"9.9.9"}"#,
        )
        .expect("record with an unknown kind");
        assert_eq!(record.kind, AgentStartupFailureKind::Other);
        assert_eq!(record.detail, "boom");
        assert_eq!(record.at_ms, 7);
    }
}
