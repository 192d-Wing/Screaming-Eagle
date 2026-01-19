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

    #[serde(default)]
    pub security: SecurityConfig,

    #[serde(default)]
    pub observability: ObservabilityConfig,

    #[serde(default)]
    pub edge: EdgeConfig,
}

/// Edge logic configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    /// Enable edge processing (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// URL rewriting rules
    #[serde(default)]
    pub rewrite_rules: Vec<RewriteRuleConfig>,

    /// Header transformation settings
    #[serde(default)]
    pub header_transforms: HeaderTransformsConfig,

    /// Query string normalization settings
    #[serde(default)]
    pub query_normalization: QueryNormalizationConfig,

    /// Conditional routing rules
    #[serde(default)]
    pub routing_rules: Vec<RoutingRuleConfig>,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rewrite_rules: Vec::new(),
            header_transforms: HeaderTransformsConfig::default(),
            query_normalization: QueryNormalizationConfig::default(),
            routing_rules: Vec::new(),
        }
    }
}

/// URL rewrite rule configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRuleConfig {
    /// Rule name for logging
    pub name: String,

    /// Regex pattern to match against the URL path
    pub pattern: String,

    /// Replacement string (supports capture groups like $1, $2)
    pub replacement: String,

    /// Whether to stop processing after this rule matches
    #[serde(default)]
    pub stop: bool,

    /// Optional condition for when this rule applies
    #[serde(default)]
    pub condition: Option<RewriteConditionConfig>,
}

/// Condition for rewrite rule application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteConditionConfig {
    /// Header name to check
    pub header: Option<String>,

    /// Header value pattern (regex)
    pub header_pattern: Option<String>,

    /// Query parameter to check
    pub query_param: Option<String>,

    /// Query value pattern (regex)
    pub query_pattern: Option<String>,

    /// HTTP methods this rule applies to
    #[serde(default)]
    pub methods: Vec<String>,
}

/// Header transformation configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderTransformsConfig {
    /// Headers to add to requests going to origin
    #[serde(default)]
    pub request_add: HashMap<String, String>,

    /// Headers to remove from requests going to origin
    #[serde(default)]
    pub request_remove: Vec<String>,

    /// Headers to add to responses going to client
    #[serde(default)]
    pub response_add: HashMap<String, String>,

    /// Headers to remove from responses going to client
    #[serde(default)]
    pub response_remove: Vec<String>,

    /// Header value transformations (regex-based)
    #[serde(default)]
    pub transformations: Vec<HeaderTransformationConfig>,
}

/// A single header transformation rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderTransformationConfig {
    /// Header name to transform
    pub header: String,

    /// Pattern to match in the header value
    pub pattern: String,

    /// Replacement value
    pub replacement: String,

    /// Apply to request (true) or response (false)
    #[serde(default)]
    pub request: bool,
}

/// Query string normalization configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryNormalizationConfig {
    /// Whether to sort query parameters alphabetically
    #[serde(default = "default_true")]
    pub sort_params: bool,

    /// Whether to remove empty parameters
    #[serde(default = "default_true")]
    pub remove_empty: bool,

    /// Parameters to always remove (e.g., tracking params)
    #[serde(default = "default_tracking_params")]
    pub remove_params: Vec<String>,

    /// Parameters to keep (if set, only these are kept)
    #[serde(default)]
    pub keep_only_params: Vec<String>,

    /// Whether to lowercase parameter names
    #[serde(default)]
    pub lowercase_names: bool,

    /// Whether to decode and re-encode values for consistency
    #[serde(default = "default_true")]
    pub normalize_encoding: bool,
}

impl Default for QueryNormalizationConfig {
    fn default() -> Self {
        Self {
            sort_params: true,
            remove_empty: true,
            remove_params: default_tracking_params(),
            keep_only_params: Vec::new(),
            lowercase_names: false,
            normalize_encoding: true,
        }
    }
}

