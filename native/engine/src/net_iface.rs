//! 本机网卡事实（`daemon.net.interfaces`）：类型、启用状态、协商链路速率（Wi-Fi 为当前 PHY 速率）、
//! MTU、地址范围、默认路由与 DNS 服务器。
//!
//! 地址只给范围分类，不输出地址本身（DNS 服务器的公网地址除外：公共解析服务可辨识、不指向个人）。
//! 链路速率、类型与默认路由没有跨平台的安全 API：Linux 读 `/sys` 与 `/proc`；macOS 调
//! `ifconfig -v` 与 `route -n get`；Windows 调一次 PowerShell 的 `Get-NetAdapter` / `Get-NetRoute`
//! / `Get-DnsClientServerAddress`。都是固定命令、不拼接外部输入；拿不到的事实记进 `notes`。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// 地址范围分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressScope {
    Loopback,
    LinkLocal,
    Private,
    SharedCgnat,
    Public,
    Other,
}

/// 网卡类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceKind {
    Ethernet,
    Wifi,
    Cellular,
    Vpn,
    Loopback,
    Other,
}

/// 一块网卡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetInterface {
    pub name: String,
    pub kind: InterfaceKind,
    pub up: bool,
    pub link_speed_mbps: Option<u64>,
    pub mtu: Option<u32>,
    pub ipv4: Vec<AddressScope>,
    pub ipv6: Vec<AddressScope>,
    pub default_route_v4: bool,
    pub default_route_v6: bool,
}

/// 一个 DNS 服务器：公网地址原样给出，其余只给范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsServer {
    pub scope: AddressScope,
    pub address: Option<String>,
}

/// 采集结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetInterfaces {
    pub interfaces: Vec<NetInterface>,
    pub dns_servers: Vec<DnsServer>,
    pub notes: Vec<String>,
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// IPv4 地址范围。
#[must_use]
pub fn scope_v4(ip: Ipv4Addr) -> AddressScope {
    let [a, b, ..] = ip.octets();
    if ip.is_loopback() {
        AddressScope::Loopback
    } else if ip.is_link_local() {
        AddressScope::LinkLocal
    } else if ip.is_private() {
        AddressScope::Private
    } else if a == 100 && (64..=127).contains(&b) {
        AddressScope::SharedCgnat
    } else if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || ip.is_documentation()
    {
        AddressScope::Other
    } else {
        AddressScope::Public
    }
}

/// IPv6 地址范围（ULA 记为私有）。
#[must_use]
pub fn scope_v6(ip: Ipv6Addr) -> AddressScope {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return scope_v4(v4);
    }
    let first = ip.segments()[0];
    if ip.is_loopback() {
        AddressScope::Loopback
    } else if first & 0xffc0 == 0xfe80 {
        AddressScope::LinkLocal
    } else if first & 0xfe00 == 0xfc00 {
        AddressScope::Private
    } else if ip.is_unspecified()
        || ip.is_multicast()
        || first == 0x2001 && ip.segments()[1] == 0x0db8
    {
        AddressScope::Other
    } else if first & 0xe000 == 0x2000 {
        AddressScope::Public
    } else {
        AddressScope::Other
    }
}

fn scope_of(ip: IpAddr) -> AddressScope {
    match ip {
        IpAddr::V4(ip) => scope_v4(ip),
        IpAddr::V6(ip) => scope_v6(ip),
    }
}

/// 名称推断的类型（系统没有给出明确类型时使用）。
fn kind_from_name(name: &str) -> InterfaceKind {
    let lower = name.to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| lower.starts_with(prefix));
    if starts(&["lo"]) {
        InterfaceKind::Loopback
    } else if starts(&[
        "utun",
        "tun",
        "tap",
        "wg",
        "ppp",
        "ipsec",
        "tailscale",
        "zt",
    ]) {
        InterfaceKind::Vpn
    } else if starts(&["wl", "wifi", "wi-fi", "wlan"]) {
        InterfaceKind::Wifi
    } else if starts(&["wwan", "rmnet", "pdp_ip", "ccmni"]) {
        InterfaceKind::Cellular
    } else if starts(&["eth", "en", "em", "eno", "ens", "enp", "ethernet"]) {
        InterfaceKind::Ethernet
    } else {
        InterfaceKind::Other
    }
}

fn dns_server(ip: IpAddr) -> DnsServer {
    let scope = scope_of(ip);
    DnsServer {
        scope,
        address: (scope == AddressScope::Public).then(|| ip.to_string()),
    }
}

