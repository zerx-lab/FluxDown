//! eD2K 端口映射 —— 争取 NAT 后 HighID。
//!
//! eD2K 服务器判定 HighID 的前提是「我方声明的监听端口可从公网回连」。NAT 后
//! 的客户端默认不可达（→ LowID）。本模块在网关上建立 `外部端口 → 本机监听端口`
//! 映射，使入站连接能穿透 NAT 到达我方监听器，从而登录时被服务器判为 HighID。
//!
//! 委托 rqbit fork 的 [`PortMapper`]（与 BT 同一实现）：UPnP IGD（`WANIPConnection`
//! v1 / v2 与 `WANPPPConnection` 逐个服务、按服务类型发 SOAP、725 降级永久租约、
//! 内网地址按应答网关所在子网选取）+ NAT-PMP / PCP，周期重新发现与续租，网络变化后
//! 自动重映射；停止时删除已建立的映射。
//!
//! 全程 best-effort：无网关 / 映射失败只体现在 [`MappingOutcome::Failed`]，
//! 下载回退到 LowID + 服务器 callback 路径（仍可用），不阻断主流程。

use std::net::IpAddr;
use std::time::Duration;

use librqbit_upnp::{
    PortMapper, PortMappingMethods, PortMappingOptions, PortMappingProtocol, PortMappingState,
    PortMappingStatus,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::logger::log_error;

/// 端口映射租约时长。有限租约防网关重启后残留死映射；映射器在到期前续租。
const LEASE: Duration = Duration::from_secs(3600);

/// 映射描述（网关管理页可见）。
const MAPPING_DESC: &str = "FluxDown eD2K";

/// 监听端口映射的折算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MappingOutcome {
    /// 仍在找网关或等网关应答。
    Pending,
    /// TCP 监听端口已以同号外部端口映射。
    Mapped {
        /// 网关报告的公网 IP；未知为 `None`。
        external_ip: Option<IpAddr>,
    },
    /// 所有方式都未映射成功；文本为各方式 / 网关的原因。映射器仍会周期重试。
    Failed(String),
}

/// 运行中的端口映射。drop 即停止：后台任务删除已建立的映射后退出。
pub struct UpnpMapping {
    mapper: PortMapper,
    tcp_port: u16,
    stop: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl UpnpMapping {
    /// 为 TCP 监听端口启动映射。`after` 为上一个已停止映射的后台任务：等它删完旧映射再
    /// 开始，防止旧映射器的删除覆盖新建的同号映射。
    ///
    /// Kad 只以 leech 身份用临时 UDP 套接字主动查询（应答经 NAT 原路返回），不接受入站
    /// 请求，所以不映射 UDP。
    ///
    /// 须在 tokio 运行时内调用。
    pub fn start(tcp_port: u16, after: Option<JoinHandle<()>>) -> Self {
        let mapper = PortMapper::new(
            [(PortMappingProtocol::Tcp, tcp_port)],
            PortMappingMethods {
                upnp: true,
                natpmp: true,
            },
            PortMappingOptions {
                description: MAPPING_DESC.to_owned(),
                lease_duration: LEASE,
                ..Default::default()
            },
            None,
        );
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let mapper = mapper.clone();
            let stop = stop.clone();
            async move {
                if let Some(previous) = after
                    && let Err(error) = previous.await
                {
                    log_error!(
                        "[ed2k-upnp] previous port mapping task ended abnormally: {}",
                        error
                    );
                }
                mapper.run(stop.cancelled_owned()).await;
            }
        });
        Self {
            mapper,
            tcp_port,
            stop,
            task: Some(task),
        }
    }

    /// 停止映射，返回后台任务句柄（等待它即等待旧映射删除完毕）。
    #[must_use]
    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.task.take()
    }

    /// TCP 监听端口的映射结果。
    #[must_use]
    pub fn outcome(&self) -> MappingOutcome {
        fold_outcome(&self.mapper.status(), self.tcp_port)
    }
}

impl Drop for UpnpMapping {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// 把映射器的状态行折算为 TCP 监听端口的结果。eD2K 登录只声明本机监听端口，服务器按
/// `公网 IP:该端口` 回连判定 HighID，所以外部端口与内部端口不同（NAT-PMP / PCP 网关
/// 另分配了端口）的映射不能算成功。
fn fold_outcome(rows: &[PortMappingStatus], tcp_port: u16) -> MappingOutcome {
    let Some(row) = rows
        .iter()
        .find(|row| row.protocol == PortMappingProtocol::Tcp && row.internal_port == tcp_port)
    else {
        return MappingOutcome::Pending;
    };
    match &row.state {
        PortMappingState::Pending => MappingOutcome::Pending,
        PortMappingState::Failed(reason) => MappingOutcome::Failed(reason.clone()),
        PortMappingState::Mapped if row.external_port == Some(tcp_port) => MappingOutcome::Mapped {
            external_ip: row.external_ip,
        },
        PortMappingState::Mapped => MappingOutcome::Failed(format!(
            "gateway mapped TCP {tcp_port} to external port {:?}; eD2K needs the same port",
            row.external_port
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp_row(state: PortMappingState, external_port: Option<u16>) -> PortMappingStatus {
        PortMappingStatus {
            protocol: PortMappingProtocol::Tcp,
            internal_port: 4662,
            external_port,
            method: None,
            external_ip: Some(IpAddr::from([203, 0, 113, 7])),
            state,
        }
    }

    #[test]
    fn mapping_to_another_external_port_is_not_reachable_for_ed2k() {
        let outcome = fold_outcome(&[tcp_row(PortMappingState::Mapped, Some(51234))], 4662);
        assert!(matches!(outcome, MappingOutcome::Failed(_)), "{outcome:?}");
    }

    #[test]
    fn same_port_mapping_is_mapped_and_udp_rows_are_ignored() {
        let udp = PortMappingStatus {
            protocol: PortMappingProtocol::Udp,
            internal_port: 4662,
            state: PortMappingState::Failed("UPnP: refused".to_owned()),
            ..tcp_row(PortMappingState::Pending, None)
        };
        let outcome = fold_outcome(&[udp, tcp_row(PortMappingState::Mapped, Some(4662))], 4662);
        assert_eq!(
            outcome,
            MappingOutcome::Mapped {
                external_ip: Some(IpAddr::from([203, 0, 113, 7]))
            }
        );
    }
}
