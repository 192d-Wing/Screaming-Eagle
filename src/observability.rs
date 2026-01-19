//! Observability module for Screaming Eagle CDN
//!
//! Provides distributed tracing, structured request logging,
//! detailed metrics, and alerting thresholds.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, Request, Response, StatusCode},
    middleware::Next,
};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    runtime,
    trace::{self, RandomIdGenerator, Sampler},
    Resource,
};
use prometheus::{CounterVec, GaugeVec, HistogramOpts, HistogramVec, Opts, Registry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, error, info, info_span, warn, Instrument};
use uuid::Uuid;

use crate::cache::CacheStatus;
use crate::config::ObservabilityConfig;

/// Request context for tracking through the request lifecycle
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub start_time: Instant,
    pub origin: Option<String>,
    pub path: String,
    pub method: String,
    pub client_ip: Option<String>,
}

impl RequestContext {
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            trace_id: None,
            span_id: None,
            start_time: Instant::now(),
            origin: None,
            path: path.to_string(),
            method: method.to_string(),
            client_ip: None,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }
}

/// Structured log entry for requests
#[derive(Debug, Serialize)]
pub struct RequestLogEntry {
    pub timestamp: String,
    pub request_id: String,
    pub trace_id: Option<String>,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub origin: Option<String>,
    pub status: u16,
    pub cache_status: String,
    pub duration_ms: f64,
    pub bytes_sent: u64,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub country: Option<String>,
}

/// Enhanced metrics with per-path and detailed tracking
pub struct EnhancedMetrics {
    registry: Registry,

    // Request metrics
    requests_total: CounterVec,
    requests_by_path: CounterVec,
    request_duration: HistogramVec,
    request_duration_by_path: HistogramVec,

    // Cache metrics
    cache_operations: CounterVec,
    cache_size_bytes: GaugeVec,

    // Origin metrics
    origin_requests: CounterVec,
    origin_latency: HistogramVec,
    origin_errors: CounterVec,

    // Bandwidth metrics
    bytes_sent: CounterVec,
    bytes_received: CounterVec,

    // Error metrics
    errors_by_type: CounterVec,

    // Rate limiting metrics
    rate_limited_requests: CounterVec,

    // Circuit breaker metrics
    circuit_breaker_state: GaugeVec,
    circuit_breaker_trips: CounterVec,

    // Connection metrics
    active_connections: GaugeVec,
    connection_duration: HistogramVec,

    // Path-level tracking (for top paths)
    path_stats: Arc<RwLock<HashMap<String, PathStats>>>,
    max_tracked_paths: usize,
}

#[derive(Debug, Default)]
pub struct PathStats {
    pub requests: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub errors: AtomicU64,
    pub total_duration_ms: AtomicU64,
    pub bytes_sent: AtomicU64,
}

