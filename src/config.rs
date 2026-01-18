use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use crate::error::{CdnError, CdnResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server: ServerConfig,

    #[serde(default)]
    pub cache: CacheConfig,

    #[serde(default)]
    pub origins: HashMap<String, OriginConfig>,

    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    #[serde(default)]
    pub tls: Option<TlsConfig>,

    #[serde(default)]
    pub admin: AdminConfig,

    #[serde(default)]
    pub coalesce: CoalesceConfig,

    #[serde(default)]
    pub error_pages: ErrorPagesConfig,

    #[serde(default)]
    pub connection_pool: ConnectionPoolConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_workers")]
    pub workers: usize,

    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default = "default_max_size")]
    pub max_size_mb: usize,

    #[serde(default = "default_max_entry_size")]
    pub max_entry_size_mb: usize,

    #[serde(default = "default_ttl")]
    pub default_ttl_secs: u64,

    #[serde(default = "default_max_ttl")]
    pub max_ttl_secs: u64,

    #[serde(default = "default_stale_while_revalidate")]
    pub stale_while_revalidate_secs: u64,

    #[serde(default)]
    pub respect_cache_control: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginConfig {
    pub url: String,

    #[serde(default)]
    pub host_header: Option<String>,

    #[serde(default = "default_origin_timeout")]
    pub timeout_secs: u64,

    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Health check path (e.g., "/health" or "/_health")
    #[serde(default)]
    pub health_check_path: Option<String>,

    /// Health check interval in seconds (default: 30)
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_secs: u64,

    /// Health check timeout in seconds (default: 5)
    #[serde(default = "default_health_check_timeout")]
    pub health_check_timeout_secs: u64,
}

/// Connection pool configuration for origin connections
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPoolConfig {
    /// Maximum idle connections per host (default: 100)
    #[serde(default = "default_pool_max_idle_per_host")]
    pub max_idle_per_host: usize,

    /// Idle connection timeout in seconds (default: 90)
    #[serde(default = "default_pool_idle_timeout")]
    pub idle_timeout_secs: u64,

    /// Connection timeout in seconds (default: 10)
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// Enable TCP keepalive (default: true)
    #[serde(default = "default_tcp_keepalive")]
    pub tcp_keepalive: bool,

    /// TCP keepalive interval in seconds (default: 60)
    #[serde(default = "default_tcp_keepalive_interval")]
    pub tcp_keepalive_interval_secs: u64,

    /// Enable TCP nodelay (default: true)
    #[serde(default = "default_tcp_nodelay")]
    pub tcp_nodelay: bool,

    /// Enable HTTP/2 (default: true)
    #[serde(default = "default_http2_enabled")]
    pub http2_enabled: bool,

    /// HTTP/2 initial stream window size (default: 65535)
    #[serde(default = "default_http2_initial_stream_window")]
    pub http2_initial_stream_window_size: u32,

    /// HTTP/2 initial connection window size (default: 65535)
    #[serde(default = "default_http2_initial_connection_window")]
    pub http2_initial_connection_window_size: u32,
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: default_pool_max_idle_per_host(),
            idle_timeout_secs: default_pool_idle_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            tcp_keepalive: default_tcp_keepalive(),
            tcp_keepalive_interval_secs: default_tcp_keepalive_interval(),
            tcp_nodelay: default_tcp_nodelay(),
            http2_enabled: default_http2_enabled(),
            http2_initial_stream_window_size: default_http2_initial_stream_window(),
            http2_initial_connection_window_size: default_http2_initial_connection_window(),
        }
    }
}

fn default_pool_max_idle_per_host() -> usize {
    100
}

fn default_pool_idle_timeout() -> u64 {
    90
}

fn default_connect_timeout() -> u64 {
    10
}

fn default_tcp_keepalive() -> bool {
    true
}

fn default_tcp_keepalive_interval() -> u64 {
    60
}

fn default_tcp_nodelay() -> bool {
    true
}

fn default_http2_enabled() -> bool {
    true
}

fn default_http2_initial_stream_window() -> u32 {
    65535 * 16 // 1MB - better for large files
}

fn default_http2_initial_connection_window() -> u32 {
    65535 * 16 // 1MB
}

fn default_health_check_interval() -> u64 {
    30
}

