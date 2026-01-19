//! Origin health checking module
//!
//! Provides periodic health checks for configured origins and tracks their status.

use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::config::OriginConfig;

/// Health status of an origin
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Origin is healthy and responding
    Healthy,
    /// Origin is unhealthy (failed health checks)
    Unhealthy,
    /// Health status is unknown (no checks yet)
    Unknown,
}

impl HealthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HealthStatus::Healthy => "healthy",
            HealthStatus::Unhealthy => "unhealthy",
            HealthStatus::Unknown => "unknown",
        }
    }
}

/// Information about an origin's health
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginHealth {
    pub status: HealthStatus,
    pub last_check: Option<u64>, // Unix timestamp
    pub last_success: Option<u64>,
    pub last_failure: Option<u64>,
    pub consecutive_failures: u32,
    pub response_time_ms: Option<u64>,
    pub error_message: Option<String>,
}

impl Default for OriginHealth {
    fn default() -> Self {
        Self {
            status: HealthStatus::Unknown,
            last_check: None,
            last_success: None,
            last_failure: None,
            consecutive_failures: 0,
            response_time_ms: None,
            error_message: None,
        }
    }
}

/// Health checker for all origins
pub struct HealthChecker {
    client: Client,
    origins: HashMap<String, OriginConfig>,
    health_status: Arc<DashMap<String, OriginHealth>>,
    unhealthy_threshold: u32,
}

impl HealthChecker {
    pub fn new(origins: HashMap<String, OriginConfig>) -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create health check HTTP client");

        let health_status = Arc::new(DashMap::new());

        // Initialize health status for all origins
        for name in origins.keys() {
            health_status.insert(name.clone(), OriginHealth::default());
        }

        Self {
            client,
            origins,
            health_status,
            unhealthy_threshold: 3, // 3 consecutive failures = unhealthy
        }
    }

    /// Get the current health status for an origin
    pub fn get_status(&self, origin_name: &str) -> Option<OriginHealth> {
        self.health_status.get(origin_name).map(|h| h.clone())
    }

    /// Get all origin health statuses
    pub fn get_all_statuses(&self) -> HashMap<String, OriginHealth> {
        self.health_status
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    /// Check if an origin is healthy
    pub fn is_healthy(&self, origin_name: &str) -> bool {
        self.health_status
            .get(origin_name)
            .map(|h| h.status == HealthStatus::Healthy || h.status == HealthStatus::Unknown)
            .unwrap_or(true) // Default to healthy if unknown origin
    }

    /// Perform a health check for a specific origin
    pub async fn check_origin(&self, origin_name: &str) -> HealthStatus {
        let origin = match self.origins.get(origin_name) {
            Some(o) => o,
            None => {
                warn!(origin = %origin_name, "Unknown origin for health check");
                return HealthStatus::Unknown;
            }
        };

        // If no health check path configured, skip
        let health_path = match &origin.health_check_path {
            Some(p) => p,
            None => {
                debug!(origin = %origin_name, "No health check path configured");
                return HealthStatus::Unknown;
            }
        };

        let url = format!("{}{}", origin.url.trim_end_matches('/'), health_path);
        let start = Instant::now();

        debug!(origin = %origin_name, url = %url, "Performing health check");

        let result = self
            .client
            .get(&url)
            .timeout(origin.health_check_timeout())
            .send()
            .await;

        let response_time = start.elapsed();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut health = self
            .health_status
            .get(origin_name)
            .map(|h| h.clone())
            .unwrap_or_default();

        health.last_check = Some(now);
        health.response_time_ms = Some(response_time.as_millis() as u64);

        match result {
            Ok(response) if response.status().is_success() => {
                health.status = HealthStatus::Healthy;
                health.last_success = Some(now);
                health.consecutive_failures = 0;
                health.error_message = None;

                info!(
                    origin = %origin_name,
                    status = response.status().as_u16(),
                    response_time_ms = response_time.as_millis(),
                    "Health check passed"
                );
            }
            Ok(response) => {
                health.consecutive_failures += 1;
                health.last_failure = Some(now);
                health.error_message = Some(format!("HTTP {}", response.status()));

                if health.consecutive_failures >= self.unhealthy_threshold {
                    health.status = HealthStatus::Unhealthy;
                }

                warn!(
                    origin = %origin_name,
                    status = response.status().as_u16(),
                    consecutive_failures = health.consecutive_failures,
                    "Health check failed: non-success status"
                );
            }
            Err(e) => {
                health.consecutive_failures += 1;
                health.last_failure = Some(now);
                health.error_message = Some(e.to_string());

                if health.consecutive_failures >= self.unhealthy_threshold {
                    health.status = HealthStatus::Unhealthy;
                }

                error!(
                    origin = %origin_name,
                    error = %e,
                    consecutive_failures = health.consecutive_failures,
                    "Health check failed: connection error"
                );
            }
        }

        let status = health.status;
        self.health_status.insert(origin_name.to_string(), health);
        status
    }

    /// Check all origins
    pub async fn check_all(&self) {
        for origin_name in self.origins.keys() {
            self.check_origin(origin_name).await;
        }
    }

    /// Get a clone of the health status map for sharing
    pub fn health_status_handle(&self) -> Arc<DashMap<String, OriginHealth>> {
        Arc::clone(&self.health_status)
    }
}

/// Spawn background health check tasks for all origins
pub fn spawn_health_checks(checker: Arc<HealthChecker>, shutdown: watch::Receiver<bool>) {
    for (origin_name, origin_config) in &checker.origins {
        if origin_config.health_check_path.is_none() {
            debug!(origin = %origin_name, "Skipping health checks (no path configured)");
            continue;
        }

        let checker = Arc::clone(&checker);
        let origin_name = origin_name.clone();
        let interval = origin_config.health_check_interval();
        let mut shutdown = shutdown.clone();

        tokio::spawn(async move {
            info!(
                origin = %origin_name,
                interval_secs = interval.as_secs(),
                "Starting health check task"
            );

            // Initial check
            checker.check_origin(&origin_name).await;

            let mut interval_timer = tokio::time::interval(interval);
            interval_timer.tick().await; // Skip first tick

            loop {
                tokio::select! {
                    _ = interval_timer.tick() => {
                        checker.check_origin(&origin_name).await;
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!(origin = %origin_name, "Shutting down health check task");
                            break;
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_status_default() {
        let health = OriginHealth::default();
        assert_eq!(health.status, HealthStatus::Unknown);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn test_health_checker_new() {
        let mut origins = HashMap::new();
        origins.insert(
            "test".to_string(),
            OriginConfig {
                url: "http://localhost:8080".to_string(),
                host_header: None,
                timeout_secs: 30,
                max_retries: 3,
                headers: HashMap::new(),
                health_check_path: Some("/health".to_string()),
                health_check_interval_secs: 30,
                health_check_timeout_secs: 5,
            },
        );

        let checker = HealthChecker::new(origins);
        let status = checker.get_status("test");
        assert!(status.is_some());
        assert_eq!(status.unwrap().status, HealthStatus::Unknown);
    }

    #[test]
    fn test_is_healthy_unknown_origin() {
        let checker = HealthChecker::new(HashMap::new());
        // Unknown origins default to healthy
        assert!(checker.is_healthy("nonexistent"));
    }
}
