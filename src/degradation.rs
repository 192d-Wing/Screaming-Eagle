//! Graceful degradation patterns for handling origin failures.
//!
//! Provides fallback behaviors when origins are unavailable, including:
//! - Stale content serving (already in cache module)
//! - Static fallback responses
//! - Failover to backup origins
//! - Reduced functionality mode

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Degradation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DegradationConfig {
    /// Enable graceful degradation
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Serve stale content when origin fails (in addition to stale-if-error)
    #[serde(default = "default_true")]
    pub serve_stale_on_error: bool,

    /// Maximum stale age to serve during degradation (seconds)
    #[serde(default = "default_max_stale_age")]
    pub max_stale_age_secs: u64,

    /// Static fallback responses by path pattern
    #[serde(default)]
    pub fallbacks: Vec<FallbackRule>,

    /// Backup origins to try when primary fails
    #[serde(default)]
    pub backup_origins: HashMap<String, Vec<String>>,

    /// Enter reduced mode after this many consecutive failures
    #[serde(default = "default_reduced_mode_threshold")]
    pub reduced_mode_threshold: u32,

    /// Exit reduced mode after this duration without failures
    #[serde(default = "default_reduced_mode_recovery")]
    pub reduced_mode_recovery_secs: u64,
}

impl Default for DegradationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            serve_stale_on_error: true,
            max_stale_age_secs: default_max_stale_age(),
            fallbacks: Vec::new(),
            backup_origins: HashMap::new(),
            reduced_mode_threshold: default_reduced_mode_threshold(),
            reduced_mode_recovery_secs: default_reduced_mode_recovery(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_max_stale_age() -> u64 {
    86400 // 24 hours
}

fn default_reduced_mode_threshold() -> u32 {
    10
}

fn default_reduced_mode_recovery() -> u64 {
    300 // 5 minutes
}

/// A fallback rule for specific paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackRule {
    /// Path pattern (prefix match)
    pub path_prefix: String,

    /// Status code to return
    #[serde(default = "default_fallback_status")]
    pub status: u16,

    /// Response body
    #[serde(default)]
    pub body: String,

    /// Content-Type header
    #[serde(default = "default_content_type")]
    pub content_type: String,

    /// Additional headers
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn default_fallback_status() -> u16 {
    503
}

fn default_content_type() -> String {
    "text/html; charset=utf-8".to_string()
}

/// Degradation state for an origin.
#[derive(Debug)]
pub struct OriginDegradationState {
    /// Consecutive failure count
    failures: AtomicU64,
    /// Last failure timestamp (unix millis)
    last_failure: AtomicU64,
    /// Whether we're in reduced mode
    reduced_mode: AtomicBool,
    /// When reduced mode started
    reduced_mode_since: RwLock<Option<Instant>>,
}

impl Default for OriginDegradationState {
    fn default() -> Self {
        Self {
            failures: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
            reduced_mode: AtomicBool::new(false),
            reduced_mode_since: RwLock::new(None),
        }
    }
}

impl OriginDegradationState {
    pub fn record_failure(&self, threshold: u32) {
        let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
        self.last_failure.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            Ordering::SeqCst,
        );

        if failures >= threshold as u64 && !self.reduced_mode.load(Ordering::SeqCst) {
            self.reduced_mode.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = self.reduced_mode_since.try_write() {
                *guard = Some(Instant::now());
            }
            warn!(failures, "Origin entering reduced mode");
        }
    }

    pub fn record_success(&self, recovery_secs: u64) {
        self.failures.store(0, Ordering::SeqCst);

        // Check if we should exit reduced mode
        if self.reduced_mode.load(Ordering::SeqCst) {
            if let Ok(guard) = self.reduced_mode_since.try_read() {
                if let Some(since) = *guard {
                    if since.elapsed() >= Duration::from_secs(recovery_secs) {
                        self.reduced_mode.store(false, Ordering::SeqCst);
                        info!("Origin exiting reduced mode after recovery period");
                    }
                }
            }
        }
    }

    pub fn is_reduced_mode(&self) -> bool {
        self.reduced_mode.load(Ordering::SeqCst)
    }

    pub fn failure_count(&self) -> u64 {
        self.failures.load(Ordering::SeqCst)
    }
}

/// Degradation manager handling fallbacks and state.
pub struct DegradationManager {
    config: DegradationConfig,
    origin_states: dashmap::DashMap<String, Arc<OriginDegradationState>>,
    compiled_fallbacks: Vec<CompiledFallback>,
}

struct CompiledFallback {
    path_prefix: String,
    status: StatusCode,
    body: Bytes,
    headers: HeaderMap,
}

impl DegradationManager {
    pub fn new(config: DegradationConfig) -> Self {
        let compiled_fallbacks = config
            .fallbacks
            .iter()
            .filter_map(|f| {
                let status = StatusCode::from_u16(f.status).ok()?;
                let mut headers = HeaderMap::new();
                if let Ok(ct) = HeaderValue::from_str(&f.content_type) {
                    headers.insert("content-type", ct);
                }
                for (k, v) in &f.headers {
                    if let (Ok(name), Ok(val)) = (
                        k.parse::<axum::http::header::HeaderName>(),
                        HeaderValue::from_str(v),
                    ) {
                        headers.insert(name, val);
                    }
                }
                Some(CompiledFallback {
                    path_prefix: f.path_prefix.clone(),
                    status,
                    body: Bytes::from(f.body.clone()),
                    headers,
                })
            })
            .collect();

        Self {
            config,
            origin_states: dashmap::DashMap::new(),
            compiled_fallbacks,
        }
    }