fn default_tracking_params() -> Vec<String> {
    vec![
        "utm_source".to_string(),
        "utm_medium".to_string(),
        "utm_campaign".to_string(),
        "utm_term".to_string(),
        "utm_content".to_string(),
        "fbclid".to_string(),
        "gclid".to_string(),
    ]
}

/// A conditional routing rule configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRuleConfig {
    /// Rule name for logging
    pub name: String,

    /// Conditions that must all match
    pub conditions: Vec<RoutingConditionConfig>,

    /// Action to take when conditions match
    pub action: RoutingActionConfig,

    /// Priority (higher = checked first)
    #[serde(default)]
    pub priority: i32,
}

/// A routing condition configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RoutingConditionConfig {
    /// Match path pattern
    #[serde(rename = "path")]
    Path { pattern: String },

    /// Match header value
    #[serde(rename = "header")]
    Header { name: String, pattern: String },

    /// Match query parameter
    #[serde(rename = "query")]
    Query { param: String, pattern: String },

    /// Match HTTP method
    #[serde(rename = "method")]
    Method { methods: Vec<String> },

    /// Match client IP (CIDR)
    #[serde(rename = "ip")]
    ClientIp { cidrs: Vec<String> },

    /// Geographic location (country codes)
    #[serde(rename = "geo")]
    Geo { countries: Vec<String> },

    /// Time-based condition
    #[serde(rename = "time")]
    Time {
        /// Days of week (0=Sunday, 6=Saturday)
        days: Option<Vec<u8>>,
        /// Start hour (0-23)
        start_hour: Option<u8>,
        /// End hour (0-23)
        end_hour: Option<u8>,
    },
}

/// Action to take when routing conditions match
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RoutingActionConfig {
    /// Route to a specific origin
    #[serde(rename = "origin")]
    RouteToOrigin { origin: String },

    /// Redirect to a URL
    #[serde(rename = "redirect")]
    Redirect { url: String, status: u16 },

    /// Return a fixed response
    #[serde(rename = "response")]
    FixedResponse {
        status: u16,
        body: Option<String>,
        headers: Option<HashMap<String, String>>,
    },

    /// Modify the request and continue
    #[serde(rename = "modify")]
    Modify {
        set_headers: Option<HashMap<String, String>>,
        set_path: Option<String>,
    },

    /// Block the request
    #[serde(rename = "block")]
    Block {
        status: u16,
        message: Option<String>,
    },
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

/// Security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Security headers configuration
    #[serde(default)]
    pub headers: SecurityHeadersConfig,

    /// Request signing configuration
    #[serde(default)]
    pub signing: RequestSigningConfig,

    /// IP-based access control
    #[serde(default)]
    pub ip_access: IpAccessConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            headers: SecurityHeadersConfig::default(),
            signing: RequestSigningConfig::default(),
            ip_access: IpAccessConfig::default(),
        }
    }
}

/// Security headers configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityHeadersConfig {
    /// Enable security headers (default: true)
    #[serde(default = "default_security_headers_enabled")]
    pub enabled: bool,

    /// Content-Security-Policy header value
    #[serde(default = "default_csp")]
    pub content_security_policy: Option<String>,

    /// X-Frame-Options header value (e.g., "DENY", "SAMEORIGIN")
    #[serde(default = "default_x_frame_options")]
    pub x_frame_options: Option<String>,

    /// Enable X-Content-Type-Options: nosniff (default: true)
    #[serde(default = "default_true")]
    pub x_content_type_options: bool,

    /// X-XSS-Protection header value
    #[serde(default = "default_x_xss_protection")]
    pub x_xss_protection: Option<String>,

    /// Strict-Transport-Security header value
    #[serde(default)]
    pub strict_transport_security: Option<String>,

    /// Referrer-Policy header value
    #[serde(default = "default_referrer_policy")]
    pub referrer_policy: Option<String>,

    /// Permissions-Policy header value
    #[serde(default)]
    pub permissions_policy: Option<String>,

    /// Cross-Origin-Embedder-Policy header value
    #[serde(default)]
    pub cross_origin_embedder_policy: Option<String>,

    /// Cross-Origin-Opener-Policy header value
    #[serde(default)]
    pub cross_origin_opener_policy: Option<String>,

    /// Cross-Origin-Resource-Policy header value
    #[serde(default)]
    pub cross_origin_resource_policy: Option<String>,

    /// Remove Server header from responses (default: true)
    #[serde(default = "default_true")]
    pub remove_server_header: bool,
}