/// 解析 `resolv.conf` 的 `nameserver` 行（去重、保持顺序）。
fn parse_resolv_conf(text: &str) -> Vec<IpAddr> {
    let mut servers: Vec<IpAddr> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("nameserver") {
            continue;
        }
        let Some(raw) = parts.next() else { continue };
        // IPv6 链路本地地址可能带 `%iface`。
        let raw = raw.split('%').next().unwrap_or(raw);
        if let Ok(ip) = raw.parse::<IpAddr>()
            && !servers.contains(&ip)
        {
            servers.push(ip);
        }
    }
    servers
}

/// 采集本机网卡事实。阻塞部分在阻塞线程池执行。
pub async fn collect() -> NetInterfaces {
    let mut result = NetInterfaces::default();
    let nics = match tokio::task::spawn_blocking(crate::multi_nic::enumerate_nics).await {
        Ok(Ok(nics)) => nics,
        Ok(Err(error)) => {
            result
                .notes
                .push(format!("interface enumeration failed: {error}"));
            Vec::new()
        }
        Err(error) => {
            result
                .notes
                .push(format!("interface enumeration task failed: {error}"));
            Vec::new()
        }
    };
    result.interfaces = nics
        .iter()
        .map(|nic| NetInterface {
            kind: kind_from_name(&nic.name),
            name: nic.name.clone(),
            up: nic.up,
            link_speed_mbps: None,
            mtu: None,
            ipv4: nic.v4.iter().map(|(ip, _)| scope_v4(*ip)).collect(),
            ipv6: nic.v6.iter().map(|(ip, _)| scope_v6(*ip)).collect(),
            default_route_v4: false,
            default_route_v6: false,
        })
        .collect();
    platform::enrich(&mut result).await;
    result
}