impl EnhancedMetrics {
    pub fn new(config: &ObservabilityConfig) -> Self {
        let registry = Registry::new();

        // Request metrics
        let requests_total = CounterVec::new(
            Opts::new("cdn_requests_total", "Total CDN requests")
                .namespace("screaming_eagle"),
            &["origin", "method", "status", "cache_status"],
        )
        .unwrap();

        let requests_by_path = CounterVec::new(
            Opts::new("cdn_requests_by_path_total", "Requests by path prefix")
                .namespace("screaming_eagle"),
            &["origin", "path_prefix", "status"],
        )
        .unwrap();

        let request_duration = HistogramVec::new(
            HistogramOpts::new("cdn_request_duration_seconds", "Request duration")
                .namespace("screaming_eagle")
                .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["origin", "cache_status"],
        )
        .unwrap();

        let request_duration_by_path = HistogramVec::new(
            HistogramOpts::new("cdn_request_duration_by_path_seconds", "Request duration by path")
                .namespace("screaming_eagle")
                .buckets(vec![0.01, 0.05, 0.1, 0.5, 1.0, 5.0]),
            &["origin", "path_prefix"],
        )
        .unwrap();

        // Cache metrics
        let cache_operations = CounterVec::new(
            Opts::new("cdn_cache_operations_total", "Cache operations")
                .namespace("screaming_eagle"),
            &["operation", "result"],
        )
        .unwrap();

        let cache_size_bytes = GaugeVec::new(
            Opts::new("cdn_cache_size_bytes", "Current cache size in bytes")
                .namespace("screaming_eagle"),
            &["type"],
        )
        .unwrap();

        // Origin metrics
        let origin_requests = CounterVec::new(
            Opts::new("cdn_origin_requests_total", "Origin server requests")
                .namespace("screaming_eagle"),
            &["origin", "status"],
        )
        .unwrap();

        let origin_latency = HistogramVec::new(
            HistogramOpts::new("cdn_origin_latency_seconds", "Origin response latency")
                .namespace("screaming_eagle")
                .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["origin"],
        )
        .unwrap();

        let origin_errors = CounterVec::new(
            Opts::new("cdn_origin_errors_total", "Origin server errors")
                .namespace("screaming_eagle"),
            &["origin", "error_type"],
        )
        .unwrap();

        // Bandwidth metrics
        let bytes_sent = CounterVec::new(
            Opts::new("cdn_bytes_sent_total", "Total bytes sent to clients")
                .namespace("screaming_eagle"),
            &["origin", "cache_status"],
        )
        .unwrap();

        let bytes_received = CounterVec::new(
            Opts::new("cdn_bytes_received_total", "Total bytes received from origins")
                .namespace("screaming_eagle"),
            &["origin"],
        )
        .unwrap();

        // Error metrics
        let errors_by_type = CounterVec::new(
            Opts::new("cdn_errors_total", "Errors by type")
                .namespace("screaming_eagle"),
            &["error_type", "origin"],
        )
        .unwrap();

        // Rate limiting metrics
        let rate_limited_requests = CounterVec::new(
            Opts::new("cdn_rate_limited_total", "Rate limited requests")
                .namespace("screaming_eagle"),
            &["origin"],
        )
        .unwrap();

        // Circuit breaker metrics
        let circuit_breaker_state = GaugeVec::new(
            Opts::new("cdn_circuit_breaker_state", "Circuit breaker state (0=closed, 1=half-open, 2=open)")
                .namespace("screaming_eagle"),
            &["origin"],
        )
        .unwrap();

        let circuit_breaker_trips = CounterVec::new(
            Opts::new("cdn_circuit_breaker_trips_total", "Circuit breaker trips")
                .namespace("screaming_eagle"),
            &["origin"],
        )
        .unwrap();

        // Connection metrics
        let active_connections = GaugeVec::new(
            Opts::new("cdn_active_connections", "Active connections")
                .namespace("screaming_eagle"),
            &["type"],
        )
        .unwrap();

        let connection_duration = HistogramVec::new(
            HistogramOpts::new("cdn_connection_duration_seconds", "Connection duration")
                .namespace("screaming_eagle")
                .buckets(vec![0.1, 1.0, 5.0, 30.0, 60.0, 300.0]),
            &["type"],
        )
        .unwrap();

        // Register all metrics
        let metrics_to_register: Vec<Box<dyn prometheus::core::Collector>> = vec![
            Box::new(requests_total.clone()),
            Box::new(requests_by_path.clone()),
            Box::new(request_duration.clone()),
            Box::new(request_duration_by_path.clone()),
            Box::new(cache_operations.clone()),
            Box::new(cache_size_bytes.clone()),
            Box::new(origin_requests.clone()),
            Box::new(origin_latency.clone()),
            Box::new(origin_errors.clone()),
            Box::new(bytes_sent.clone()),
            Box::new(bytes_received.clone()),
            Box::new(errors_by_type.clone()),
            Box::new(rate_limited_requests.clone()),
            Box::new(circuit_breaker_state.clone()),
            Box::new(circuit_breaker_trips.clone()),
            Box::new(active_connections.clone()),
            Box::new(connection_duration.clone()),
        ];

        for metric in metrics_to_register {
            if let Err(e) = registry.register(metric) {
                warn!("Failed to register metric: {}", e);
            }
        }

        Self {
            registry,
            requests_total,
            requests_by_path,
            request_duration,
            request_duration_by_path,
            cache_operations,
            cache_size_bytes,
            origin_requests,
            origin_latency,
            origin_errors,
            bytes_sent,
            bytes_received,
            errors_by_type,
            rate_limited_requests,
            circuit_breaker_state,
            circuit_breaker_trips,
            active_connections,
            connection_duration,
            path_stats: Arc::new(RwLock::new(HashMap::new())),
            max_tracked_paths: config.metrics.max_tracked_paths,
        }
    }

