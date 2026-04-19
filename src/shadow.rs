//! Traffic shadowing (mirroring) to test origins.
//!
//! Mirrors a percentage of production traffic to shadow origins for testing
//! without affecting the primary response path.

use bytes::Bytes;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

/// Traffic shadowing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowConfig {
    /// Enable traffic shadowing
    #[serde(default)]
    pub enabled: bool,

    /// Shadow rules mapping primary origins to shadow targets
    #[serde(default)]
    pub rules: Vec<ShadowRule>,

    /// Maximum concurrent shadow requests
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// Shadow request timeout (seconds)
    #[serde(default = "default_shadow_timeout")]
    pub timeout_secs: u64,

    /// Whether to log response differences
    #[serde(default = "default_true")]
    pub log_differences: bool,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rules: Vec::new(),
            max_concurrent: default_max_concurrent(),
            timeout_secs: default_shadow_timeout(),
            log_differences: true,
        }
    }
}

fn default_max_concurrent() -> usize {
    100
}

fn default_shadow_timeout() -> u64 {
    10
}

fn default_true() -> bool {
    true
}

/// A shadow rule defining where to mirror traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowRule {
    /// Name for this shadow rule
    pub name: String,

    /// Primary origin to shadow (or "*" for all)
    pub primary_origin: String,

    /// Shadow target URL (base URL)
    pub shadow_url: String,

    /// Percentage of traffic to shadow (0-100)
    #[serde(default = "default_sample_rate")]
    pub sample_percent: u8,

    /// Path patterns to include (regex, empty = all)
    #[serde(default)]
    pub include_paths: Vec<String>,

    /// Path patterns to exclude (regex)
    #[serde(default)]
    pub exclude_paths: Vec<String>,

    /// HTTP methods to shadow
    #[serde(default = "default_methods")]
    pub methods: Vec<String>,

    /// Headers to forward to shadow
    #[serde(default = "default_forward_headers")]
    pub forward_headers: Vec<String>,

    /// Additional headers to add to shadow requests
    #[serde(default)]
    pub add_headers: HashMap<String, String>,
}

fn default_sample_rate() -> u8 {
    100
}

fn default_methods() -> Vec<String> {
    vec!["GET".to_string(), "HEAD".to_string()]
}

fn default_forward_headers() -> Vec<String> {
    vec![
        "accept".to_string(),
        "accept-encoding".to_string(),
        "accept-language".to_string(),
        "user-agent".to_string(),
        "x-request-id".to_string(),
    ]
}

/// Compiled shadow rule with regex patterns.
pub struct CompiledShadowRule {
    pub name: String,
    pub primary_origin: String,
    pub shadow_url: String,
    pub sample_percent: u8,
    pub include_paths: Vec<regex::Regex>,
    pub exclude_paths: Vec<regex::Regex>,
    pub methods: Vec<http::Method>,
    pub forward_headers: Vec<String>,
    pub add_headers: HashMap<String, String>,
}

impl CompiledShadowRule {
    fn from_config(rule: &ShadowRule) -> Option<Self> {
        let include_paths: Vec<regex::Regex> = rule
            .include_paths
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();

        let exclude_paths: Vec<regex::Regex> = rule
            .exclude_paths
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();

        let methods: Vec<http::Method> = rule
            .methods
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect();

        Some(Self {
            name: rule.name.clone(),
            primary_origin: rule.primary_origin.clone(),
            shadow_url: rule.shadow_url.clone(),
            sample_percent: rule.sample_percent.min(100),
            include_paths,
            exclude_paths,
            methods,
            forward_headers: rule.forward_headers.clone(),
            add_headers: rule.add_headers.clone(),
        })
    }

    fn matches(&self, origin: &str, path: &str, method: &http::Method) -> bool {
        // Check origin
        if self.primary_origin != "*" && self.primary_origin != origin {
            return false;
        }

        // Check method
        if !self.methods.is_empty() && !self.methods.contains(method) {
            return false;
        }

        // Check exclude patterns first
        for pattern in &self.exclude_paths {
            if pattern.is_match(path) {
                return false;
            }
        }

        // Check include patterns (empty = include all)
        if self.include_paths.is_empty() {
            return true;
        }

        for pattern in &self.include_paths {
            if pattern.is_match(path) {
                return true;
            }
        }

        false
    }

    fn should_sample(&self) -> bool {
        if self.sample_percent >= 100 {
            return true;
        }
        if self.sample_percent == 0 {
            return false;
        }
        fastrand::u8(0..100) < self.sample_percent
    }
}

/// Shadow request to be executed.
#[derive(Debug, Clone)]
pub struct ShadowRequest {
    pub rule_name: String,
    pub method: http::Method,
    pub url: String,
    pub headers: http::HeaderMap,
    pub body: Option<Bytes>,
}