/// 运行固定系统命令，返回 stdout（失败记进 `notes`）。
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
async fn command_output(program: &str, args: &[&str], notes: &mut Vec<String>) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::proc::no_console_window(&mut command);
    match tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Ok(output)) => {
            notes.push(format!("`{program}` exited with {}", output.status));
            None
        }
        Ok(Err(error)) => {
            notes.push(format!("`{program}` unavailable: {error}"));
            None
        }
        Err(_) => {
            notes.push(format!("`{program}` timed out"));
            None
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{InterfaceKind, NetInterfaces, command_output, dns_server, parse_resolv_conf};

    fn read_trimmed(path: &str) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim().to_owned())
    }

    /// `/proc/net/route` 中目的为 0.0.0.0 的网卡名。
    fn default_v4(text: &str) -> Vec<String> {
        text.lines()
            .skip(1)
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                (fields.next()? == "00000000").then(|| name.to_owned())
            })
            .collect()
    }

    /// `/proc/net/ipv6_route` 中目的为 ::/0 的网卡名（最后一列）。
    fn default_v6(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let destination = fields.first()?;
                let prefix = fields.get(1)?;
                let name = fields.last()?;
                (destination.bytes().all(|b| b == b'0') && *prefix == "00" && *name != "lo")
                    .then(|| (*name).to_owned())
            })
            .collect()
    }

    /// `iw dev <name> link` 的 `tx bitrate: 866.7 MBit/s`。
    fn wifi_bitrate(text: &str) -> Option<u64> {
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with("tx bitrate:"))?;
        let value = line.split(':').nth(1)?.split_whitespace().next()?;
        value.parse::<f64>().ok().map(|mbps| mbps.round() as u64)
    }

    pub(super) async fn enrich(result: &mut NetInterfaces) {
        let route_v4 = read_trimmed("/proc/net/route")
            .map(|text| default_v4(&text))
            .unwrap_or_default();
        let route_v6 = read_trimmed("/proc/net/ipv6_route")
            .map(|text| default_v6(&text))
            .unwrap_or_default();
        let mut notes = Vec::new();
        for nic in &mut result.interfaces {
            let base = format!("/sys/class/net/{}", nic.name);
            nic.mtu = read_trimmed(&format!("{base}/mtu")).and_then(|v| v.parse().ok());
            nic.link_speed_mbps = read_trimmed(&format!("{base}/speed"))
                .and_then(|v| v.parse::<i64>().ok())
                .and_then(|speed| u64::try_from(speed).ok())
                .filter(|speed| *speed > 0);
            if std::path::Path::new(&format!("{base}/wireless")).exists()
                || std::path::Path::new(&format!("{base}/phy80211")).exists()
            {
                nic.kind = InterfaceKind::Wifi;
            } else if read_trimmed(&format!("{base}/type")).as_deref() == Some("772") {
                nic.kind = InterfaceKind::Loopback;
            } else if nic.kind == InterfaceKind::Other
                && std::path::Path::new(&format!("{base}/device")).exists()
            {
                nic.kind = InterfaceKind::Ethernet;
            }
            if nic.kind == InterfaceKind::Wifi
                && nic.up
                && let Some(text) =
                    command_output("iw", &["dev", &nic.name, "link"], &mut notes).await
            {
                nic.link_speed_mbps = wifi_bitrate(&text).or(nic.link_speed_mbps);
            }
            nic.default_route_v4 = route_v4.contains(&nic.name);
            nic.default_route_v6 = route_v6.contains(&nic.name);
        }
        result.notes.extend(notes);
        let mut servers = read_trimmed("/etc/resolv.conf")
            .map(|text| parse_resolv_conf(&text))
            .unwrap_or_default();
        // systemd-resolved 的本地桩：真正的上游在 /run/systemd/resolve/resolv.conf。
        if servers.iter().all(std::net::IpAddr::is_loopback)
            && let Some(text) = read_trimmed("/run/systemd/resolve/resolv.conf")
        {
            let upstream = parse_resolv_conf(&text);
            if !upstream.is_empty() {
                servers = upstream;
            }
        }
        result.dns_servers = servers.into_iter().map(dns_server).collect();
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_proc_routes_and_iw_bitrate() {
            let v4 = "Iface\tDestination\tGateway\nwlp2s0\t00000000\t0101A8C0\nwlp2s0\t0001A8C0\t00000000\n";
            assert_eq!(default_v4(v4), vec!["wlp2s0".to_owned()]);
            let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n";
            assert_eq!(default_v6(v6), vec!["eth0".to_owned()]);
            assert_eq!(
                wifi_bitrate("Connected to aa:bb\n\ttx bitrate: 866.7 MBit/s VHT-MCS 9\n"),
                Some(867)
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{InterfaceKind, NetInterfaces, command_output, dns_server, parse_resolv_conf};

    /// `ifconfig -a -v` 的一个网卡块。
    #[derive(Debug, Default)]
    struct IfconfigBlock {
        name: String,
        mtu: Option<u32>,
        kind: Option<String>,
        link_mbps: Option<u64>,
    }

    fn parse_ifconfig(text: &str) -> Vec<IfconfigBlock> {
        let mut blocks: Vec<IfconfigBlock> = Vec::new();
        for line in text.lines() {
            if !line.starts_with(char::is_whitespace) && !line.is_empty() {
                let Some((name, rest)) = line.split_once(':') else {
                    continue;
                };
                let mtu = rest
                    .split_whitespace()
                    .skip_while(|word| *word != "mtu")
                    .nth(1)
                    .and_then(|value| value.parse().ok());
                blocks.push(IfconfigBlock {
                    name: name.to_owned(),
                    mtu,
                    ..IfconfigBlock::default()
                });
                continue;
            }
            let Some(current) = blocks.last_mut() else {
                continue;
            };
            let trimmed = line.trim();
            if let Some(kind) = trimmed.strip_prefix("type:") {
                current.kind = Some(kind.trim().to_owned());
            } else if let Some(rate) = trimmed.strip_prefix("link rate:") {
                current.link_mbps = parse_rate(rate);
            }
        }
        blocks
    }

    /// `1.00 Gbps` / `866.70 Mbps` / `1.20 Gbps / 866.70 Mbps`（上行 / 下行取首个）。
    fn parse_rate(text: &str) -> Option<u64> {
        let mut parts = text.split_whitespace();
        let value = parts.next()?.parse::<f64>().ok()?;
        let unit = parts.next()?;
        let mbps = match unit.to_ascii_lowercase().as_str() {
            "gbps" => value * 1000.0,
            "mbps" => value,
            "kbps" => value / 1000.0,
            "bps" => value / 1_000_000.0,
            _ => return None,
        };
        (mbps >= 1.0).then(|| mbps.round() as u64)
    }

    fn route_interface(text: &str) -> Option<String> {
        text.lines().find_map(|line| {
            line.trim()
                .strip_prefix("interface:")
                .map(|name| name.trim().to_owned())
        })
    }

    pub(super) async fn enrich(result: &mut NetInterfaces) {
        let mut notes = Vec::new();
        let blocks = command_output("/sbin/ifconfig", &["-a", "-v"], &mut notes)
            .await
            .map(|text| parse_ifconfig(&text))
            .unwrap_or_default();
        let route_v4 = command_output("/sbin/route", &["-n", "get", "default"], &mut notes)
            .await
            .and_then(|text| route_interface(&text));
        // 没有 IPv6 默认路由是常态（`route` 以非零退出），不记进 notes。
        let route_v6 = command_output(
            "/sbin/route",
            &["-n", "get", "-inet6", "default"],
            &mut Vec::new(),
        )
        .await
        .and_then(|text| route_interface(&text));
        for nic in &mut result.interfaces {
            if let Some(block) = blocks.iter().find(|block| block.name == nic.name) {
                nic.mtu = block.mtu;
                nic.link_speed_mbps = block.link_mbps;
                match block.kind.as_deref() {
                    Some("Wi-Fi") => nic.kind = InterfaceKind::Wifi,
                    Some("Ethernet") => nic.kind = InterfaceKind::Ethernet,
                    Some("Loopback") => nic.kind = InterfaceKind::Loopback,
                    Some("Cellular") => nic.kind = InterfaceKind::Cellular,
                    _ => {}
                }
            }
            nic.default_route_v4 = route_v4.as_deref() == Some(nic.name.as_str());
            nic.default_route_v6 = route_v6.as_deref() == Some(nic.name.as_str());
        }
        result.notes.extend(notes);
        let servers = std::fs::read_to_string("/etc/resolv.conf")
            .map(|text| parse_resolv_conf(&text))
            .unwrap_or_default();
        result.dns_servers = servers.into_iter().map(dns_server).collect();
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_ifconfig_verbose_and_route() {
            let text = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\ttype: Loopback\nen0: flags=8863<UP,BROADCAST> mtu 1500\n\tether aa:bb\n\ttype: Wi-Fi\n\tlink rate: 1.20 Gbps / 866.70 Mbps\n";
            let blocks = parse_ifconfig(text);
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[1].name, "en0");
            assert_eq!(blocks[1].mtu, Some(1500));
            assert_eq!(blocks[1].kind.as_deref(), Some("Wi-Fi"));
            assert_eq!(blocks[1].link_mbps, Some(1200));
            assert_eq!(
                route_interface("   route to: default\n  interface: en0\n"),
                Some("en0".to_owned())
            );
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::{InterfaceKind, NetInterfaces, command_output, dns_server};
    use serde::Deserialize;
    use std::net::IpAddr;

    /// 一次 PowerShell 调用收齐网卡、默认路由与 DNS（`ConvertTo-Json -Compress`）。
    const SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue';\
        $a=@(Get-NetAdapter | Select-Object Name,Status,ReceiveLinkSpeed,MtuSize,MediaType,PhysicalMediaType,InterfaceDescription);\
        $r=@(Get-NetRoute -DestinationPrefix '0.0.0.0/0','::/0' | Select-Object InterfaceAlias,DestinationPrefix);\
        $d=@(Get-DnsClientServerAddress | Where-Object {$_.ServerAddresses} | Select-Object InterfaceAlias,ServerAddresses);\
        [pscustomobject]@{adapters=$a;routes=$r;dns=$d} | ConvertTo-Json -Depth 4 -Compress";

    #[derive(Deserialize, Default)]
    #[serde(rename_all = "PascalCase")]
    struct Adapter {
        #[serde(default)]
        name: String,
        #[serde(default)]
        receive_link_speed: Option<u64>,
        #[serde(default)]
        mtu_size: Option<u32>,
        #[serde(default)]
        media_type: Option<String>,
        #[serde(default)]
        physical_media_type: Option<String>,
        #[serde(default)]
        interface_description: Option<String>,
    }

    #[derive(Deserialize, Default)]
    #[serde(rename_all = "PascalCase")]
    struct Route {
        #[serde(default)]
        interface_alias: String,
        #[serde(default)]
        destination_prefix: String,
    }

    #[derive(Deserialize, Default)]
    #[serde(rename_all = "PascalCase")]
    struct Dns {
        #[serde(default)]
        server_addresses: Vec<String>,
    }

    #[derive(Deserialize, Default)]
    struct Report {
        #[serde(default)]
        adapters: Vec<Adapter>,
        #[serde(default)]
        routes: Vec<Route>,
        #[serde(default)]
        dns: Vec<Dns>,
    }

    fn kind_of(adapter: &Adapter) -> Option<InterfaceKind> {
        let physical = adapter.physical_media_type.as_deref().unwrap_or_default();
        let media = adapter.media_type.as_deref().unwrap_or_default();
        let description = adapter
            .interface_description
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if physical.contains("802.11") || physical.contains("Native 802.11") {
            Some(InterfaceKind::Wifi)
        } else if physical.contains("Wireless WAN") || media.contains("Wireless WAN") {
            Some(InterfaceKind::Cellular)
        } else if description.contains("vpn")
            || description.contains("tap-")
            || description.contains("wireguard")
            || description.contains("tunnel")
        {
            Some(InterfaceKind::Vpn)
        } else if physical.contains("802.3") || media.contains("802.3") {
            Some(InterfaceKind::Ethernet)
        } else {
            None
        }
    }

    pub(super) async fn enrich(result: &mut NetInterfaces) {
        let mut notes = Vec::new();
        let Some(text) = command_output(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
            &mut notes,
        )
        .await
        else {
            result.notes.extend(notes);
            return;
        };
        let report: Report = match serde_json::from_str(text.trim()) {
            Ok(report) => report,
            Err(error) => {
                result
                    .notes
                    .push(format!("adapter report could not be parsed: {error}"));
                return;
            }
        };
        for nic in &mut result.interfaces {
            if let Some(adapter) = report
                .adapters
                .iter()
                .find(|adapter| adapter.name == nic.name)
            {
                nic.mtu = adapter.mtu_size;
                nic.link_speed_mbps = adapter
                    .receive_link_speed
                    .map(|bps| bps / 1_000_000)
                    .filter(|mbps| *mbps > 0);
                if let Some(kind) = kind_of(adapter) {
                    nic.kind = kind;
                }
            }
            nic.default_route_v4 = report.routes.iter().any(|route| {
                route.interface_alias == nic.name && route.destination_prefix == "0.0.0.0/0"
            });
            nic.default_route_v6 = report.routes.iter().any(|route| {
                route.interface_alias == nic.name && route.destination_prefix == "::/0"
            });
        }
        result.notes.extend(notes);
        let mut servers: Vec<IpAddr> = Vec::new();
        for entry in &report.dns {
            for raw in &entry.server_addresses {
                if let Ok(ip) = raw.parse::<IpAddr>()
                    && !servers.contains(&ip)
                {
                    servers.push(ip);
                }
            }
        }
        result.dns_servers = servers.into_iter().map(dns_server).collect();
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::NetInterfaces;

    pub(super) async fn enrich(result: &mut NetInterfaces) {
        result
            .notes
            .push("link speed, MTU, routes and DNS are not available on this platform".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_classify_without_exposing_addresses() {
        assert_eq!(
            scope_v4(Ipv4Addr::new(192, 168, 1, 2)),
            AddressScope::Private
        );
        assert_eq!(
            scope_v4(Ipv4Addr::new(100, 72, 0, 1)),
            AddressScope::SharedCgnat
        );
        assert_eq!(scope_v4(Ipv4Addr::new(8, 8, 8, 8)), AddressScope::Public);
        assert_eq!(
            scope_v4(Ipv4Addr::new(169, 254, 3, 4)),
            AddressScope::LinkLocal
        );
        assert_eq!(
            scope_v6("fe80::1".parse().unwrap()),
            AddressScope::LinkLocal
        );
        assert_eq!(scope_v6("fd00::1".parse().unwrap()), AddressScope::Private);
        assert_eq!(
            scope_v6("2400:3200::1".parse().unwrap()),
            AddressScope::Public
        );
        assert_eq!(dns_server("192.168.1.1".parse().unwrap()).address, None);
        assert_eq!(
            dns_server("223.5.5.5".parse().unwrap()).address.as_deref(),
            Some("223.5.5.5")
        );
    }

    #[test]
    fn resolv_conf_dedupes_and_strips_zone() {
        let servers = parse_resolv_conf(
            "# comment\nnameserver 1.1.1.1\nnameserver fe80::1%en0\nnameserver 1.1.1.1\nsearch lan\n",
        );
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[1], "fe80::1".parse::<IpAddr>().unwrap());
    }
}
