//! Proxy configuration module.
//!
//! Provides the [`ProxyConfig`] type that holds user/system proxy settings and
//! helper functions for:
//! - Building proxy URLs for reqwest (`to_proxy_url`)
//! - Detecting Windows system proxy via the registry
//! - Parsing a Windows `ProxyServer` registry value (multi-protocol format)

use std::collections::HashMap;

use crate::downloader::DownloadError;

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
}

impl ProxyMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "manual" => Self::Manual,
            _ => Self::None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::System => "system",
            Self::Manual => "manual",
        }
    }
}

/// Supported proxy protocol types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyType {
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "https" => Self::Https,
            "socks4" => Self::Socks4,
            "socks5" => Self::Socks5,
            _ => Self::Http,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks5 => "socks5",
        }
    }

    /// URL scheme used by reqwest's `Proxy::all(url)`.
    pub fn scheme(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks5 => "socks5",
        }
    }

    /// Whether this is a SOCKS variant (4 or 5).
    #[allow(dead_code)]
    pub fn is_socks(&self) -> bool {
        matches!(self, Self::Socks4 | Self::Socks5)
    }
}

// ---------------------------------------------------------------------------
// ProxyConfig
// ---------------------------------------------------------------------------

/// Complete proxy configuration.
#[derive(Debug, Clone)]
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
            .map(|v| ProxyMode::from_str(v))
            .unwrap_or(ProxyMode::None);
        let proxy_type = map
            .get("proxy_type")
            .map(|v| ProxyType::from_str(v))
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

    /// Build the full proxy URL string, e.g. `socks5://user:pass@host:port`.
    ///
    /// Used by reqwest's `Proxy::all(url)` and for display purposes.
    /// Returns `None` if mode is `None` or host is empty.
    pub fn to_proxy_url(&self) -> Option<String> {
        match self.mode {
            ProxyMode::None => None,
            ProxyMode::System => {
                // System proxy is resolved at call time via detect_system_proxy()
                None
            }
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
                        rinf::debug_print!("[proxy] system proxy detection failed: {}", e);
                        Self::default()
                    }
                }
            }
            _ => self.clone(),
        }
    }

    /// Return the `host:port` string for direct socket connections (FTP SOCKS proxy).
    #[allow(dead_code)]
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Parse a proxy URL string like `socks5://user:pass@host:port` into a ProxyConfig.
    ///
    /// Used for per-task proxy override where the user provides a single URL.
    pub fn from_proxy_url(url: &str) -> Self {
        if url.is_empty() {
            return Self::default();
        }

        // Extract scheme
        let (scheme, rest) = if let Some(idx) = url.find("://") {
            (&url[..idx], &url[idx + 3..])
        } else {
            ("http", url)
        };

        let proxy_type = ProxyType::from_str(scheme);

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
        let (host, port) = parse_host_port(host_port);

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
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

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

/// Fallback for non-Windows platforms — returns `None`.
#[cfg(not(target_os = "windows"))]
pub fn detect_system_proxy() -> Result<Option<ProxyConfig>, DownloadError> {
    // On non-Windows, reqwest already reads HTTP_PROXY/HTTPS_PROXY env vars.
    // We don't need extra detection.
    Ok(None)
}

/// Parse the Windows `ProxyServer` registry value.
///
/// Handles both formats:
/// - Simple: `host:port` → (Http, host, port)
/// - Multi-protocol: `http=host:port;https=host:port;socks=host:port` → prefer https > socks > http
pub fn parse_windows_proxy_server(server: &str) -> (ProxyType, String, u16) {
    // Check if it's multi-protocol format (contains '=')
    if server.contains('=') {
        let entries = parse_multi_protocol_proxy(server);

        // Priority: socks > https > http
        if let Some((host, port)) = entries.get("socks") {
            return (ProxyType::Socks5, host.clone(), *port);
        }
        if let Some((host, port)) = entries.get("https") {
            return (ProxyType::Https, host.clone(), *port);
        }
        if let Some((host, port)) = entries.get("http") {
            return (ProxyType::Http, host.clone(), *port);
        }
        // Fallback: take first entry
        if let Some((_key, (host, port))) = entries.into_iter().next() {
            return (ProxyType::Http, host, port);
        }
    }

    // Simple format: "host:port"
    let (host, port) = parse_host_port(server);
    (ProxyType::Http, host, port)
}

/// Parse multi-protocol proxy string like `http=host:port;https=host2:port2;socks=host3:port3`.
fn parse_multi_protocol_proxy(server: &str) -> HashMap<String, (String, u16)> {
    let mut result = HashMap::new();
    for entry in server.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((protocol, addr)) = entry.split_once('=') {
            let protocol = protocol.trim().to_ascii_lowercase();
            let (host, port) = parse_host_port(addr.trim());
            if !host.is_empty() {
                result.insert(protocol, (host, port));
            }
        }
    }
    result
}

