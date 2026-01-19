//! Security module for Screaming Eagle CDN
//!
//! Provides security headers middleware, request signing (HMAC validation),
//! and IP-based access control.

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::config::SecurityConfig;

type HmacSha256 = Hmac<Sha256>;

/// Security state for middleware
#[derive(Clone)]
pub struct Security {
    config: SecurityConfig,
}

impl Security {
    pub fn new(config: SecurityConfig) -> Self {
        Self { config }
    }

    /// Check if security headers are enabled
    pub fn headers_enabled(&self) -> bool {
        self.config.headers.enabled
    }

    /// Check if request signing is enabled
    pub fn signing_enabled(&self) -> bool {
        self.config.signing.enabled
    }

    /// Check if IP access control is enabled
    pub fn ip_control_enabled(&self) -> bool {
        self.config.ip_access.enabled
    }
}

/// Middleware to add security headers to all responses
pub async fn security_headers_middleware(
    State(security): State<Arc<Security>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;

    if !security.headers_enabled() {
        return response;
    }

    let headers = response.headers_mut();
    let config = &security.config.headers;

    // Content-Security-Policy
    if let Some(ref csp) = config.content_security_policy {
        if let Ok(value) = HeaderValue::from_str(csp) {
            headers.insert("content-security-policy", value);
        }
    }

    // X-Frame-Options
    if let Some(ref xfo) = config.x_frame_options {
        if let Ok(value) = HeaderValue::from_str(xfo) {
            headers.insert("x-frame-options", value);
        }
    }

    // X-Content-Type-Options
    if config.x_content_type_options {
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
    }

    // X-XSS-Protection
    if let Some(ref xxss) = config.x_xss_protection {
        if let Ok(value) = HeaderValue::from_str(xxss) {
            headers.insert("x-xss-protection", value);
        }
    }

    // Strict-Transport-Security (HSTS)
    if let Some(ref hsts) = config.strict_transport_security {
        if let Ok(value) = HeaderValue::from_str(hsts) {
            headers.insert("strict-transport-security", value);
        }
    }

    // Referrer-Policy
    if let Some(ref rp) = config.referrer_policy {
        if let Ok(value) = HeaderValue::from_str(rp) {
            headers.insert("referrer-policy", value);
        }
    }

    // Permissions-Policy
    if let Some(ref pp) = config.permissions_policy {
        if let Ok(value) = HeaderValue::from_str(pp) {
            headers.insert("permissions-policy", value);
        }
    }

    // Cross-Origin-Embedder-Policy
    if let Some(ref coep) = config.cross_origin_embedder_policy {
        if let Ok(value) = HeaderValue::from_str(coep) {
            headers.insert("cross-origin-embedder-policy", value);
        }
    }

    // Cross-Origin-Opener-Policy
    if let Some(ref coop) = config.cross_origin_opener_policy {
        if let Ok(value) = HeaderValue::from_str(coop) {
            headers.insert("cross-origin-opener-policy", value);
        }
    }

    // Cross-Origin-Resource-Policy
    if let Some(ref corp) = config.cross_origin_resource_policy {
        if let Ok(value) = HeaderValue::from_str(corp) {
            headers.insert("cross-origin-resource-policy", value);
        }
    }

    // Remove server header if configured
    if config.remove_server_header {
        headers.remove(header::SERVER);
    }

    response
}

/// Middleware for HMAC request signing validation
pub async fn request_signing_middleware(
    State(security): State<Arc<Security>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !security.signing_enabled() {
        return next.run(request).await;
    }

    let config = &security.config.signing;
    let secret = match &config.secret_key {
        Some(key) => key,
        None => {
            warn!("Request signing enabled but no secret key configured");
            return next.run(request).await;
        }
    };

    // Extract signature from header
    let signature_header = config
        .signature_header
        .as_deref()
        .unwrap_or("X-Signature-256");

    let provided_signature = match request.headers().get(signature_header) {
        Some(value) => match value.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "Invalid signature header").into_response();
            }
        },
        None => {
            // Check if signing is required or optional
            if config.require_signature {
                debug!("Missing signature header: {}", signature_header);
                return (
                    StatusCode::UNAUTHORIZED,
                    format!("Missing required header: {}", signature_header),
                )
                    .into_response();
            }
            return next.run(request).await;
        }
    };

    // Extract timestamp if required
    let timestamp_header = config.timestamp_header.as_deref().unwrap_or("X-Timestamp");

    if config.require_timestamp {
        match validate_timestamp(
            request.headers(),
            timestamp_header,
            config.timestamp_tolerance_secs,
        ) {
            Ok(()) => {}
            Err(msg) => {
                return (StatusCode::UNAUTHORIZED, msg).into_response();
            }
        }
    }

    // Build the string to sign
    let string_to_sign = build_string_to_sign(&request, timestamp_header);

    // Verify HMAC signature
    if !verify_hmac_signature(secret, &string_to_sign, &provided_signature) {
        warn!("Invalid request signature");
        return (StatusCode::UNAUTHORIZED, "Invalid signature").into_response();
    }

    debug!("Request signature verified successfully");
    next.run(request).await
}