/// Shadow response for comparison.
#[derive(Debug)]
pub struct ShadowResponse {
    pub rule_name: String,
    pub status: http::StatusCode,
    pub latency_ms: u64,
    pub body_size: usize,
}

/// Traffic shadowing manager.
pub struct ShadowManager {
    config: ShadowConfig,
    rules: Vec<CompiledShadowRule>,
    client: Client,
    semaphore: Arc<Semaphore>,
    stats: Arc<ShadowStats>,
}

/// Shadow statistics.
#[derive(Debug, Default)]
pub struct ShadowStats {
    pub requests_sent: AtomicU64,
    pub requests_succeeded: AtomicU64,
    pub requests_failed: AtomicU64,
    pub requests_skipped: AtomicU64,
}

impl ShadowManager {
    pub fn new(config: ShadowConfig) -> Self {
        let rules: Vec<CompiledShadowRule> = config
            .rules
            .iter()
            .filter_map(CompiledShadowRule::from_config)
            .collect();

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();

        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));

        if config.enabled && !rules.is_empty() {
            info!(rules = rules.len(), "Traffic shadowing enabled");
        }

        Self {
            config,
            rules,
            client,
            semaphore,
            stats: Arc::new(ShadowStats::default()),
        }
    }

    /// Check if shadowing is enabled and applicable.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled && !self.rules.is_empty()
    }

    /// Find matching shadow rules for a request.
    pub fn find_rules(
        &self,
        origin: &str,
        path: &str,
        method: &http::Method,
    ) -> Vec<&CompiledShadowRule> {
        if !self.config.enabled {
            return Vec::new();
        }

        self.rules
            .iter()
            .filter(|r| r.matches(origin, path, method) && r.should_sample())
            .collect()
    }

    /// Build a shadow request from an incoming request.
    pub fn build_shadow_request(
        &self,
        rule: &CompiledShadowRule,
        path: &str,
        method: &http::Method,
        headers: &http::HeaderMap,
        body: Option<Bytes>,
    ) -> ShadowRequest {
        let url = format!("{}{}", rule.shadow_url.trim_end_matches('/'), path);

        let mut shadow_headers = http::HeaderMap::new();

        // Forward specified headers
        for header_name in &rule.forward_headers {
            if let Some(value) = headers.get(header_name) {
                if let Ok(name) = header_name.parse::<http::header::HeaderName>() {
                    shadow_headers.insert(name, value.clone());
                }
            }
        }

        // Add custom headers
        for (name, value) in &rule.add_headers {
            if let (Ok(n), Ok(v)) = (
                name.parse::<http::header::HeaderName>(),
                http::HeaderValue::from_str(value),
            ) {
                shadow_headers.insert(n, v);
            }
        }

        // Mark as shadow request
        shadow_headers.insert(
            "x-shadow-request",
            http::HeaderValue::from_static("true"),
        );
        shadow_headers.insert(
            "x-shadow-rule",
            http::HeaderValue::from_str(&rule.name).unwrap_or_else(|_| {
                http::HeaderValue::from_static("unknown")
            }),
        );

        ShadowRequest {
            rule_name: rule.name.clone(),
            method: method.clone(),
            url,
            headers: shadow_headers,
            body,
        }
    }

    /// Execute a shadow request asynchronously (fire-and-forget).
    pub fn execute_shadow(&self, request: ShadowRequest) {
        let client = self.client.clone();
        let semaphore = self.semaphore.clone();
        let log_diff = self.config.log_differences;
        let stats = self.stats.clone();

        // Don't block if at capacity
        let permit = match semaphore.try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.stats.requests_skipped.fetch_add(1, Ordering::Relaxed);
                debug!(rule = %request.rule_name, "Shadow request skipped (at capacity)");
                return;
            }
        };

        tokio::spawn(async move {
            let _permit = permit;
            let start = std::time::Instant::now();

            stats.requests_sent.fetch_add(1, Ordering::Relaxed);

            let mut req_builder = client.request(
                request.method.clone(),
                &request.url,
            );

            for (name, value) in request.headers.iter() {
                req_builder = req_builder.header(name, value);
            }

            if let Some(body) = request.body {
                req_builder = req_builder.body(body);
            }

            match req_builder.send().await {
                Ok(response) => {
                    let status = response.status();
                    let latency_ms = start.elapsed().as_millis() as u64;

                    stats.requests_succeeded.fetch_add(1, Ordering::Relaxed);

                    if log_diff {
                        debug!(
                            rule = %request.rule_name,
                            url = %request.url,
                            status = %status,
                            latency_ms,
                            "Shadow request completed"
                        );
                    }
                }
                Err(e) => {
                    stats.requests_failed.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        rule = %request.rule_name,
                        url = %request.url,
                        error = %e,
                        "Shadow request failed"
                    );
                }
            }
        });
    }

    /// Execute shadow requests for matching rules.
    pub fn shadow_request(
        &self,
        origin: &str,
        path: &str,
        method: &http::Method,
        headers: &http::HeaderMap,
        body: Option<Bytes>,
    ) {
        if !self.is_enabled() {
            return;
        }

        let rules = self.find_rules(origin, path, method);
        for rule in rules {
            let request = self.build_shadow_request(rule, path, method, headers, body.clone());
            self.execute_shadow(request);
        }
    }

    /// Get shadow statistics.
    pub fn stats(&self) -> ShadowStatsSnapshot {
        ShadowStatsSnapshot {
            requests_sent: self.stats.requests_sent.load(Ordering::Relaxed),
            requests_succeeded: self.stats.requests_succeeded.load(Ordering::Relaxed),
            requests_failed: self.stats.requests_failed.load(Ordering::Relaxed),
            requests_skipped: self.stats.requests_skipped.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of shadow statistics.
#[derive(Debug, Clone, Serialize)]
pub struct ShadowStatsSnapshot {
    pub requests_sent: u64,
    pub requests_succeeded: u64,
    pub requests_failed: u64,
    pub requests_skipped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let config = ShadowConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn rule_matches_wildcard_origin() {
        let rule = CompiledShadowRule::from_config(&ShadowRule {
            name: "test".to_string(),
            primary_origin: "*".to_string(),
            shadow_url: "http://shadow.local".to_string(),
            sample_percent: 100,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            methods: vec!["GET".to_string()],
            forward_headers: Vec::new(),
            add_headers: HashMap::new(),
        })
        .unwrap();

        assert!(rule.matches("any-origin", "/path", &http::Method::GET));
        assert!(!rule.matches("any-origin", "/path", &http::Method::POST));
    }

    #[test]
    fn rule_excludes_patterns() {
        let rule = CompiledShadowRule::from_config(&ShadowRule {
            name: "test".to_string(),
            primary_origin: "*".to_string(),
            shadow_url: "http://shadow.local".to_string(),
            sample_percent: 100,
            include_paths: Vec::new(),
            exclude_paths: vec!["^/health".to_string(), "^/metrics".to_string()],
            methods: vec!["GET".to_string()],
            forward_headers: Vec::new(),
            add_headers: HashMap::new(),
        })
        .unwrap();

        assert!(rule.matches("origin", "/api/users", &http::Method::GET));
        assert!(!rule.matches("origin", "/health", &http::Method::GET));
        assert!(!rule.matches("origin", "/metrics", &http::Method::GET));
    }

    #[test]
    fn rule_includes_patterns() {
        let rule = CompiledShadowRule::from_config(&ShadowRule {
            name: "test".to_string(),
            primary_origin: "*".to_string(),
            shadow_url: "http://shadow.local".to_string(),
            sample_percent: 100,
            include_paths: vec!["^/api/".to_string()],
            exclude_paths: Vec::new(),
            methods: vec!["GET".to_string()],
            forward_headers: Vec::new(),
            add_headers: HashMap::new(),
        })
        .unwrap();

        assert!(rule.matches("origin", "/api/users", &http::Method::GET));
        assert!(!rule.matches("origin", "/web/page", &http::Method::GET));
    }

    #[test]
    fn manager_builds_shadow_request() {
        let config = ShadowConfig {
            enabled: true,
            rules: vec![ShadowRule {
                name: "test".to_string(),
                primary_origin: "*".to_string(),
                shadow_url: "http://shadow.local".to_string(),
                sample_percent: 100,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                methods: vec!["GET".to_string()],
                forward_headers: vec!["user-agent".to_string()],
                add_headers: [("x-test".to_string(), "value".to_string())]
                    .into_iter()
                    .collect(),
            }],
            ..Default::default()
        };

        let manager = ShadowManager::new(config);
        let rules = manager.find_rules("origin", "/path", &http::Method::GET);
        assert_eq!(rules.len(), 1);

        let mut headers = http::HeaderMap::new();
        headers.insert("user-agent", "test-agent".parse().unwrap());

        let req = manager.build_shadow_request(rules[0], "/path", &http::Method::GET, &headers, None);

        assert_eq!(req.url, "http://shadow.local/path");
        assert!(req.headers.contains_key("x-shadow-request"));
        assert!(req.headers.contains_key("user-agent"));
        assert!(req.headers.contains_key("x-test"));
    }
}
