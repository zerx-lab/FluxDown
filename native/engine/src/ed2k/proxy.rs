//! SOCKS5 transport for eD2K TCP and Kad UDP.
//!
//! An empty setting preserves direct eD2K transport; `direct://` explicitly does
//! the same. Configured SOCKS5 failures are terminal and never fall back to direct.
//! Server logins and source-peer TCP dials use SOCKS CONNECT, while Kad uses one
//! UDP ASSOCIATE relay for bootstrap, lookup and source-search datagrams.
//! The inbound eD2K listener remains local, UPnP mapping is disabled, and login
//! advertises port zero while a proxy is active; a proxy exit is not represented
//! as reachable inbound connectivity. Changing the setting drops the shared
//! server session so subsequent logins use the new route; existing peer streams
//! finish on their original route.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::Range;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use crate::downloader::DownloadError;
use crate::protocol_proxy::{DownloadProtocol, parse};
use crate::proxy_config::{ProxyConfig, ProxyMode};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);
pub(super) const MAX_UDP_HEADER: usize = 22;
// All Kad requests built by kad::proto fit comfortably in one packet.
const MAX_WRAPPED_KAD_REQUEST: usize = 1500;

pub(crate) struct Socks5(ProxyConfig);

impl Socks5 {
    pub(crate) fn parse(raw: &str) -> Result<Option<Self>, DownloadError> {
        parse(DownloadProtocol::Ed2k, raw)
            .map(|proxy| proxy.filter(|p| p.mode == ProxyMode::Manual).map(Self))
            .map_err(|error| DownloadError::Ed2k(error.to_owned()))
    }

    async fn control(&self) -> Result<TcpStream, DownloadError> {
        let proxy = &self.0;
        // IPv6 hosts are stored bracketed ("[::1]"); a bracketed &str would go to getaddrinfo and fail.
        let host = proxy.host.trim_matches(['[', ']']);
        let mut stream = TcpStream::connect((host, proxy.port)).await?;
        let method = if proxy.username.is_empty() && proxy.password.is_empty() {
            0
        } else {
            2
        };
        stream.write_all(&[5, 1, method]).await?;
        let mut answer = [0; 2];
        stream.read_exact(&mut answer).await?;
        if answer != [5, method] {
            return Err(DownloadError::Ed2k(
                "SOCKS5 authentication method rejected".into(),
            ));
        }
        if method == 2 {
            authenticate(&mut stream, &proxy.username, &proxy.password).await?;
        }
        Ok(stream)
    }

    pub(crate) async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, DownloadError> {
        let mut request = [0; 262];
        request[..3].copy_from_slice(&[5, 1, 0]);
        let len = write_destination(&mut request, host, port)?;
        self.connect_request(&request[..len]).await
    }

    pub(crate) async fn connect_ip(&self, target: SocketAddr) -> Result<TcpStream, DownloadError> {
        if target.port() == 0 {
            return Err(DownloadError::Ed2k(
                "SOCKS5 destination port is zero".into(),
            ));
        }
        let mut request = [0; MAX_UDP_HEADER];
        request[..3].copy_from_slice(&[5, 1, 0]);
        let len = write_ip(&mut request, target);
        self.connect_request(&request[..len]).await
    }