    /// Record a complete request
    pub async fn record_request(
        &self,
        origin: &str,
        method: &str,
        path: &str,
        status: StatusCode,
        cache_status: CacheStatus,
        duration: Duration,
        bytes: u64,
    ) {
        let status_str = status.as_u16().to_string();
        let cache_str = cache_status.as_str();
        let path_prefix = extract_path_prefix(path);

        // Core request metrics
        self.requests_total
            .with_label_values(&[origin, method, &status_str, cache_str])
            .inc();

        self.requests_by_path
            .with_label_values(&[origin, &path_prefix, &status_str])
            .inc();

        self.request_duration
            .with_label_values(&[origin, cache_str])
            .observe(duration.as_secs_f64());

        self.request_duration_by_path
            .with_label_values(&[origin, &path_prefix])
            .observe(duration.as_secs_f64());

        self.bytes_sent
            .with_label_values(&[origin, cache_str])
            .inc_by(bytes as f64);

        // Cache operation tracking
        match cache_status {
            CacheStatus::Hit | CacheStatus::Stale | CacheStatus::StaleIfError => {
                self.cache_operations
                    .with_label_values(&["get", "hit"])
                    .inc();
            }
            CacheStatus::Miss => {
                self.cache_operations
                    .with_label_values(&["get", "miss"])
                    .inc();
            }
            CacheStatus::Bypass => {
                self.cache_operations
                    .with_label_values(&["get", "bypass"])
                    .inc();
            }
        }

        // Update path stats
        self.update_path_stats(path, cache_status, status.is_server_error(), duration, bytes)
            .await;
    }

    /// Record an origin request
    pub fn record_origin_request(&self, origin: &str, status: StatusCode, duration: Duration, bytes: u64) {
        self.origin_requests
            .with_label_values(&[origin, &status.as_u16().to_string()])
            .inc();

        self.origin_latency
            .with_label_values(&[origin])
            .observe(duration.as_secs_f64());

        self.bytes_received
            .with_label_values(&[origin])
            .inc_by(bytes as f64);
    }

    /// Record an origin error
    pub fn record_origin_error(&self, origin: &str, error_type: &str) {
        self.origin_errors
            .with_label_values(&[origin, error_type])
            .inc();

        self.errors_by_type
            .with_label_values(&[error_type, origin])
            .inc();
    }

    /// Record a rate limited request
    pub fn record_rate_limited(&self, origin: &str) {
        self.rate_limited_requests
            .with_label_values(&[origin])
            .inc();
    }

    /// Update circuit breaker state
    pub fn set_circuit_breaker_state(&self, origin: &str, state: u8) {
        self.circuit_breaker_state
            .with_label_values(&[origin])
            .set(state as f64);
    }

    /// Record circuit breaker trip
    pub fn record_circuit_breaker_trip(&self, origin: &str) {
        self.circuit_breaker_trips
            .with_label_values(&[origin])
            .inc();
    }

    /// Update cache size metric
    pub fn set_cache_size(&self, size_bytes: usize) {
        self.cache_size_bytes
            .with_label_values(&["total"])
            .set(size_bytes as f64);
    }

    /// Set active connections count
    pub fn set_active_connections(&self, count: usize) {
        self.active_connections
            .with_label_values(&["http"])
            .set(count as f64);
    }

