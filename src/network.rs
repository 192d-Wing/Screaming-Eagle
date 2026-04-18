//! Network-layer features: IPv6-only bind mode, multi-certificate SNI
//! resolution, and PROXY protocol v2 parsing.
//!
//! Anycast is a BGP-layer concern, so no application feature is required —
//! the CDN just needs to be stateless and bind-mode flexible, which these
//! features provide.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Network layer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Address-family preference for the listening socket.
    #[serde(default)]
    pub bind_mode: BindMode,

    /// Accept PROXY protocol v2 headers on inbound connections. Only enable
    /// when behind a trusted L4 load balancer.
    #[serde(default)]
    pub proxy_protocol: ProxyProtocolConfig,

    /// Multi-certificate SNI routing.
    #[serde(default)]
    pub sni: SniConfig,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_mode: BindMode::default(),
            proxy_protocol: ProxyProtocolConfig::default(),
            sni: SniConfig::default(),
        }
    }
}

/// Bind-mode policy for the listening socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindMode {
    /// Bind IPv4 only.
    V4Only,
    /// Bind IPv6 only (IPV6_V6ONLY = 1).
    V6Only,
    /// Dual-stack: bind `::` and accept v4-mapped addresses (v6only = 0).
    Dual,
}

impl Default for BindMode {
    fn default() -> Self {
        BindMode::Dual
    }
}

/// PROXY protocol v2 configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyProtocolConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Trusted proxy source CIDRs. When set, PROXY headers are only honored
    /// from these sources. If empty and enabled, all sources are trusted
    /// (dangerous).
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

impl Default for ProxyProtocolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trusted_proxies: Vec::new(),
        }
    }
}

/// Multi-certificate SNI routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Hostname → cert/key path mapping. Wildcards `*.example.com` match one
    /// label. An entry with hostname `"default"` becomes the fallback.
    #[serde(default)]
    pub certificates: Vec<SniCertificate>,
}

impl Default for SniConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            certificates: Vec::new(),
        }
    }
}

/// A single (hostname, cert, key) triple used by the SNI resolver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniCertificate {
    /// Hostname pattern (supports `*.example.com` and the literal `default`).
    pub hostname: String,

    /// Path to the PEM-encoded certificate chain.
    pub cert_path: String,

    /// Path to the PEM-encoded private key.
    pub key_path: String,
}

// ---------------------------------------------------------------------------
// Bind address resolution
// ---------------------------------------------------------------------------

/// Resolve a listen address given a host, port, and bind-mode policy.
pub fn resolve_bind_address(host: &str, port: u16, mode: BindMode) -> SocketAddr {
    match mode {
        BindMode::V4Only => {
            let ip: Ipv4Addr = host
                .parse()
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            SocketAddr::V4(SocketAddrV4::new(ip, port))
        }
        BindMode::V6Only | BindMode::Dual => {
            let ip: Ipv6Addr = match host {
                // Translate any IPv4 unspecified to v6 unspecified.
                "0.0.0.0" => Ipv6Addr::UNSPECIFIED,
                h => h.parse().unwrap_or(Ipv6Addr::UNSPECIFIED),
            };
            SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0))
        }
    }
}

/// Build a TCP listener that honors the bind-mode policy.
///
/// For `V6Only`, we set `IPV6_V6ONLY=1` so the socket only accepts v6
/// traffic — required for strict IPv6-only deployments.
///
/// For `Dual`, we disable `IPV6_V6ONLY` so v4-mapped addresses also connect.
pub async fn build_listener(
    host: &str,
    port: u16,
    mode: BindMode,
) -> std::io::Result<tokio::net::TcpListener> {
    let addr = resolve_bind_address(host, port, mode);
    let socket = match addr {
        SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4()?,
        SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6()?,
    };

    if let SocketAddr::V6(_) = addr {
        match mode {
            BindMode::V6Only => set_v6only(&socket, true)?,
            BindMode::Dual => set_v6only(&socket, false)?,
            BindMode::V4Only => {}
        }
    }

    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

#[cfg(unix)]
fn set_v6only(socket: &tokio::net::TcpSocket, only: bool) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = socket.as_raw_fd();
    let val: libc::c_int = if only { 1 } else { 0 };
    // SAFETY: `fd` is valid for the lifetime of `socket`; setsockopt is a
    // standard POSIX call that writes exactly sizeof(int) bytes from `val`.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of_val(&val) as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_v6only(_socket: &tokio::net::TcpSocket, _only: bool) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// SNI hostname matching
// ---------------------------------------------------------------------------

/// Select the best certificate for a given SNI hostname.
///
/// Match precedence: exact hostname > wildcard > "default".
pub fn select_sni_cert<'a>(
    sni: Option<&str>,
    certs: &'a [SniCertificate],
) -> Option<&'a SniCertificate> {
    if certs.is_empty() {
        return None;
    }
    let host = match sni {
        Some(s) if !s.is_empty() => s.to_ascii_lowercase(),
        _ => {
            return certs.iter().find(|c| c.hostname == "default");
        }
    };

    // Exact match first.
    if let Some(c) = certs.iter().find(|c| c.hostname.eq_ignore_ascii_case(&host)) {
        return Some(c);
    }

    // Wildcard match: `*.example.com` matches exactly one leading label.
    for c in certs.iter() {
        if let Some(suffix) = c.hostname.strip_prefix("*.") {
            if host_matches_wildcard(&host, suffix) {
                return Some(c);
            }
        }
    }

    certs.iter().find(|c| c.hostname == "default")
}