impl Default for SecurityHeadersConfig {
    fn default() -> Self {
        Self {
            enabled: default_security_headers_enabled(),
            content_security_policy: default_csp(),
            x_frame_options: default_x_frame_options(),
            x_content_type_options: true,
            x_xss_protection: default_x_xss_protection(),
            strict_transport_security: None,
            referrer_policy: default_referrer_policy(),
            permissions_policy: None,
            cross_origin_embedder_policy: None,
            cross_origin_opener_policy: None,
            cross_origin_resource_policy: None,
            remove_server_header: true,
        }
    }
}

fn default_security_headers_enabled() -> bool {
    true
}

fn default_csp() -> Option<String> {
    Some("default-src 'self'".to_string())
}

fn default_x_frame_options() -> Option<String> {
    Some("DENY".to_string())
}

fn default_x_xss_protection() -> Option<String> {
    Some("1; mode=block".to_string())
}

fn default_referrer_policy() -> Option<String> {
    Some("strict-origin-when-cross-origin".to_string())
}

fn default_true() -> bool {
    true
}

/// Request signing configuration for HMAC validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSigningConfig {
    /// Enable request signing validation (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Secret key for HMAC validation
    #[serde(default)]
    pub secret_key: Option<String>,

    /// Header name for signature (default: "X-Signature-256")
    #[serde(default = "default_signature_header")]
    pub signature_header: Option<String>,

    /// Header name for timestamp (default: "X-Timestamp")
    #[serde(default = "default_timestamp_header")]
    pub timestamp_header: Option<String>,

    /// Require signature on all requests (default: false)
    #[serde(default)]
    pub require_signature: bool,

    /// Require timestamp for replay protection (default: false)
    #[serde(default)]
    pub require_timestamp: bool,

    /// Timestamp tolerance in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_timestamp_tolerance")]
    pub timestamp_tolerance_secs: u64,
}

impl Default for RequestSigningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret_key: None,
            signature_header: default_signature_header(),
            timestamp_header: default_timestamp_header(),
            require_signature: false,
            require_timestamp: false,
            timestamp_tolerance_secs: default_timestamp_tolerance(),
        }
    }
}

fn default_signature_header() -> Option<String> {
    Some("X-Signature-256".to_string())
}

fn default_timestamp_header() -> Option<String> {
    Some("X-Timestamp".to_string())
}

fn default_timestamp_tolerance() -> u64 {
    300 // 5 minutes
}

/// IP-based access control configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpAccessConfig {
    /// Enable IP-based access control (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// IP allowlist (empty = all allowed unless blocklisted)
    /// Supports CIDR notation (e.g., "192.168.1.0/24")
    #[serde(default)]
    pub allowlist: Vec<String>,

    /// IP blocklist (takes precedence over allowlist)
    /// Supports CIDR notation (e.g., "10.0.0.0/8")
    #[serde(default)]
    pub blocklist: Vec<String>,

    /// Trust X-Forwarded-For and similar headers (default: false)
    /// Only enable if behind a trusted reverse proxy
    #[serde(default)]
    pub trust_proxy_headers: bool,
}

impl Default for IpAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowlist: Vec::new(),
            blocklist: Vec::new(),
            trust_proxy_headers: false,
        }
    }
}

/// Observability configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Tracing configuration
    #[serde(default)]
    pub tracing: TracingConfig,

    /// Metrics configuration
    #[serde(default)]
    pub metrics: MetricsConfig,

    /// Logging configuration
    #[serde(default)]
    pub request_logging: RequestLoggingConfig,

    /// Alerting configuration
    #[serde(default)]
    pub alerting: AlertingConfig,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            tracing: TracingConfig::default(),
            metrics: MetricsConfig::default(),
            request_logging: RequestLoggingConfig::default(),
            alerting: AlertingConfig::default(),
        }
    }
}