    async fn connect_request(&self, request: &[u8]) -> Result<TcpStream, DownloadError> {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let mut stream = self.control().await?;
            stream.write_all(request).await?;
            read_reply(&mut stream).await?;
            Ok(stream)
        })
        .await
        .map_err(|_| DownloadError::Ed2k("SOCKS5 CONNECT timed out".into()))?
    }

    async fn associate(&self, port: u16) -> Result<(UdpSocket, TcpStream), DownloadError> {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let mut stream = self.control().await?;
            // The client endpoint is not known until the relay's address family is known.
            stream.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            let reply = read_reply(&mut stream).await?;
            let relay = match reply {
                BoundAddress::Ip(addr) if addr.ip().is_unspecified() => {
                    SocketAddr::new(stream.peer_addr()?.ip(), addr.port())
                }
                BoundAddress::Ip(addr) => addr,
                BoundAddress::Domain(host, port) => tokio::net::lookup_host((host.as_str(), port))
                    .await?
                    .next()
                    .ok_or_else(|| DownloadError::Ed2k("SOCKS5 UDP relay has no address".into()))?,
            };
            if relay.port() == 0 {
                return Err(DownloadError::Ed2k(
                    "SOCKS5 UDP relay returned port zero".into(),
                ));
            }
            let local = if relay.is_ipv4() {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            } else {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            };
            let socket = UdpSocket::bind(SocketAddr::new(local, port)).await?;
            socket.connect(relay).await?;
            Ok((socket, stream))
        })
        .await
        .map_err(|_| DownloadError::Ed2k("SOCKS5 UDP ASSOCIATE timed out".into()))?
    }
}

async fn authenticate(stream: &mut TcpStream, user: &str, pass: &str) -> Result<(), DownloadError> {
    let (Ok(user_len), Ok(pass_len)) = (u8::try_from(user.len()), u8::try_from(pass.len())) else {
        return Err(DownloadError::Ed2k(
            "SOCKS5 credentials exceed 255 bytes".into(),
        ));
    };
    let mut request = [0; 513];
    let split = 2 + user.len();
    let end = split + 1 + pass.len();
    request[..2].copy_from_slice(&[1, user_len]);
    request[2..split].copy_from_slice(user.as_bytes());
    request[split] = pass_len;
    request[split + 1..end].copy_from_slice(pass.as_bytes());
    stream.write_all(&request[..end]).await?;
    let mut answer = [0; 2];
    stream.read_exact(&mut answer).await?;
    if answer != [1, 0] {
        return Err(DownloadError::Ed2k(
            "SOCKS5 username/password rejected".into(),
        ));
    }
    Ok(())
}

fn write_destination(out: &mut [u8; 262], host: &str, port: u16) -> Result<usize, DownloadError> {
    if port == 0 {
        return Err(DownloadError::Ed2k(
            "SOCKS5 destination port is zero".into(),
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(write_ip(out, SocketAddr::new(ip, port)));
    }
    let len = u8::try_from(host.len())
        .ok()
        .filter(|len| *len != 0)
        .ok_or_else(|| DownloadError::Ed2k("invalid SOCKS5 destination hostname".into()))?;
    out[3..5].copy_from_slice(&[3, len]);
    let end = 5 + usize::from(len);
    out[5..end].copy_from_slice(host.as_bytes());
    out[end..end + 2].copy_from_slice(&port.to_be_bytes());
    Ok(end + 2)
}

enum BoundAddress {
    Ip(SocketAddr),
    Domain(String, u16),
}

async fn read_reply(stream: &mut TcpStream) -> Result<BoundAddress, DownloadError> {
    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 || head[1] != 0 || head[2] != 0 {
        return Err(DownloadError::Ed2k(format!(
            "SOCKS5 request failed with code {}",
            head[1]
        )));
    }
    let ip = match head[3] {
        1 => IpAddr::V4(Ipv4Addr::from(stream.read_u32().await?)),
        4 => IpAddr::V6(Ipv6Addr::from(stream.read_u128().await?)),
        3 => {
            let len = usize::from(stream.read_u8().await?);
            let mut domain = vec![0; len];
            stream.read_exact(&mut domain).await?;
            let port = stream.read_u16().await?;
            let host = String::from_utf8(domain)
                .map_err(|_| DownloadError::Ed2k("SOCKS5 response has invalid domain".into()))?;
            return Ok(BoundAddress::Domain(host, port));
        }
        _ => {
            return Err(DownloadError::Ed2k(
                "SOCKS5 response has invalid address type".into(),
            ));
        }
    };
    Ok(BoundAddress::Ip(SocketAddr::new(
        ip,
        stream.read_u16().await?,
    )))
}

fn write_ip(out: &mut [u8], target: SocketAddr) -> usize {
    let end = match target.ip() {
        IpAddr::V4(ip) => {
            out[3] = 1;
            out[4..8].copy_from_slice(&ip.octets());
            8
        }
        IpAddr::V6(ip) => {
            out[3] = 4;
            out[4..20].copy_from_slice(&ip.octets());
            20
        }
    };
    out[end..end + 2].copy_from_slice(&target.port().to_be_bytes());
    end + 2
}

fn encode_udp<'a>(
    out: &'a mut [u8; MAX_WRAPPED_KAD_REQUEST],
    target: SocketAddr,
    payload: &[u8],
) -> std::io::Result<&'a [u8]> {
    out[..3].fill(0);
    let header = write_ip(out, target);
    let end = header.saturating_add(payload.len());
    let body = out
        .get_mut(header..end)
        .ok_or_else(|| std::io::Error::other("SOCKS5 Kad request exceeds packet limit"))?;
    body.copy_from_slice(payload);
    Ok(&out[..end])
}