/// Parse `host:port` string, defaulting port to 8080 if missing/invalid.
fn parse_host_port(addr: &str) -> (String, u16) {
    // Handle IPv6: [::1]:port
    if let Some(bracket_end) = addr.find(']') {
        let host = addr[..=bracket_end].to_string();
        let rest = &addr[bracket_end + 1..];
        let port = rest
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8080);
        return (host, port);
    }

    // Standard host:port
    if let Some(colon) = addr.rfind(':') {
        let host = addr[..colon].to_string();
        let port = addr[colon + 1..].parse::<u16>().unwrap_or(8080);
        if !host.is_empty() {
            return (host, port);
        }
    }

    // No port specified
    if !addr.is_empty() {
        return (addr.to_string(), 8080);
    }

    (String::new(), 0)
}

// ---------------------------------------------------------------------------
// URL percent-encoding helpers (for proxy credentials)
// ---------------------------------------------------------------------------

/// Decode percent-encoded strings (e.g. `p%40ss` → `p@ss`).
/// Used to decode usernames/passwords from proxy URLs.
fn percent_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            result.push(byte);
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

/// Establish a TCP connection through a SOCKS4 proxy (synchronous).
///
/// SOCKS4 only supports IPv4 addresses — domain names are resolved locally
/// before connecting (SOCKS4a would support remote resolution, but is less
/// commonly configured).
pub fn socks4_connect_sync(
    proxy_host: &str,
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    timeout: std::time::Duration,
) -> Result<std::net::TcpStream, DownloadError> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

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

    // Resolve target to IPv4
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

    let ip_bytes = match target_ip.ip() {
        std::net::IpAddr::V4(ipv4) => ipv4.octets(),
        _ => {
            return Err(DownloadError::Other(
                "SOCKS4 requires IPv4 address".to_string(),
            ))
        }
    };

    // SOCKS4 CONNECT request
    let req = vec![
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

/// Convenience: connect through either SOCKS4 or SOCKS5 based on proxy config.
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
        ProxyType::Socks4 => {
            socks4_connect_sync(&proxy.host, proxy.port, target_host, target_port, timeout)
        }
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
    use std::io::{BufRead, BufReader, Write};
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

    let mut stream_write = stream;
    stream_write
        .write_all(req.as_bytes())
        .map_err(|e| DownloadError::Other(format!("HTTP CONNECT write error: {}", e)))?;

    // Read response status line
    let mut reader = BufReader::new(stream_write);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| DownloadError::Other(format!("HTTP CONNECT read error: {}", e)))?;

    // Parse status code (e.g., "HTTP/1.1 200 Connection established")
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    if status_code != 200 {
        return Err(DownloadError::Other(format!(
            "HTTP CONNECT failed: {}",
            status_line.trim()
        )));
    }

    // Read remaining headers until empty line
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| DownloadError::Other(format!("HTTP CONNECT header read error: {}", e)))?;
        if line.trim().is_empty() {
            break;
        }
    }

    let stream = reader.into_inner();
    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();

    Ok(stream)
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
        ProxyType::Socks4 | ProxyType::Socks5 => {
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

    let config = ProxyConfig {
        mode: ProxyMode::Manual,
        proxy_type: ProxyType::from_str(proxy_type),
        host: proxy_host.to_string(),
        port: proxy_port.parse::<u16>().unwrap_or(0),
        username: proxy_username.to_string(),
        password: proxy_password.to_string(),
        no_proxy_list: String::new(),
    };

    let proxy_url = config.to_proxy_url().ok_or_else(|| {
        DownloadError::Other("incomplete proxy config (host or port missing)".to_string())
    })?;

    rinf::debug_print!("[proxy-test] testing proxy: {}", proxy_url);

    let mut proxy = reqwest::Proxy::all(&proxy_url).map_err(|e| {
        DownloadError::Other(format!("invalid proxy URL: {}", e))
    })?;

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
        rinf::debug_print!("[proxy-test] trying: {}", url);
        let start = Instant::now();

        match client.head(*url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as i64;
                let status = resp.status();

                rinf::debug_print!(
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
                rinf::debug_print!("[proxy-test] {} → error: {}", url, e);
                last_err = format!("{}: {}", url, e);
            }
        }
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
mod tests {
    use super::{
        base64_encode, parse_host_port, parse_multi_protocol_proxy, parse_windows_proxy_server,
        percent_decode, percent_encode_userinfo, ProxyConfig, ProxyMode, ProxyType,
    };
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // ProxyMode
    // -----------------------------------------------------------------------

    #[test]
    fn proxy_mode_from_str_roundtrip() {
        assert_eq!(ProxyMode::from_str("none"), ProxyMode::None);
        assert_eq!(ProxyMode::from_str("system"), ProxyMode::System);
        assert_eq!(ProxyMode::from_str("manual"), ProxyMode::Manual);
        assert_eq!(ProxyMode::from_str("unknown"), ProxyMode::None);
        assert_eq!(ProxyMode::from_str(""), ProxyMode::None);
    }

    #[test]
    fn proxy_mode_as_str() {
        assert_eq!(ProxyMode::None.as_str(), "none");
        assert_eq!(ProxyMode::System.as_str(), "system");
        assert_eq!(ProxyMode::Manual.as_str(), "manual");
    }

    // -----------------------------------------------------------------------
    // ProxyType
    // -----------------------------------------------------------------------

    #[test]
    fn proxy_type_from_str_roundtrip() {
        assert_eq!(ProxyType::from_str("http"), ProxyType::Http);
        assert_eq!(ProxyType::from_str("https"), ProxyType::Https);
        assert_eq!(ProxyType::from_str("socks4"), ProxyType::Socks4);
        assert_eq!(ProxyType::from_str("socks5"), ProxyType::Socks5);
        assert_eq!(ProxyType::from_str("unknown"), ProxyType::Http);
        assert_eq!(ProxyType::from_str(""), ProxyType::Http);
    }

    #[test]
    fn proxy_type_scheme() {
        assert_eq!(ProxyType::Http.scheme(), "http");
        assert_eq!(ProxyType::Https.scheme(), "https");
        assert_eq!(ProxyType::Socks4.scheme(), "socks4");
        assert_eq!(ProxyType::Socks5.scheme(), "socks5");
    }

    #[test]
    fn proxy_type_is_socks() {
        assert!(!ProxyType::Http.is_socks());
        assert!(!ProxyType::Https.is_socks());
        assert!(ProxyType::Socks4.is_socks());
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
            Some("socks5://admin:secret@socks.example.com:1080")
        );
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
        let (h, p) = parse_host_port("proxy.com:8080");
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_no_port_defaults_8080() {
        let (h, p) = parse_host_port("proxy.com");
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_empty() {
        let (h, p) = parse_host_port("");
        assert!(h.is_empty());
        assert_eq!(p, 0);
    }

    #[test]
    fn parse_host_port_ipv6() {
        let (h, p) = parse_host_port("[::1]:8080");
        assert_eq!(h, "[::1]");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_ipv6_no_port() {
        let (h, p) = parse_host_port("[::1]");
        assert_eq!(h, "[::1]");
        assert_eq!(p, 8080);
    }

    #[test]
    fn parse_host_port_invalid_port() {
        let (h, p) = parse_host_port("proxy.com:abc");
        assert_eq!(h, "proxy.com");
        assert_eq!(p, 8080); // defaults to 8080
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

    #[test]
    fn parse_windows_proxy_multi_prefers_https_over_http() {
        let (ty, host, port) = parse_windows_proxy_server("http=a:80;https=b:443");
        assert_eq!(ty, ProxyType::Https);
        assert_eq!(host, "b");
        assert_eq!(port, 443);
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
        assert!(url.starts_with("socks5://"));
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
    // System proxy detection (Windows-only)
    // -----------------------------------------------------------------------

    #[cfg(target_os = "windows")]
    #[test]
    fn detect_system_proxy_does_not_panic() {
        // Just ensure it doesn't crash — result depends on user's system config
        let result = super::detect_system_proxy();
        assert!(result.is_ok());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn detect_system_proxy_returns_none_on_non_windows() {
        let result = super::detect_system_proxy();
        assert!(result.is_ok());
        assert!(result.unwrap_or(None).is_none());
    }
}