    /// Update path-level stats
    async fn update_path_stats(
        &self,
        path: &str,
        cache_status: CacheStatus,
        is_error: bool,
        duration: Duration,
        bytes: u64,
    ) {
        let path_key = extract_path_prefix(path);

        let mut stats = self.path_stats.write().await;

        // Limit the number of tracked paths
        if !stats.contains_key(&path_key) && stats.len() >= self.max_tracked_paths {
            return;
        }

        let entry = stats.entry(path_key).or_insert_with(PathStats::default);

        entry.requests.fetch_add(1, Ordering::Relaxed);
        entry
            .total_duration_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
        entry.bytes_sent.fetch_add(bytes, Ordering::Relaxed);

        match cache_status {
            CacheStatus::Hit | CacheStatus::Stale | CacheStatus::StaleIfError => {
                entry.cache_hits.fetch_add(1, Ordering::Relaxed);
            }
            CacheStatus::Miss | CacheStatus::Bypass => {
                entry.cache_misses.fetch_add(1, Ordering::Relaxed);
            }
        }

        if is_error {
            entry.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get path statistics
    pub async fn get_path_stats(&self) -> HashMap<String, PathStatsSnapshot> {
        let stats = self.path_stats.read().await;
        stats
            .iter()
            .map(|(path, s)| {
                let requests = s.requests.load(Ordering::Relaxed);
                let cache_hits = s.cache_hits.load(Ordering::Relaxed);
                let total_duration_ms = s.total_duration_ms.load(Ordering::Relaxed);

                (
                    path.clone(),
                    PathStatsSnapshot {
                        requests,
                        cache_hits,
                        cache_misses: s.cache_misses.load(Ordering::Relaxed),
                        errors: s.errors.load(Ordering::Relaxed),
                        avg_duration_ms: if requests > 0 {
                            total_duration_ms as f64 / requests as f64
                        } else {
                            0.0
                        },
                        bytes_sent: s.bytes_sent.load(Ordering::Relaxed),
                        cache_hit_ratio: if requests > 0 {
                            cache_hits as f64 / requests as f64
                        } else {
                            0.0
                        },
                    },
                )
            })
            .collect()
    }

    /// Export metrics in Prometheus format
    pub fn gather(&self) -> String {
        use prometheus::{Encoder, TextEncoder};
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathStatsSnapshot {
    pub requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub errors: u64,
    pub avg_duration_ms: f64,
    pub bytes_sent: u64,
    pub cache_hit_ratio: f64,
}

/// Extract path prefix for grouping (e.g., /api/v1/users/123 -> /api/v1/users)
fn extract_path_prefix(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // Keep first 3 segments, replace numeric segments with {id}
    let normalized: Vec<String> = segments
        .iter()
        .take(3)
        .map(|s| {
            if s.parse::<u64>().is_ok() || is_likely_id(s) {
                "{id}".to_string()
            } else {
                (*s).to_string()
            }
        })
        .collect();

    if normalized.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", normalized.join("/"))
    }
}

/// Check if a string looks like an ID (UUID, hex string, etc.)
fn is_likely_id(s: &str) -> bool {
    // UUID pattern
    if s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4 {
        return true;
    }
    // Hex string (likely a hash or ID)
    if s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    false
}

/// Initialize OpenTelemetry tracing
pub fn init_tracing(config: &ObservabilityConfig) -> anyhow::Result<()> {
    if !config.tracing.enabled {
        info!("OpenTelemetry tracing disabled");
        return Ok(());
    }

    let otlp_endpoint = config
        .tracing
        .otlp_endpoint
        .as_deref()
        .unwrap_or("http://localhost:4317");

    info!(endpoint = %otlp_endpoint, "Initializing OpenTelemetry tracing");

    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::TraceIdRatioBased(config.tracing.sample_rate))
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(Resource::new(vec![
                    KeyValue::new("service.name", config.tracing.service_name.clone()),
                    KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                ])),
        )
        .install_batch(runtime::Tokio)?;

    // Set global tracer
    global::set_tracer_provider(tracer.provider().unwrap().clone());

    info!("OpenTelemetry tracing initialized");
    Ok(())
}

/// Shutdown OpenTelemetry
pub fn shutdown_tracing() {
    global::shutdown_tracer_provider();
}

/// Middleware for request logging and tracing
pub async fn request_logging_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let start = Instant::now();
    let request_id = Uuid::new_v4().to_string();

    // Extract request info
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(|s| s.to_string());
    let user_agent = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let referer = request
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Extract client IP from headers or connection
    let client_ip = extract_client_ip(&request, addr.ip().to_string());