fn decode_udp(packet: &[u8]) -> Option<(Range<usize>, SocketAddr)> {
    if packet.get(..3)? != [0, 0, 0] {
        return None;
    }
    let (ip, pos) = match *packet.get(3)? {
        1 => (
            IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(4..8)?).ok()?)),
            8,
        ),
        4 => (
            IpAddr::V6(Ipv6Addr::from(
                <[u8; 16]>::try_from(packet.get(4..20)?).ok()?,
            )),
            20,
        ),
        _ => return None,
    };
    let port = u16::from_be_bytes(packet.get(pos..pos + 2)?.try_into().ok()?);
    Some((pos + 2..packet.len(), SocketAddr::new(ip, port)))
}

pub(crate) enum KadSocket {
    Direct(UdpSocket),
    Socks {
        socket: UdpSocket,
        _control: TcpStream,
    },
}

impl KadSocket {
    pub(crate) async fn bind(proxy: Option<&Socks5>, port: u16) -> Result<Self, DownloadError> {
        if let Some(proxy) = proxy {
            let (socket, control) = proxy.associate(port).await?;
            Ok(Self::Socks {
                socket,
                _control: control,
            })
        } else {
            let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
            Ok(Self::Direct(socket))
        }
    }

    pub(crate) async fn send_to(&self, bytes: &[u8], target: SocketAddr) -> std::io::Result<usize> {
        match self {
            Self::Direct(socket) => socket.send_to(bytes, target).await,
            Self::Socks { socket, .. } => {
                let mut out = [0; MAX_WRAPPED_KAD_REQUEST];
                let packet = encode_udp(&mut out, target, bytes)?;
                socket.send(packet).await.map(|_| bytes.len())
            }
        }
    }