fn default_health_check_timeout() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,

    #[serde(default)]
    pub json_format: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default = "default_rate_limit_enabled")]
    pub enabled: bool,

    #[serde(default = "default_requests_per_window")]
    pub requests_per_window: u32,

    #[serde(default = "default_window_secs")]
    pub window_secs: u64,

    #[serde(default = "default_burst_size")]
    pub burst_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,

    #[serde(default = "default_reset_timeout")]
    pub reset_timeout_secs: u64,

    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,

    #[serde(default = "default_failure_window")]
    pub failure_window_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    /// Enable authentication for admin endpoints
    #[serde(default)]
    pub auth_enabled: bool,

    /// Bearer token for admin API authentication
    #[serde(default)]
    pub auth_token: Option<String>,

    /// Allowed IP addresses for admin endpoints (empty = all allowed)
    #[serde(default)]
    pub allowed_ips: Vec<String>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            auth_enabled: false,
            auth_token: None,
            allowed_ips: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoalesceConfig {
    /// Enable request coalescing (default: true)
    #[serde(default = "default_coalesce_enabled")]
    pub enabled: bool,

    /// Maximum number of requests that can wait for a single in-flight request
    #[serde(default = "default_max_waiters")]
    pub max_waiters: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPagesConfig {
    /// Enable custom error pages (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Directory containing custom error page templates
    #[serde(default = "default_error_pages_dir")]
    pub directory: String,

    /// Custom error page for 400 Bad Request
    #[serde(default)]
    pub page_400: Option<String>,

    /// Custom error page for 404 Not Found
    #[serde(default)]
    pub page_404: Option<String>,

    /// Custom error page for 500 Internal Server Error
    #[serde(default)]
    pub page_500: Option<String>,

    /// Custom error page for 502 Bad Gateway
    #[serde(default)]
    pub page_502: Option<String>,

    /// Custom error page for 503 Service Unavailable
    #[serde(default)]
    pub page_503: Option<String>,

    /// Custom error page for 504 Gateway Timeout
    #[serde(default)]
    pub page_504: Option<String>,
}

impl Default for ErrorPagesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: default_error_pages_dir(),
            page_400: None,
            page_404: None,
            page_500: None,
            page_502: None,
            page_503: None,
            page_504: None,
        }
    }
}

fn default_error_pages_dir() -> String {
    "error_pages".to_string()
}

impl Default for CoalesceConfig {
    fn default() -> Self {
        Self {
            enabled: default_coalesce_enabled(),
            max_waiters: default_max_waiters(),
        }
    }
}

fn default_coalesce_enabled() -> bool {
    true
}

fn default_max_waiters() -> usize {
    1000
}

// Default value functions
fn default_server() -> ServerConfig {
    ServerConfig {
        host: default_host(),
        port: default_port(),
        workers: default_workers(),
        request_timeout_secs: default_request_timeout(),
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_workers() -> usize {
    num_cpus::get().max(1)
}

fn default_request_timeout() -> u64 {
    30
}

fn default_max_size() -> usize {
    1024 // 1GB default
}

fn default_max_entry_size() -> usize {
    100 // 100MB default per entry
}

fn default_ttl() -> u64 {
    3600 // 1 hour
}

fn default_max_ttl() -> u64 {
    86400 // 24 hours
}

fn default_stale_while_revalidate() -> u64 {
    60 // 1 minute
}

fn default_origin_timeout() -> u64 {
    30
}

fn default_max_retries() -> u32 {
    3
}

fn default_log_level() -> String {
    "info".to_string()
}

// Rate limit defaults
fn default_rate_limit_enabled() -> bool {
    true
}

fn default_requests_per_window() -> u32 {
    1000
}

fn default_window_secs() -> u64 {
    60
}

fn default_burst_size() -> u32 {
    50
}

// Circuit breaker defaults
fn default_failure_threshold() -> u32 {
    5
}

fn default_reset_timeout() -> u64 {
    30
}

fn default_success_threshold() -> u32 {
    3
}

fn default_failure_window() -> u64 {
    60
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: default_server(),
            cache: CacheConfig::default(),
            origins: HashMap::new(),
            logging: LoggingConfig::default(),
            rate_limit: RateLimitConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            tls: None,
            admin: AdminConfig::default(),
            coalesce: CoalesceConfig::default(),
            error_pages: ErrorPagesConfig::default(),
            connection_pool: ConnectionPoolConfig::default(),
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_size_mb: default_max_size(),
            max_entry_size_mb: default_max_entry_size(),
            default_ttl_secs: default_ttl(),
            max_ttl_secs: default_max_ttl(),
            stale_while_revalidate_secs: default_stale_while_revalidate(),
            respect_cache_control: true,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            json_format: false,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: default_rate_limit_enabled(),
            requests_per_window: default_requests_per_window(),
            window_secs: default_window_secs(),
            burst_size: default_burst_size(),
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: default_failure_threshold(),
            reset_timeout_secs: default_reset_timeout(),
            success_threshold: default_success_threshold(),
            failure_window_secs: default_failure_window(),
        }
    }
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> CdnResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CdnError::ConfigError(format!("Failed to read config file: {}", e)))?;

        toml::from_str(&content)
            .map_err(|e| CdnError::ConfigError(format!("Failed to parse config: {}", e)))
    }

    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.server.request_timeout_secs)
    }
}

impl CacheConfig {
    pub fn default_ttl(&self) -> Duration {
        Duration::from_secs(self.default_ttl_secs)
    }

    pub fn max_ttl(&self) -> Duration {
        Duration::from_secs(self.max_ttl_secs)
    }

    pub fn max_size_bytes(&self) -> usize {
        self.max_size_mb * 1024 * 1024
    }

    pub fn max_entry_size_bytes(&self) -> usize {
        self.max_entry_size_mb * 1024 * 1024
    }
}

impl OriginConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    pub fn health_check_timeout(&self) -> Duration {
        Duration::from_secs(self.health_check_timeout_secs)
    }

    pub fn health_check_interval(&self) -> Duration {
        Duration::from_secs(self.health_check_interval_secs)
    }
}