/// Middleware for IP-based access control
pub async fn ip_access_control_middleware(
    State(security): State<Arc<Security>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !security.ip_control_enabled() {
        return next.run(request).await;
    }

    let config = &security.config.ip_access;
    let client_ip = extract_client_ip(&request, addr.ip(), config.trust_proxy_headers);

    // Check blocklist first (takes precedence)
    if !config.blocklist.is_empty() && is_ip_in_list(&client_ip, &config.blocklist) {
        warn!(ip = %client_ip, "Request blocked: IP in blocklist");
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    // Check allowlist if configured
    if !config.allowlist.is_empty() && !is_ip_in_list(&client_ip, &config.allowlist) {
        warn!(ip = %client_ip, "Request blocked: IP not in allowlist");
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    debug!(ip = %client_ip, "IP access control passed");
    next.run(request).await
}

/// Validate timestamp is within tolerance
fn validate_timestamp(
    headers: &HeaderMap,
    timestamp_header: &str,
    tolerance_secs: u64,
) -> Result<(), String> {
    let timestamp_str = headers
        .get(timestamp_header)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("Missing required header: {}", timestamp_header))?;

    let timestamp: u64 = timestamp_str
        .parse()
        .map_err(|_| "Invalid timestamp format")?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System time error")?
        .as_secs();

    let diff = if now > timestamp {
        now - timestamp
    } else {
        timestamp - now
    };

    if diff > tolerance_secs {
        return Err(format!(
            "Timestamp outside tolerance window ({} seconds)",
            tolerance_secs
        ));
    }

    Ok(())
}

/// Build the string to sign from request components
fn build_string_to_sign(request: &Request<Body>, timestamp_header: &str) -> String {
    let method = request.method().as_str();
    let path = request.uri().path();
    let query = request.uri().query().unwrap_or("");

    let timestamp = request
        .headers()
        .get(timestamp_header)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Format: METHOD\nPATH\nQUERY\nTIMESTAMP
    format!("{}\n{}\n{}\n{}", method, path, query, timestamp)
}

/// Verify HMAC-SHA256 signature
fn verify_hmac_signature(secret: &str, message: &str, signature: &str) -> bool {
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };

    mac.update(message.as_bytes());

    // Support both hex and base64 encoded signatures
    let expected = if signature.starts_with("sha256=") {
        // GitHub-style: sha256=<hex>
        let hex_sig = &signature[7..];
        match hex::decode(hex_sig) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        }
    } else if signature.len() == 64 && signature.chars().all(|c| c.is_ascii_hexdigit()) {
        // Plain hex
        match hex::decode(signature) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        }
    } else {
        // Try base64
        use base64::{engine::general_purpose::STANDARD, Engine};
        match STANDARD.decode(signature) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        }
    };

    mac.verify_slice(&expected).is_ok()
}

/// Check if IP is in a list (supports CIDR notation)
fn is_ip_in_list(ip: &IpAddr, list: &[String]) -> bool {
    let ip_str = ip.to_string();

    for entry in list {
        if entry.contains('/') {
            // CIDR notation - parse and check
            if let Some(matched) = check_cidr(ip, entry) {
                if matched {
                    return true;
                }
            }
        } else if &ip_str == entry {
            return true;
        }
    }

    false
}