    // Extract trace context from headers
    let trace_id = request
        .headers()
        .get("x-trace-id")
        .or_else(|| request.headers().get("traceparent"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Create span for tracing
    let span = info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        path = %path,
        client_ip = %client_ip,
        trace_id = ?trace_id,
    );

    // Execute request
    let response = next.run(request).instrument(span).await;

    let duration = start.elapsed();
    let status = response.status();

    // Get response size if available
    let bytes_sent = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    // Get cache status from response headers
    let cache_status = response
        .headers()
        .get("x-cache-status")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("NONE")
        .to_string();

    // Get origin from response headers
    let origin = response
        .headers()
        .get("x-origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Create structured log entry
    let log_entry = RequestLogEntry {
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_default(),
        request_id: request_id.clone(),
        trace_id,
        method: method.clone(),
        path: path.clone(),
        query,
        origin: origin.clone(),
        status: status.as_u16(),
        cache_status: cache_status.clone(),
        duration_ms: duration.as_secs_f64() * 1000.0,
        bytes_sent,
        client_ip: Some(client_ip.clone()),
        user_agent,
        referer,
        country: None, // Would come from GeoIP lookup
    };

    // Log based on status
    if status.is_server_error() {
        error!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration.as_millis(),
            cache_status = %cache_status,
            "Request completed with server error"
        );
    } else if status.is_client_error() {
        warn!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration.as_millis(),
            "Request completed with client error"
        );
    } else {
        debug!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration.as_millis(),
            cache_status = %cache_status,
            bytes = bytes_sent,
            "Request completed"
        );
    }

    // Add request ID to response headers
    let mut response = response;
    if let Ok(value) = header::HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }

    response
}

fn extract_client_ip(request: &Request<Body>, fallback: String) -> String {
    // Check X-Forwarded-For
    if let Some(forwarded) = request.headers().get("x-forwarded-for") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first_ip) = value.split(',').next() {
                return first_ip.trim().to_string();
            }
        }
    }

    // Check X-Real-IP
    if let Some(real_ip) = request.headers().get("x-real-ip") {
        if let Ok(value) = real_ip.to_str() {
            return value.trim().to_string();
        }
    }

    // Check CF-Connecting-IP (Cloudflare)
    if let Some(cf_ip) = request.headers().get("cf-connecting-ip") {
        if let Ok(value) = cf_ip.to_str() {
            return value.trim().to_string();
        }
    }

    fallback
}

/// Alerting thresholds configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    /// Error rate threshold (percentage)
    pub error_rate_threshold: f64,
    /// Latency P99 threshold in milliseconds
    pub latency_p99_threshold_ms: u64,
    /// Cache hit ratio minimum threshold
    pub cache_hit_ratio_min: f64,
    /// Origin error rate threshold
    pub origin_error_rate_threshold: f64,
    /// Rate limiting threshold (requests per minute)
    pub rate_limit_threshold: u64,
    /// Circuit breaker open duration threshold in seconds
    pub circuit_breaker_open_threshold_secs: u64,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            error_rate_threshold: 5.0,        // 5% error rate
            latency_p99_threshold_ms: 1000,   // 1 second P99
            cache_hit_ratio_min: 0.7,         // 70% cache hit ratio
            origin_error_rate_threshold: 10.0, // 10% origin error rate
            rate_limit_threshold: 1000,       // 1000 rate limited requests/min
            circuit_breaker_open_threshold_secs: 60, // 60 seconds open
        }
    }
}

