//! 网络诊断：引擎模型 ↔ protocol DTO。

use fluxdown_engine::net_iface::{AddressScope, InterfaceKind, NetInterfaces};
use fluxdown_engine::net_probe::{MeasureReport, MeasureRequest, RouteReport};
use fluxdown_protocol::net_diag::{
    NET_MEASURE_DEFAULT_MEGABYTES, NET_MEASURE_DEFAULT_SECONDS, NET_MEASURE_MAX_MEGABYTES,
    NET_MEASURE_MAX_SECONDS, NET_MEASURE_MIN_SECONDS, NetAddressScope, NetDnsServerDto,
    NetInterfaceDto, NetInterfaceKind, NetInterfacesDto, NetMeasureParams, NetMeasureResultDto,
    NetMeasureSourceDto, NetRouteMeasureDto,
};
use std::time::Duration;

fn scope_to_dto(scope: AddressScope) -> NetAddressScope {
    match scope {
        AddressScope::Loopback => NetAddressScope::Loopback,
        AddressScope::LinkLocal => NetAddressScope::LinkLocal,
        AddressScope::Private => NetAddressScope::Private,
        AddressScope::SharedCgnat => NetAddressScope::SharedCgnat,
        AddressScope::Public => NetAddressScope::Public,
        AddressScope::Other => NetAddressScope::Other,
    }
}

fn kind_to_dto(kind: InterfaceKind) -> NetInterfaceKind {
    match kind {
        InterfaceKind::Ethernet => NetInterfaceKind::Ethernet,
        InterfaceKind::Wifi => NetInterfaceKind::Wifi,
        InterfaceKind::Cellular => NetInterfaceKind::Cellular,
        InterfaceKind::Vpn => NetInterfaceKind::Vpn,
        InterfaceKind::Loopback => NetInterfaceKind::Loopback,
        InterfaceKind::Other => NetInterfaceKind::Other,
    }
}

/// 网卡采集结果 → wire。
#[must_use]
pub fn net_interfaces_to_dto(facts: NetInterfaces) -> NetInterfacesDto {
    NetInterfacesDto {
        interfaces: facts
            .interfaces
            .into_iter()
            .map(|nic| NetInterfaceDto {
                name: nic.name,
                kind: kind_to_dto(nic.kind),
                up: nic.up,
                link_speed_mbps: nic.link_speed_mbps,
                mtu: nic.mtu,
                ipv4: nic.ipv4.into_iter().map(scope_to_dto).collect(),
                ipv6: nic.ipv6.into_iter().map(scope_to_dto).collect(),
                default_route_v4: nic.default_route_v4,
                default_route_v6: nic.default_route_v6,
            })
            .collect(),
        dns_servers: facts
            .dns_servers
            .into_iter()
            .map(|server| NetDnsServerDto {
                scope: scope_to_dto(server.scope),
                address: server.address,
            })
            .collect(),
        notes: facts.notes,
    }
}

/// wire 参数 → 测量请求：读取量与时长按协议上下限收敛，缺省取协议默认值。
#[must_use]
pub fn net_measure_request(params: NetMeasureParams) -> MeasureRequest {
    let megabytes = params
        .max_megabytes
        .unwrap_or(NET_MEASURE_DEFAULT_MEGABYTES)
        .clamp(1, NET_MEASURE_MAX_MEGABYTES);
    let seconds = params
        .max_seconds
        .unwrap_or(NET_MEASURE_DEFAULT_SECONDS)
        .clamp(NET_MEASURE_MIN_SECONDS, NET_MEASURE_MAX_SECONDS);
    MeasureRequest {
        task_id: params.task_id.filter(|id| !id.trim().is_empty()),
        url: params.url.filter(|url| !url.trim().is_empty()),
        routes: params.routes,
        connections: params.connections,
        max_bytes: u64::from(megabytes) * 1024 * 1024,
        max_duration: Duration::from_secs(u64::from(seconds)),
    }
}

fn route_to_dto(route: RouteReport) -> NetRouteMeasureDto {
    NetRouteMeasureDto {
        route: route.route,
        egress: route.egress,
        route_label: route.route_label,
        ok: route.ok,
        error: route.error,
        http_status: route.http_status,
        timing_target: route.timing_target,
        dns_ms: route.dns_ms,
        connect_ms: route.connect_ms,
        tls_ms: route.tls_ms,
        first_byte_ms: route.first_byte_ms,
        range_supported: route.range_supported,
        total_bytes: route.total_bytes,
        bytes: route.bytes,
        seconds: route.seconds,
        avg_bytes_per_sec: route.avg_bytes_per_sec,
        peak_bytes_per_sec: route.peak_bytes_per_sec,
        connections_used: route.connections_used,
        per_connection_bytes_per_sec: route.per_connection_bytes_per_sec,
        stopped_by: route.stopped_by,
    }
}

/// 测量结果 → wire。
#[must_use]
pub fn net_measure_to_dto(report: MeasureReport) -> NetMeasureResultDto {
    NetMeasureResultDto {
        source: NetMeasureSourceDto {
            scheme: report.scheme,
            host: report.host,
            task_id: report.task_id,
            connections: report.connections,
            max_bytes: report.max_bytes,
            max_seconds: report.max_seconds,
        },
        routes: report.routes.into_iter().map(route_to_dto).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_limits_clamp_to_protocol_bounds() {
        let request = net_measure_request(NetMeasureParams {
            task_id: Some("  ".to_owned()),
            url: Some("https://example.com/a.bin".to_owned()),
            max_megabytes: Some(100_000),
            max_seconds: Some(1),
            ..NetMeasureParams::default()
        });
        assert_eq!(request.task_id, None);
        assert_eq!(
            request.max_bytes,
            u64::from(NET_MEASURE_MAX_MEGABYTES) * 1024 * 1024
        );
        assert_eq!(
            request.max_duration,
            Duration::from_secs(u64::from(NET_MEASURE_MIN_SECONDS))
        );

        let defaults = net_measure_request(NetMeasureParams::default());
        assert_eq!(
            defaults.max_bytes,
            u64::from(NET_MEASURE_DEFAULT_MEGABYTES) * 1024 * 1024
        );
        assert_eq!(
            defaults.max_duration,
            Duration::from_secs(u64::from(NET_MEASURE_DEFAULT_SECONDS))
        );
    }
}