    /// Get or create degradation state for an origin.
    pub fn get_state(&self, origin: &str) -> Arc<OriginDegradationState> {
        self.origin_states
            .entry(origin.to_string())
            .or_insert_with(|| Arc::new(OriginDegradationState::default()))
            .clone()
    }

    /// Record a failure for an origin.
    pub fn record_failure(&self, origin: &str) {
        if !self.config.enabled {
            return;
        }
        let state = self.get_state(origin);
        state.record_failure(self.config.reduced_mode_threshold);
    }

    /// Record a success for an origin.
    pub fn record_success(&self, origin: &str) {
        if !self.config.enabled {
            return;
        }
        let state = self.get_state(origin);
        state.record_success(self.config.reduced_mode_recovery_secs);
    }

    /// Check if an origin is in reduced mode.
    pub fn is_reduced_mode(&self, origin: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        self.origin_states
            .get(origin)
            .map(|s| s.is_reduced_mode())
            .unwrap_or(false)
    }

    /// Get backup origins for a failed primary.
    pub fn get_backup_origins(&self, primary: &str) -> Option<Vec<String>> {
        self.config.backup_origins.get(primary).cloned()
    }

    /// Find a fallback response for a path.
    pub fn find_fallback(&self, path: &str) -> Option<FallbackResponse> {
        if !self.config.enabled {
            return None;
        }

        for fallback in &self.compiled_fallbacks {
            if path.starts_with(&fallback.path_prefix) {
                debug!(path, prefix = %fallback.path_prefix, "Using fallback response");
                return Some(FallbackResponse {
                    status: fallback.status,
                    headers: fallback.headers.clone(),
                    body: fallback.body.clone(),
                });
            }
        }
        None
    }

    /// Check if we should serve stale content.
    pub fn should_serve_stale(&self) -> bool {
        self.config.enabled && self.config.serve_stale_on_error
    }

    /// Maximum stale age to serve.
    pub fn max_stale_age(&self) -> Duration {
        Duration::from_secs(self.config.max_stale_age_secs)
    }

    /// Get degradation status for all origins.
    pub fn status(&self) -> DegradationStatus {
        let origins: HashMap<String, OriginStatus> = self
            .origin_states
            .iter()
            .map(|entry| {
                let name = entry.key().clone();
                let state = entry.value();
                (
                    name,
                    OriginStatus {
                        failures: state.failure_count(),
                        reduced_mode: state.is_reduced_mode(),
                    },
                )
            })
            .collect();

        DegradationStatus {
            enabled: self.config.enabled,
            origins,
        }
    }
}

/// A fallback response to serve.
#[derive(Debug, Clone)]
pub struct FallbackResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Status of an origin for monitoring.
#[derive(Debug, Clone, Serialize)]
pub struct OriginStatus {
    pub failures: u64,
    pub reduced_mode: bool,
}

/// Overall degradation status.
#[derive(Debug, Clone, Serialize)]
pub struct DegradationStatus {
    pub enabled: bool,
    pub origins: HashMap<String, OriginStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_enabled() {
        let config = DegradationConfig::default();
        assert!(config.enabled);
        assert!(config.serve_stale_on_error);
    }

    #[test]
    fn origin_state_tracks_failures() {
        let state = OriginDegradationState::default();
        assert_eq!(state.failure_count(), 0);
        state.record_failure(10);
        assert_eq!(state.failure_count(), 1);
        state.record_failure(10);
        assert_eq!(state.failure_count(), 2);
    }

    #[test]
    fn origin_enters_reduced_mode_after_threshold() {
        let state = OriginDegradationState::default();
        assert!(!state.is_reduced_mode());

        for _ in 0..10 {
            state.record_failure(10);
        }

        assert!(state.is_reduced_mode());
    }

    #[test]
    fn success_resets_failure_count() {
        let state = OriginDegradationState::default();
        state.record_failure(10);
        state.record_failure(10);
        assert_eq!(state.failure_count(), 2);

        state.record_success(300);
        assert_eq!(state.failure_count(), 0);
    }

    #[test]
    fn fallback_matches_path_prefix() {
        let config = DegradationConfig {
            fallbacks: vec![FallbackRule {
                path_prefix: "/api/".to_string(),
                status: 503,
                body: r#"{"error": "unavailable"}"#.to_string(),
                content_type: "application/json".to_string(),
                headers: HashMap::new(),
            }],
            ..Default::default()
        };

        let manager = DegradationManager::new(config);

        assert!(manager.find_fallback("/api/users").is_some());
        assert!(manager.find_fallback("/web/page").is_none());
    }

    #[test]
    fn manager_tracks_origin_states() {
        let manager = DegradationManager::new(DegradationConfig::default());

        manager.record_failure("origin1");
        manager.record_failure("origin1");
        manager.record_success("origin2");

        let status = manager.status();
        assert_eq!(status.origins.get("origin1").unwrap().failures, 2);
        assert_eq!(status.origins.get("origin2").unwrap().failures, 0);
    }
}