/// Alert state for monitoring
#[derive(Debug, Clone, Serialize)]
pub struct AlertState {
    pub alert_type: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub current_value: f64,
    pub threshold: f64,
    pub origin: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

/// Alert evaluator for checking thresholds
pub struct AlertEvaluator {
    thresholds: AlertThresholds,
    active_alerts: Arc<RwLock<Vec<AlertState>>>,
}

impl AlertEvaluator {
    pub fn new(thresholds: AlertThresholds) -> Self {
        Self {
            thresholds,
            active_alerts: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Evaluate error rate and generate alerts
    pub async fn evaluate_error_rate(&self, origin: &str, error_count: u64, total_count: u64) {
        if total_count == 0 {
            return;
        }

        let error_rate = (error_count as f64 / total_count as f64) * 100.0;

        if error_rate > self.thresholds.error_rate_threshold {
            let alert = AlertState {
                alert_type: "high_error_rate".to_string(),
                severity: if error_rate > self.thresholds.error_rate_threshold * 2.0 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
                message: format!(
                    "Error rate {}% exceeds threshold {}%",
                    error_rate, self.thresholds.error_rate_threshold
                ),
                current_value: error_rate,
                threshold: self.thresholds.error_rate_threshold,
                origin: Some(origin.to_string()),
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };

            self.add_alert(alert).await;
        }
    }

    /// Evaluate cache hit ratio
    pub async fn evaluate_cache_hit_ratio(&self, origin: &str, hits: u64, total: u64) {
        if total == 0 {
            return;
        }

        let hit_ratio = hits as f64 / total as f64;

        if hit_ratio < self.thresholds.cache_hit_ratio_min {
            let alert = AlertState {
                alert_type: "low_cache_hit_ratio".to_string(),
                severity: AlertSeverity::Warning,
                message: format!(
                    "Cache hit ratio {:.1}% below threshold {:.1}%",
                    hit_ratio * 100.0,
                    self.thresholds.cache_hit_ratio_min * 100.0
                ),
                current_value: hit_ratio,
                threshold: self.thresholds.cache_hit_ratio_min,
                origin: Some(origin.to_string()),
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };

            self.add_alert(alert).await;
        }
    }

    async fn add_alert(&self, alert: AlertState) {
        let mut alerts = self.active_alerts.write().await;

        // Remove old alerts of the same type for the same origin
        alerts.retain(|a| {
            !(a.alert_type == alert.alert_type && a.origin == alert.origin)
        });

        alerts.push(alert);

        // Keep only last 100 alerts
        let len = alerts.len();
        if len > 100 {
            alerts.drain(0..len - 100);
        }
    }

    /// Get active alerts
    pub async fn get_active_alerts(&self) -> Vec<AlertState> {
        self.active_alerts.read().await.clone()
    }

    /// Clear alerts for an origin
    pub async fn clear_alerts(&self, origin: Option<&str>) {
        let mut alerts = self.active_alerts.write().await;
        if let Some(origin) = origin {
            alerts.retain(|a| a.origin.as_deref() != Some(origin));
        } else {
            alerts.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_path_prefix() {
        assert_eq!(extract_path_prefix("/"), "/");
        assert_eq!(extract_path_prefix("/api"), "/api");
        assert_eq!(extract_path_prefix("/api/v1/users"), "/api/v1/users");
        assert_eq!(extract_path_prefix("/api/v1/users/123"), "/api/v1/users");
        assert_eq!(
            extract_path_prefix("/api/v1/users/550e8400-e29b-41d4-a716-446655440000"),
            "/api/v1/users"
        );
    }

    #[test]
    fn test_is_likely_id() {
        assert!(is_likely_id("550e8400-e29b-41d4-a716-446655440000")); // UUID
        assert!(is_likely_id("abc123def456abc123def456")); // Hex string
        assert!(!is_likely_id("users"));
        assert!(!is_likely_id("api"));
    }

    #[test]
    fn test_alert_thresholds_default() {
        let thresholds = AlertThresholds::default();
        assert_eq!(thresholds.error_rate_threshold, 5.0);
        assert_eq!(thresholds.latency_p99_threshold_ms, 1000);
    }

    #[tokio::test]
    async fn test_alert_evaluator() {
        let evaluator = AlertEvaluator::new(AlertThresholds::default());

        // Should not trigger alert (2% error rate)
        evaluator.evaluate_error_rate("origin1", 2, 100).await;
        assert!(evaluator.get_active_alerts().await.is_empty());

        // Should trigger alert (10% error rate)
        evaluator.evaluate_error_rate("origin1", 10, 100).await;
        let alerts = evaluator.get_active_alerts().await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].alert_type, "high_error_rate");
    }
}
