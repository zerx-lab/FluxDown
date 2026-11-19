//! 本机服务之间经数据目录交换的文件契约：端点发布文件与 agent 启动失败记录。
//!
//! 这些文件只在同一台机器、同一用户的数据目录内读写（daemon → agent、agent → 桌面界面），
//! 不经网络传输、不含任何凭据；内容为 camelCase JSON，写入方一律原子替换。
//!
//! - 端点文件让监听方在默认端口不可用（被占用 / 落在系统保留区间）时回退到系统分配的
//!   端口，读取方每次连接前重读，不必猜端口。
//! - 启动失败记录让拉起方在子进程反复退出时给出具体原因，而不只是「连不上」。

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

/// agent UI Gateway 的本机 IPC 端点（Windows Named Pipe）名：
/// `\\.\pipe\fluxdown-gateway-<16 位十六进制>`，十六进制为规范化端点目录（`agent.token`
/// 所在目录）的 FNV-1a 64 散列。
///
/// 规范化：去掉 `\\?\` 前缀、`/` 换成 `\`、去掉结尾分隔符、转小写。监听方（agent）与拨号方
/// （桌面界面）各自由同一目录推导，不经文件交换；目录不同（另一用户、另一数据目录）即名字不同。
/// IPC 只是首选通道：推导不一致或端点不可用时，拨号方照常走 [`GATEWAY_ENDPOINT_FILE`] 发布的
/// 回环地址。
///
/// ```
/// use fluxdown_protocol::local_service::gateway_pipe_name;
///
/// assert_eq!(
///     gateway_pipe_name(r"\\?\C:\Users\Me\AppData\Roaming\FluxDown\agent\"),
///     gateway_pipe_name("c:/users/me/appdata/roaming/fluxdown/agent"),
/// );
/// ```
#[must_use]
pub fn gateway_pipe_name(endpoint_dir: &str) -> String {
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
    format!(r"\\.\pipe\fluxdown-gateway-{hash:016x}")
}

/// 已发布的本机 RPC 端点（无凭据）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEndpoint {
    /// `ws://<回环地址>:<端口>/rpc`。
    pub rpc_url: String,
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
    use super::{AgentStartupFailure, AgentStartupFailureKind, gateway_pipe_name};

    /// 两侧各自推导管道名：同一目录的不同写法必须得到同一名字，不同目录不得相同。
    #[test]
    fn pipe_name_is_stable_across_spellings_of_one_directory() {
        let canonical = gateway_pipe_name(r"C:\Users\Me\AppData\Roaming\FluxDown\agent");
        assert_eq!(canonical, r"\\.\pipe\fluxdown-gateway-90507e7cb547adb8");
        for spelling in [
            r"\\?\C:\Users\Me\AppData\Roaming\FluxDown\agent",
            r"c:\users\me\appdata\roaming\fluxdown\agent\",
            "C:/Users/Me/AppData/Roaming/FluxDown/agent/",
        ] {
            assert_eq!(gateway_pipe_name(spelling), canonical, "{spelling}");
        }
        assert_ne!(
            gateway_pipe_name(r"C:\Users\Other\AppData\Roaming\FluxDown\agent"),
            canonical
        );
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
