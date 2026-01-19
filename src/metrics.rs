use axum::http::StatusCode;
use prometheus::{CounterVec, Encoder, HistogramOpts, HistogramVec, Opts, Registry, TextEncoder};
use std::time::Duration;

use crate::cache::CacheStatus;

pub struct Metrics {
    registry: Registry,
    requests_total: CounterVec,
    cache_hits: CounterVec,
    cache_misses: CounterVec,
    request_duration: HistogramVec,
    origin_requests: CounterVec,
    bytes_served: CounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        // Total requests counter
        let requests_total = CounterVec::new(
            Opts::new("cdn_requests_total", "Total number of CDN requests"),
            &["origin", "status", "cache_status"],
        )
        .unwrap();

        // Cache hit counter
        let cache_hits = CounterVec::new(
            Opts::new("cdn_cache_hits_total", "Total cache hits"),
            &["origin"],
        )
        .unwrap();

        // Cache miss counter
        let cache_misses = CounterVec::new(
            Opts::new("cdn_cache_misses_total", "Total cache misses"),
            &["origin"],
        )
        .unwrap();

        // Request duration histogram
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "cdn_request_duration_seconds",
                "Request duration in seconds",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["origin", "cache_status"],
        )
        .unwrap();

        // Origin requests counter
        let origin_requests = CounterVec::new(
            Opts::new(
                "cdn_origin_requests_total",
                "Total requests to origin servers",
            ),
            &["origin", "status"],
        )
        .unwrap();

        // Bytes served counter
        let bytes_served = CounterVec::new(
            Opts::new("cdn_bytes_served_total", "Total bytes served"),
            &["origin", "cache_status"],
        )
        .unwrap();

        // Register all metrics
        registry.register(Box::new(requests_total.clone())).unwrap();
        registry.register(Box::new(cache_hits.clone())).unwrap();
        registry.register(Box::new(cache_misses.clone())).unwrap();
        registry
            .register(Box::new(request_duration.clone()))
            .unwrap();
        registry
            .register(Box::new(origin_requests.clone()))
            .unwrap();
        registry.register(Box::new(bytes_served.clone())).unwrap();

        Self {
            registry,
            requests_total,
            cache_hits,
            cache_misses,
            request_duration,
            origin_requests,
            bytes_served,
        }
    }

    pub fn record_request(
        &self,
        origin: &str,
        cache_status: CacheStatus,
        status: StatusCode,
        duration: Duration,
    ) {
        let status_str = status.as_u16().to_string();
        let cache_str = cache_status.as_str();

        self.requests_total
            .with_label_values(&[origin, &status_str, cache_str])
            .inc();

        self.request_duration
            .with_label_values(&[origin, cache_str])
            .observe(duration.as_secs_f64());

        match cache_status {
            CacheStatus::Hit | CacheStatus::Stale | CacheStatus::StaleIfError => {
                self.cache_hits.with_label_values(&[origin]).inc();
            }
            CacheStatus::Miss | CacheStatus::Bypass => {
                self.cache_misses.with_label_values(&[origin]).inc();
            }
        }
    }

    pub fn record_origin_request(&self, origin: &str, status: StatusCode) {
        self.origin_requests
            .with_label_values(&[origin, &status.as_u16().to_string()])
            .inc();
    }

    pub fn record_bytes_served(&self, origin: &str, cache_status: CacheStatus, bytes: u64) {
        self.bytes_served
            .with_label_values(&[origin, cache_status.as_str()])
            .inc_by(bytes as f64);
    }

    pub fn gather(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