/// OpenTelemetry tracing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Enable OpenTelemetry tracing (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// OTLP endpoint (default: http://localhost:4317)
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: Option<String>,

    /// Service name for traces
    #[serde(default = "default_service_name")]
    pub service_name: String,

    /// Sample rate (0.0 to 1.0, default: 1.0)
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,

    /// Propagate trace context from incoming requests
    #[serde(default = "default_true")]
    pub propagate_context: bool,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
            sample_rate: default_sample_rate(),
            propagate_context: true,
        }
    }
}

fn default_otlp_endpoint() -> Option<String> {
    Some("http://localhost:4317".to_string())
}

fn default_service_name() -> String {
    "screaming-eagle-cdn".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

/// Enhanced metrics configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Enable enhanced metrics (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum number of paths to track individually
    #[serde(default = "default_max_tracked_paths")]
    pub max_tracked_paths: usize,

    /// Enable per-path metrics (can increase cardinality)
    #[serde(default = "default_true")]
    pub per_path_metrics: bool,

    /// Include histogram buckets for latency
    #[serde(default = "default_true")]
    pub latency_histograms: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tracked_paths: default_max_tracked_paths(),
            per_path_metrics: true,
            latency_histograms: true,
        }
    }
}

fn default_max_tracked_paths() -> usize {
    1000
}

/// Request logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLoggingConfig {
    /// Enable structured request logging (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Log level for successful requests
    #[serde(default = "default_success_log_level")]
    pub success_log_level: String,

    /// Log level for error requests
    #[serde(default = "default_error_log_level")]
    pub error_log_level: String,

    /// Include request headers in logs
    #[serde(default)]
    pub log_headers: bool,

    /// Include response headers in logs
    #[serde(default)]
    pub log_response_headers: bool,

    /// Headers to redact from logs
    #[serde(default = "default_redacted_headers")]
    pub redacted_headers: Vec<String>,
}

impl Default for RequestLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            success_log_level: default_success_log_level(),
            error_log_level: default_error_log_level(),
            log_headers: false,
            log_response_headers: false,
            redacted_headers: default_redacted_headers(),
        }
    }
}

fn default_success_log_level() -> String {
    "debug".to_string()
}

fn default_error_log_level() -> String {
    "error".to_string()
}

fn default_redacted_headers() -> Vec<String> {
    vec![
        "authorization".to_string(),
        "cookie".to_string(),
        "x-api-key".to_string(),
    ]
}

/// Alerting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertingConfig {
    /// Enable alerting (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Error rate threshold percentage
    #[serde(default = "default_error_rate_threshold")]
    pub error_rate_threshold: f64,

    /// P99 latency threshold in milliseconds
    #[serde(default = "default_latency_threshold")]
    pub latency_p99_threshold_ms: u64,

    /// Minimum cache hit ratio
    #[serde(default = "default_cache_hit_ratio_min")]
    pub cache_hit_ratio_min: f64,

    /// Origin error rate threshold
    #[serde(default = "default_origin_error_rate")]
    pub origin_error_rate_threshold: f64,
}

impl Default for AlertingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            error_rate_threshold: default_error_rate_threshold(),
            latency_p99_threshold_ms: default_latency_threshold(),
            cache_hit_ratio_min: default_cache_hit_ratio_min(),
            origin_error_rate_threshold: default_origin_error_rate(),
        }
    }
}

fn default_error_rate_threshold() -> f64 {
    5.0
}

fn default_latency_threshold() -> u64 {
    1000
}

fn default_cache_hit_ratio_min() -> f64 {
    0.7
}

fn default_origin_error_rate() -> f64 {
    10.0
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
            security: SecurityConfig::default(),
            observability: ObservabilityConfig::default(),
            edge: EdgeConfig::default(),
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