/// Check if IP matches CIDR notation
fn check_cidr(ip: &IpAddr, cidr: &str) -> Option<bool> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return None;
    }

    let network_ip: IpAddr = parts[0].parse().ok()?;
    let prefix_len: u8 = parts[1].parse().ok()?;

    match (ip, network_ip) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            if prefix_len > 32 {
                return None;
            }
            let mask = if prefix_len == 0 {
                0u32
            } else {
                !0u32 << (32 - prefix_len)
            };
            let ip_bits = u32::from(*ip);
            let network_bits = u32::from(network);
            Some((ip_bits & mask) == (network_bits & mask))
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            if prefix_len > 128 {
                return None;
            }
            let ip_bits = u128::from(*ip);
            let network_bits = u128::from(network);
            let mask = if prefix_len == 0 {
                0u128
            } else {
                !0u128 << (128 - prefix_len)
            };
            Some((ip_bits & mask) == (network_bits & mask))
        }
        _ => None, // Mixed IP versions
    }
}

/// Extract client IP from request headers or connection info
fn extract_client_ip(request: &Request<Body>, fallback: IpAddr, trust_proxy: bool) -> IpAddr {
    if !trust_proxy {
        return fallback;
    }

    // Check X-Forwarded-For header
    if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first_ip) = value.split(',').next() {
                if let Ok(ip) = first_ip.trim().parse() {
                    return ip;
                }
            }
        }
    }

    // Check X-Real-IP header
    if let Some(real_ip) = request.headers().get("X-Real-IP") {
        if let Ok(value) = real_ip.to_str() {
            if let Ok(ip) = value.trim().parse() {
                return ip;
            }
        }
    }

    // Check CF-Connecting-IP (Cloudflare)
    if let Some(cf_ip) = request.headers().get("CF-Connecting-IP") {
        if let Ok(value) = cf_ip.to_str() {
            if let Ok(ip) = value.trim().parse() {
                return ip;
            }
        }
    }

    fallback
}

/// Generate HMAC signature for a request (utility for clients)
pub fn generate_signature(
    secret: &str,
    method: &str,
    path: &str,
    query: &str,
    timestamp: u64,
) -> String {
    let string_to_sign = format!("{}\n{}\n{}\n{}", method, path, query, timestamp);

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(string_to_sign.as_bytes());

    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_cidr_v4() {
        let ip: IpAddr = "192.168.1.100".parse().unwrap();

        assert_eq!(check_cidr(&ip, "192.168.1.0/24"), Some(true));
        assert_eq!(check_cidr(&ip, "192.168.0.0/16"), Some(true));
        assert_eq!(check_cidr(&ip, "192.168.2.0/24"), Some(false));
        assert_eq!(check_cidr(&ip, "10.0.0.0/8"), Some(false));
    }

    #[test]
    fn test_check_cidr_v6() {
        let ip: IpAddr = "2001:db8::1".parse().unwrap();

        assert_eq!(check_cidr(&ip, "2001:db8::/32"), Some(true));
        assert_eq!(check_cidr(&ip, "2001:db9::/32"), Some(false));
    }

    #[test]
    fn test_is_ip_in_list() {
        let ip: IpAddr = "192.168.1.100".parse().unwrap();
        let list = vec!["10.0.0.1".to_string(), "192.168.1.0/24".to_string()];

        assert!(is_ip_in_list(&ip, &list));

        let ip2: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(!is_ip_in_list(&ip2, &list));
    }

    #[test]
    fn test_verify_hmac_signature() {
        let secret = "test-secret";
        let message = "GET\n/test/path\nfoo=bar\n1234567890";

        // Generate expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let expected_hex = hex::encode(mac.finalize().into_bytes());

        assert!(verify_hmac_signature(secret, message, &expected_hex));
        assert!(verify_hmac_signature(
            secret,
            message,
            &format!("sha256={}", expected_hex)
        ));
        assert!(!verify_hmac_signature(secret, message, "invalid"));
    }

    #[test]
    fn test_generate_signature() {
        let secret = "test-secret";
        let signature = generate_signature(secret, "GET", "/path", "query=1", 1234567890);

        // Verify it produces consistent results
        let signature2 = generate_signature(secret, "GET", "/path", "query=1", 1234567890);
        assert_eq!(signature, signature2);

        // Verify different inputs produce different signatures
        let signature3 = generate_signature(secret, "POST", "/path", "query=1", 1234567890);
        assert_ne!(signature, signature3);
    }
}
