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
}