fn host_matches_wildcard(host: &str, suffix: &str) -> bool {
    // Wildcard covers exactly one label: `*.example.com` matches
    // `a.example.com` but not `a.b.example.com` and not `example.com`.
    if host == suffix {
        return false;
    }
    match host.strip_suffix(suffix) {
        Some(prefix) => {
            let prefix = prefix.strip_suffix('.').unwrap_or(prefix);
            !prefix.is_empty() && !prefix.contains('.')
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// PROXY protocol v2 parsing
// ---------------------------------------------------------------------------

const PP2_SIGNATURE: &[u8; 12] = b"\r\n\r\n\x00\r\nQUIT\n";

/// Result of a successful PROXY v2 parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyInfo {
    pub client: Option<SocketAddr>,
    pub server: Option<SocketAddr>,
}

/// Parse a PROXY protocol v2 header from the start of `buf`. Returns the
/// consumed byte count and parsed info. Returns `Ok(None)` if the buffer is
/// too short to contain a valid v2 header.
pub fn parse_proxy_v2(buf: &[u8]) -> Result<Option<(usize, ProxyInfo)>, ProxyError> {
    if buf.len() < 16 {
        return Ok(None);
    }
    if &buf[..12] != PP2_SIGNATURE {
        return Err(ProxyError::BadSignature);
    }

    let ver_cmd = buf[12];
    let version = ver_cmd >> 4;
    let command = ver_cmd & 0x0F;
    if version != 0x2 {
        return Err(ProxyError::UnsupportedVersion(version));
    }

    let fam_proto = buf[13];
    let family = fam_proto >> 4; // 0 unspec, 1 inet, 2 inet6, 3 unix
    let length = u16::from_be_bytes([buf[14], buf[15]]) as usize;
    let total = 16 + length;
    if buf.len() < total {
        return Ok(None);
    }

    // LOCAL command: ignore addresses; use socket peer.
    if command == 0x0 {
        return Ok(Some((total, ProxyInfo { client: None, server: None })));
    }
    if command != 0x1 {
        return Err(ProxyError::UnsupportedCommand(command));
    }

    let addr_bytes = &buf[16..total];
    let info = match family {
        0x1 if addr_bytes.len() >= 12 => {
            let src: [u8; 4] = addr_bytes[0..4].try_into().unwrap();
            let dst: [u8; 4] = addr_bytes[4..8].try_into().unwrap();
            let sport = u16::from_be_bytes([addr_bytes[8], addr_bytes[9]]);
            let dport = u16::from_be_bytes([addr_bytes[10], addr_bytes[11]]);
            ProxyInfo {
                client: Some(SocketAddr::new(IpAddr::V4(src.into()), sport)),
                server: Some(SocketAddr::new(IpAddr::V4(dst.into()), dport)),
            }
        }
        0x2 if addr_bytes.len() >= 36 => {
            let mut src = [0u8; 16];
            let mut dst = [0u8; 16];
            src.copy_from_slice(&addr_bytes[0..16]);
            dst.copy_from_slice(&addr_bytes[16..32]);
            let sport = u16::from_be_bytes([addr_bytes[32], addr_bytes[33]]);
            let dport = u16::from_be_bytes([addr_bytes[34], addr_bytes[35]]);
            ProxyInfo {
                client: Some(SocketAddr::new(IpAddr::V6(src.into()), sport)),
                server: Some(SocketAddr::new(IpAddr::V6(dst.into()), dport)),
            }
        }
        0x0 => ProxyInfo { client: None, server: None },
        _ => return Err(ProxyError::UnsupportedFamily(family)),
    };

    Ok(Some((total, info)))
}

/// Errors produced while parsing a PROXY protocol v2 header.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProxyError {
    #[error("bad PROXY v2 signature")]
    BadSignature,
    #[error("unsupported PROXY version: {0}")]
    UnsupportedVersion(u8),
    #[error("unsupported PROXY command: {0}")]
    UnsupportedCommand(u8),
    #[error("unsupported PROXY address family: {0}")]
    UnsupportedFamily(u8),
}

/// Check whether a peer address falls within any of the configured trusted
/// proxy CIDRs. Empty list means all are trusted.
pub fn is_trusted_proxy(peer: IpAddr, trusted: &[String]) -> bool {
    if trusted.is_empty() {
        return true;
    }
    trusted.iter().any(|cidr| cidr_contains(cidr, peer))
}

fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let (net, prefix) = match cidr.split_once('/') {
        Some((n, p)) => (n, p),
        None => return false,
    };
    let prefix: u8 = match prefix.parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let net: IpAddr = match net.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) if prefix <= 32 => {
            let mask: u32 = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if prefix <= 128 => {
            let mask: u128 = if prefix == 0 {
                0
            } else {
                !0u128 << (128 - prefix)
            };
            (u128::from(a) & mask) == (u128::from(b) & mask)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_mode_defaults_to_dual() {
        assert_eq!(BindMode::default(), BindMode::Dual);
    }

    #[test]
    fn resolve_bind_addr_v4_only() {
        let a = resolve_bind_address("127.0.0.1", 8080, BindMode::V4Only);
        assert!(matches!(a, SocketAddr::V4(_)));
    }

    #[test]
    fn resolve_bind_addr_v6_only_from_v4_wildcard() {
        let a = resolve_bind_address("0.0.0.0", 8080, BindMode::V6Only);
        assert!(matches!(a, SocketAddr::V6(_)));
    }

    #[test]
    fn sni_exact_match_wins_over_wildcard() {
        let certs = vec![
            SniCertificate {
                hostname: "*.example.com".into(),
                cert_path: "w".into(),
                key_path: "w".into(),
            },
            SniCertificate {
                hostname: "api.example.com".into(),
                cert_path: "a".into(),
                key_path: "a".into(),
            },
        ];
        let c = select_sni_cert(Some("api.example.com"), &certs).unwrap();
        assert_eq!(c.cert_path, "a");
    }

    #[test]
    fn sni_wildcard_matches_one_label() {
        let certs = vec![SniCertificate {
            hostname: "*.example.com".into(),
            cert_path: "w".into(),
            key_path: "w".into(),
        }];
        assert!(select_sni_cert(Some("api.example.com"), &certs).is_some());
        assert!(select_sni_cert(Some("a.b.example.com"), &certs).is_none());
        assert!(select_sni_cert(Some("example.com"), &certs).is_none());
    }

    #[test]
    fn sni_default_is_fallback() {
        let certs = vec![
            SniCertificate {
                hostname: "api.example.com".into(),
                cert_path: "a".into(),
                key_path: "a".into(),
            },
            SniCertificate {
                hostname: "default".into(),
                cert_path: "d".into(),
                key_path: "d".into(),
            },
        ];
        let c = select_sni_cert(Some("other.com"), &certs).unwrap();
        assert_eq!(c.cert_path, "d");
        let c = select_sni_cert(None, &certs).unwrap();
        assert_eq!(c.cert_path, "d");
    }

    #[test]
    fn proxy_v2_short_buffer_returns_none() {
        let out = parse_proxy_v2(&[0u8; 4]).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn proxy_v2_bad_signature() {
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(b"not a signat");
        assert!(matches!(parse_proxy_v2(&buf), Err(ProxyError::BadSignature)));
    }

    #[test]
    fn proxy_v2_parses_ipv4() {
        let mut buf = Vec::new();
        buf.extend_from_slice(PP2_SIGNATURE);
        buf.push(0x21); // version 2, PROXY
        buf.push(0x11); // inet, stream
        buf.extend_from_slice(&12u16.to_be_bytes()); // length
        buf.extend_from_slice(&[1, 2, 3, 4]); // src
        buf.extend_from_slice(&[5, 6, 7, 8]); // dst
        buf.extend_from_slice(&1234u16.to_be_bytes());
        buf.extend_from_slice(&80u16.to_be_bytes());
        let (consumed, info) = parse_proxy_v2(&buf).unwrap().unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(
            info.client.unwrap(),
            "1.2.3.4:1234".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            info.server.unwrap(),
            "5.6.7.8:80".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn proxy_v2_local_command_clears_addrs() {
        let mut buf = Vec::new();
        buf.extend_from_slice(PP2_SIGNATURE);
        buf.push(0x20); // version 2, LOCAL
        buf.push(0x00); // unspec
        buf.extend_from_slice(&0u16.to_be_bytes());
        let (consumed, info) = parse_proxy_v2(&buf).unwrap().unwrap();
        assert_eq!(consumed, 16);
        assert_eq!(info.client, None);
    }

    #[test]
    fn trusted_proxy_allows_empty_list() {
        assert!(is_trusted_proxy(
            "10.0.0.1".parse().unwrap(),
            &[] as &[String]
        ));
    }

    #[test]
    fn trusted_proxy_matches_cidr() {
        let trusted = vec!["10.0.0.0/8".to_string()];
        assert!(is_trusted_proxy("10.1.2.3".parse().unwrap(), &trusted));
        assert!(!is_trusted_proxy("11.0.0.1".parse().unwrap(), &trusted));
    }
}
