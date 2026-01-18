//! Admin API authentication module
//!
//! Provides middleware for authenticating admin API requests using bearer tokens
//! and optional IP-based access control.

use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::config::AdminConfig;

/// Admin authentication state
#[derive(Clone)]
pub struct AdminAuth {
    config: AdminConfig,
}

impl AdminAuth {
    pub fn new(config: AdminConfig) -> Self {
        Self { config }
    }

    /// Check if authentication is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.auth_enabled
    }

    /// Verify the bearer token
    pub fn verify_token(&self, token: &str) -> bool {
        match &self.config.auth_token {
            Some(expected) => {
                // Constant-time comparison to prevent timing attacks
                constant_time_compare(token, expected)
            }
            None => {
                // No token configured but auth is enabled - deny access
                false
            }
        }
    }

    /// Check if IP is allowed
    pub fn is_ip_allowed(&self, ip: &IpAddr) -> bool {
        if self.config.allowed_ips.is_empty() {
            // Empty list means all IPs are allowed
            return true;
        }

        let ip_str = ip.to_string();
        self.config.allowed_ips.iter().any(|allowed| {
            if allowed.contains('/') {
                // CIDR notation - simple prefix match for now
                // For production, use a proper CIDR parser
                ip_str.starts_with(allowed.split('/').next().unwrap_or(""))
            } else {
                &ip_str == allowed
            }
        })
    }
}

/// Constant-time string comparison to prevent timing attacks
fn constant_time_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

/// Middleware for admin API authentication
pub async fn admin_auth_middleware(
    State(auth): State<Arc<AdminAuth>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Check if auth is enabled
    if !auth.is_enabled() {
        debug!("Admin auth disabled, allowing request");
        return next.run(request).await;
    }

    let client_ip = extract_client_ip(&request, addr.ip());

    // Check IP allowlist
    if !auth.is_ip_allowed(&client_ip) {
        warn!(ip = %client_ip, "Admin request from non-allowed IP");
        return (
            StatusCode::FORBIDDEN,
            "Access denied: IP not in allowlist",
        )
            .into_response();
    }

    // Check Authorization header
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..]; // Skip "Bearer "
            if auth.verify_token(token) {
                debug!(ip = %client_ip, "Admin auth successful");
                next.run(request).await
            } else {
                warn!(ip = %client_ip, "Invalid admin token");
                (StatusCode::UNAUTHORIZED, "Invalid authentication token").into_response()
            }
        }
        Some(_) => {
            warn!(ip = %client_ip, "Invalid Authorization header format");
            (
                StatusCode::UNAUTHORIZED,
                "Authorization header must use Bearer scheme",
            )
                .into_response()
        }
        None => {
            warn!(ip = %client_ip, "Missing Authorization header for admin endpoint");
            (
                StatusCode::UNAUTHORIZED,
                "Authentication required. Use: Authorization: Bearer <token>",
            )
                .into_response()
        }
    }
}

/// Extract client IP from request headers or connection info
fn extract_client_ip(request: &Request<Body>, fallback: IpAddr) -> IpAddr {
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

    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare("hello", "hello"));
        assert!(!constant_time_compare("hello", "world"));
        assert!(!constant_time_compare("hello", "hell"));
        assert!(!constant_time_compare("hello", "helloo"));
    }

    #[test]
    fn test_admin_auth_disabled() {
        let auth = AdminAuth::new(AdminConfig {
            auth_enabled: false,
            auth_token: None,
            allowed_ips: vec![],
        });
        assert!(!auth.is_enabled());
    }

    #[test]
    fn test_admin_auth_verify_token() {
        let auth = AdminAuth::new(AdminConfig {
            auth_enabled: true,
            auth_token: Some("secret123".to_string()),
            allowed_ips: vec![],
        });

        assert!(auth.verify_token("secret123"));
        assert!(!auth.verify_token("wrong"));
        assert!(!auth.verify_token("secret12"));
        assert!(!auth.verify_token("secret1234"));
    }

    #[test]
    fn test_admin_auth_ip_allowlist() {
        let auth = AdminAuth::new(AdminConfig {
            auth_enabled: true,
            auth_token: Some("secret".to_string()),
            allowed_ips: vec!["127.0.0.1".to_string(), "192.168.1.1".to_string()],
        });

        assert!(auth.is_ip_allowed(&"127.0.0.1".parse().unwrap()));
        assert!(auth.is_ip_allowed(&"192.168.1.1".parse().unwrap()));
        assert!(!auth.is_ip_allowed(&"10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn test_admin_auth_empty_allowlist() {
        let auth = AdminAuth::new(AdminConfig {
            auth_enabled: true,
            auth_token: Some("secret".to_string()),
            allowed_ips: vec![],
        });

        // Empty allowlist means all IPs allowed
        assert!(auth.is_ip_allowed(&"127.0.0.1".parse().unwrap()));
        assert!(auth.is_ip_allowed(&"10.0.0.1".parse().unwrap()));
    }
}