    pub(crate) async fn recv_from(
        &self,
        bytes: &mut [u8],
    ) -> std::io::Result<(Range<usize>, SocketAddr)> {
        match self {
            Self::Direct(socket) => socket.recv_from(bytes).await.map(|(n, from)| (0..n, from)),
            Self::Socks { socket, .. } => loop {
                let n = socket.recv(bytes).await?;
                if let Some(datagram) = decode_udp(&bytes[..n]) {
                    return Ok(datagram);
                }
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{KadSocket, MAX_WRAPPED_KAD_REQUEST, Socks5, decode_udp, encode_udp};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    #[tokio::test]
    async fn tcp_peer_connect_authenticates_and_uses_socks_remote_dns() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control");
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.expect("greeting");
            assert_eq!(greeting, [5, 1, 2]);
            stream.write_all(&[5, 2]).await.expect("method");
            let mut auth = [0; 9];
            stream.read_exact(&mut auth).await.expect("authentication");
            assert_eq!(&auth, b"\x01\x03u@x\x03p:w");
            stream
                .write_all(&[1, 0])
                .await
                .expect("authentication reply");
            let mut head = [0; 5];
            stream.read_exact(&mut head).await.expect("request head");
            assert_eq!(head, [5, 1, 0, 3, 15]);
            let mut host = [0; 15];
            stream.read_exact(&mut host).await.expect("hostname");
            assert_eq!(&host, b"example.invalid");
            let mut port = [0; 2];
            stream
                .read_exact(&mut port)
                .await
                .expect("destination port");
            assert_eq!(u16::from_be_bytes(port), 4662);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .await
                .expect("connect reply");
            stream.write_all(b"peer").await.expect("peer payload");
        });
        let proxy = Socks5::parse(&format!("socks5h://u%40x:p%3Aw@{addr}"))
            .expect("parse")
            .expect("configured");
        let mut stream = proxy
            .connect("example.invalid", 4662)
            .await
            .expect("connect");
        let mut data = [0; 4];
        stream.read_exact(&mut data).await.expect("peer data");
        assert_eq!(&data, b"peer");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn ipv6_literal_proxy_host_is_dialed_without_brackets() {
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("ipv6 proxy");
        let port = listener.local_addr().expect("proxy addr").port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control");
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.expect("greeting");
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await.expect("method");
            let mut head = [0; 5];
            stream.read_exact(&mut head).await.expect("request head");
            assert_eq!(head, [5, 1, 0, 3, 15]);
            let mut rest = [0; 17];
            stream.read_exact(&mut rest).await.expect("destination");
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .await
                .expect("connect reply");
        });
        let proxy = Socks5::parse(&format!("socks5h://[::1]:{port}"))
            .expect("parse")
            .expect("configured");
        proxy
            .connect("example.invalid", 4662)
            .await
            .expect("connect via bracketed IPv6 proxy host");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn kad_datagrams_use_socks_udp_associate() {
        let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("proxy tcp");
        let tcp_addr = tcp.local_addr().expect("tcp address");
        let relay = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("relay");
        let relay_addr = relay.local_addr().expect("relay address");
        let relay_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (n, client) = relay.recv_from(&mut buf).await.expect("wrapped request");
            let (payload, target) = decode_udp(&buf[..n]).expect("UDP request envelope");
            assert_eq!(target, SocketAddr::from(([198, 51, 100, 7], 4665)));
            let mut out = [0; MAX_WRAPPED_KAD_REQUEST];
            let response = encode_udp(
                &mut out,
                SocketAddr::from(([203, 0, 113, 9], 4672)),
                &buf[payload],
            )
            .expect("response envelope");
            relay
                .send_to(response, client)
                .await
                .expect("wrapped response");
        });
        let proxy_task = tokio::spawn(async move {
            let (mut control, _) = tcp.accept().await.expect("control");
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.expect("greeting");
            assert_eq!(greeting, [5, 1, 0]);
            control.write_all(&[5, 0]).await.expect("method");
            let mut request = [0; 10];
            control.read_exact(&mut request).await.expect("associate");
            assert_eq!(request[1], 3);
            // An unspecified relay address uses the TCP peer's IP, not its port.
            let mut response = vec![5, 0, 0, 1, 0, 0, 0, 0];
            response.extend_from_slice(&relay_addr.port().to_be_bytes());
            control.write_all(&response).await.expect("associate reply");
            // Keep control alive for the lifetime of the UDP association.
            let mut eof = [0];
            assert_eq!(control.read(&mut eof).await.expect("control EOF"), 0);
        });
        let proxy = Socks5::parse(&format!("socks5h://{tcp_addr}"))
            .expect("parse proxy")
            .expect("configured");
        let socket = KadSocket::bind(Some(&proxy), 0).await.expect("associate");
        let target = SocketAddr::from(([198, 51, 100, 7], 4665));
        socket.send_to(b"request", target).await.expect("send");
        let mut buf = [0u8; 64];
        let (payload, from) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            socket.recv_from(&mut buf),
        )
        .await
        .expect("reply timeout")
        .expect("reply");
        assert_eq!(from, SocketAddr::from(([203, 0, 113, 9], 4672)));
        assert_eq!(&buf[payload], b"request");
        relay_task.await.expect("relay task");
        drop(socket);
        proxy_task.await.expect("proxy task");
    }
}
