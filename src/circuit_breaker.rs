use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed, requests flow normally
    Closed,
    /// Circuit is open, requests are rejected
    Open,
    /// Circuit is half-open, limited requests allowed to test recovery
    HalfOpen,
}

/// Configuration for circuit breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening the circuit
    pub failure_threshold: u32,
    /// Duration to wait before transitioning from Open to HalfOpen
    pub reset_timeout_secs: u64,
    /// Number of successful requests needed to close the circuit from HalfOpen
    pub success_threshold: u32,
    /// Window size for counting failures (in seconds)
    pub failure_window_secs: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            reset_timeout_secs: 30,
            success_threshold: 3,
            failure_window_secs: 60,
        }
    }
}

/// Individual circuit breaker for a single origin
pub struct CircuitBreaker {
    state: RwLock<CircuitState>,
    failure_count: AtomicU32,
    success_count: AtomicU32,
    last_failure_time: RwLock<Option<Instant>>,
    opened_at: RwLock<Option<Instant>>,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: RwLock::new(CircuitState::Closed),
            failure_count: AtomicU32::new(0),
            success_count: AtomicU32::new(0),
            last_failure_time: RwLock::new(None),
            opened_at: RwLock::new(None),
            config,
        }
    }

    /// Check if a request should be allowed
    pub fn should_allow(&self) -> bool {
        let state = *self.state.read().unwrap();

        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if we should transition to HalfOpen
                if self.should_transition_to_half_open() {
                    self.transition_to_half_open();
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Allow limited requests in half-open state
                true
            }
        }
    }

    /// Record a successful request
    pub fn record_success(&self) {
        let state = *self.state.read().unwrap();

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let count = self.success_count.fetch_add(1, Ordering::Relaxed) + 1;
                debug!(
                    success_count = count,
                    threshold = self.config.success_threshold,
                    "HalfOpen success"
                );

                if count >= self.config.success_threshold {
                    self.transition_to_closed();
                }
            }
            CircuitState::Open => {
                // Shouldn't happen, but handle gracefully
            }
        }
    }

    /// Record a failed request
    pub fn record_failure(&self) {
        let state = *self.state.read().unwrap();
        *self.last_failure_time.write().unwrap() = Some(Instant::now());

        match state {
            CircuitState::Closed => {
                let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                debug!(
                    failure_count = count,
                    threshold = self.config.failure_threshold,
                    "Recording failure"
                );

                if count >= self.config.failure_threshold {
                    self.transition_to_open();
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open state opens the circuit again
                self.transition_to_open();
            }
            CircuitState::Open => {
                // Already open, nothing to do
            }
        }
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap()
    }

    fn should_transition_to_half_open(&self) -> bool {
        if let Some(opened_at) = *self.opened_at.read().unwrap() {
            let elapsed = Instant::now().duration_since(opened_at);
            elapsed >= Duration::from_secs(self.config.reset_timeout_secs)
        } else {
            false
        }
    }

    fn transition_to_open(&self) {
        let mut state = self.state.write().unwrap();
        if *state != CircuitState::Open {
            warn!("Circuit breaker OPEN");
            *state = CircuitState::Open;
            *self.opened_at.write().unwrap() = Some(Instant::now());
            self.success_count.store(0, Ordering::Relaxed);
        }
    }

    fn transition_to_half_open(&self) {
        let mut state = self.state.write().unwrap();
        if *state == CircuitState::Open {
            info!("Circuit breaker HALF-OPEN");
            *state = CircuitState::HalfOpen;
            self.success_count.store(0, Ordering::Relaxed);
            self.failure_count.store(0, Ordering::Relaxed);
        }
    }

    fn transition_to_closed(&self) {
        let mut state = self.state.write().unwrap();
        info!("Circuit breaker CLOSED");
        *state = CircuitState::Closed;
        *self.opened_at.write().unwrap() = None;
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
    }
}

/// Manager for multiple circuit breakers (one per origin)
pub struct CircuitBreakerManager {
    breakers: DashMap<String, CircuitBreaker>,
    config: CircuitBreakerConfig,
}

impl CircuitBreakerManager {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: DashMap::new(),
            config,
        }
    }

    /// Get or create a circuit breaker for an origin
    pub fn get_breaker(&self, origin: &str) -> dashmap::mapref::one::Ref<'_, String, CircuitBreaker> {
        if !self.breakers.contains_key(origin) {
            self.breakers
                .insert(origin.to_string(), CircuitBreaker::new(self.config.clone()));
        }
        self.breakers.get(origin).unwrap()
    }

    /// Check if requests to an origin should be allowed
    pub fn should_allow(&self, origin: &str) -> bool {
        self.get_breaker(origin).should_allow()
    }

    /// Record a successful request to an origin
    pub fn record_success(&self, origin: &str) {
        self.get_breaker(origin).record_success();
    }

    /// Record a failed request to an origin
    pub fn record_failure(&self, origin: &str) {
        self.get_breaker(origin).record_failure();
    }

    /// Get the state of a circuit breaker for an origin
    pub fn state(&self, origin: &str) -> CircuitState {
        self.get_breaker(origin).state()
    }

    /// Get states for all origins
    pub fn all_states(&self) -> Vec<(String, CircuitState)> {
        self.breakers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().state()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_closed_to_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            reset_timeout_secs: 1,
            success_threshold: 2,
            failure_window_secs: 60,
        };

        let cb = CircuitBreaker::new(config);

        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.should_allow());

        // Record failures
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.should_allow());
    }

    #[test]
    fn test_circuit_breaker_recovery() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            reset_timeout_secs: 0, // Immediate transition for testing
            success_threshold: 2,
            failure_window_secs: 60,
        };

        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Should transition to half-open immediately (reset_timeout is 0)
        assert!(cb.should_allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Record successes
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            reset_timeout_secs: 0,
            success_threshold: 2,
            failure_window_secs: 60,
        };

        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        cb.record_failure();

        // Transition to half-open
        cb.should_allow();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Failure in half-open should open again
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_manager() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };

        let manager = CircuitBreakerManager::new(config);

        assert!(manager.should_allow("origin1"));
        assert!(manager.should_allow("origin2"));

        manager.record_failure("origin1");
        manager.record_failure("origin1");

        assert!(!manager.should_allow("origin1"));
        assert!(manager.should_allow("origin2"));
    }
}
