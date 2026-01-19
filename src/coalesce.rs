//! Request coalescing module
//!
//! Prevents the "thundering herd" problem by deduplicating concurrent requests
//! for the same resource. When multiple requests arrive for an uncached resource,
//! only one request is sent to the origin and all waiters receive the same response.

use bytes::Bytes;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info};

/// Result of a coalesced request
#[derive(Debug, Clone)]
pub struct CoalescedResponse {
    pub body: Bytes,
    pub headers: HashMap<String, String>,
    pub status_code: u16,
}

/// Internal state for the coalescer
struct CoalescerInner {
    /// Map of cache keys to broadcast channels for in-flight requests
    in_flight: DashMap<String, broadcast::Sender<Result<CoalescedResponse, String>>>,
    /// Maximum number of waiters per request
    max_waiters: usize,
}

/// Manages in-flight requests to prevent duplicate origin fetches
#[derive(Clone)]
pub struct RequestCoalescer {
    inner: Arc<CoalescerInner>,
}

impl RequestCoalescer {
    pub fn new(max_waiters: usize) -> Self {
        Self {
            inner: Arc::new(CoalescerInner {
                in_flight: DashMap::new(),
                max_waiters,
            }),
        }
    }

    /// Try to acquire the right to fetch from origin.
    /// Returns Ok(None) if this request should fetch from origin.
    /// Returns Ok(Some(receiver)) if another request is already fetching.
    pub fn try_acquire(&self, cache_key: &str) -> AcquireResult {
        // Check if there's already an in-flight request
        if let Some(sender) = self.inner.in_flight.get(cache_key) {
            // Subscribe to the existing request
            let receiver = sender.subscribe();
            debug!(cache_key = %cache_key, "Coalescing request with in-flight fetch");
            return AcquireResult::Wait(receiver);
        }

        // No in-flight request, create a new broadcast channel
        let (tx, _) = broadcast::channel(self.inner.max_waiters);
        self.inner.in_flight.insert(cache_key.to_string(), tx);

        debug!(cache_key = %cache_key, "Acquired origin fetch lock");
        AcquireResult::Fetch(FetchGuard {
            cache_key: cache_key.to_string(),
            inner: Arc::clone(&self.inner),
        })
    }

    /// Get statistics about current in-flight requests
    pub fn stats(&self) -> CoalesceStats {
        let in_flight_count = self.inner.in_flight.len();
        let total_waiters: usize = self
            .inner
            .in_flight
            .iter()
            .map(|entry| entry.value().receiver_count())
            .sum();

        CoalesceStats {
            in_flight_requests: in_flight_count,
            total_waiters,
        }
    }
}

/// Result of trying to acquire a fetch lock
pub enum AcquireResult {
    /// This request should fetch from origin
    Fetch(FetchGuard),
    /// Another request is fetching, wait for result
    Wait(broadcast::Receiver<Result<CoalescedResponse, String>>),
}

/// Guard that ensures we notify waiters when the fetch completes
pub struct FetchGuard {
    cache_key: String,
    inner: Arc<CoalescerInner>,
}

impl FetchGuard {
    /// Complete the fetch with a successful response
    pub fn complete(self, response: CoalescedResponse) {
        self.complete_internal(Ok(response));
    }

    /// Complete the fetch with an error
    pub fn complete_error(self, error: String) {
        self.complete_internal(Err(error));
    }

    fn complete_internal(self, result: Result<CoalescedResponse, String>) {
        if let Some((_, sender)) = self.inner.in_flight.remove(&self.cache_key) {
            let waiter_count = sender.receiver_count();
            if waiter_count > 0 {
                info!(
                    cache_key = %self.cache_key,
                    waiters = waiter_count,
                    "Notifying coalesced request waiters"
                );
            }
            // Send result to all waiters (ignore errors if no receivers)
            let _ = sender.send(result);
        }
        // Prevent Drop from running
        std::mem::forget(self);
    }
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        // If the guard is dropped without completing, remove the in-flight entry
        // This handles panics or early returns
        if let Some((_, sender)) = self.inner.in_flight.remove(&self.cache_key) {
            let _ = sender.send(Err("Request was cancelled".to_string()));
        }
    }
}

/// Statistics about request coalescing
#[derive(Debug, Clone, serde::Serialize)]
pub struct CoalesceStats {
    pub in_flight_requests: usize,
    pub total_waiters: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_single_request() {
        let coalescer = RequestCoalescer::new(100);

        // First request
        match coalescer.try_acquire("test-key") {
            AcquireResult::Fetch(guard) => {
                guard.complete(CoalescedResponse {
                    body: Bytes::from("hello"),
                    headers: HashMap::new(),
                    status_code: 200,
                });
            }
            AcquireResult::Wait(_) => panic!("Should have acquired fetch lock"),
        }

        // After completion, a new request should get a fresh fetch lock
        match coalescer.try_acquire("test-key") {
            AcquireResult::Fetch(guard) => {
                guard.complete(CoalescedResponse {
                    body: Bytes::from("hello2"),
                    headers: HashMap::new(),
                    status_code: 200,
                });
            }
            AcquireResult::Wait(_) => panic!("Should have acquired fetch lock"),
        }
    }

    #[tokio::test]
    async fn test_coalesced_requests() {
        let coalescer = RequestCoalescer::new(100);

        // First request acquires lock
        let guard = match coalescer.try_acquire("test-key") {
            AcquireResult::Fetch(guard) => guard,
            AcquireResult::Wait(_) => panic!("Should have acquired fetch lock"),
        };

        // Second request should wait
        let mut receiver = match coalescer.try_acquire("test-key") {
            AcquireResult::Wait(rx) => rx,
            AcquireResult::Fetch(_) => panic!("Should have waited"),
        };

        // Third request should also wait
        let mut receiver2 = match coalescer.try_acquire("test-key") {
            AcquireResult::Wait(rx) => rx,
            AcquireResult::Fetch(_) => panic!("Should have waited"),
        };

        // Complete the first request
        guard.complete(CoalescedResponse {
            body: Bytes::from("shared response"),
            headers: HashMap::new(),
            status_code: 200,
        });

        // Both waiters should receive the response
        let result1 = receiver.recv().await.unwrap().unwrap();
        let result2 = receiver2.recv().await.unwrap().unwrap();

        assert_eq!(result1.body, Bytes::from("shared response"));
        assert_eq!(result2.body, Bytes::from("shared response"));
    }

    #[tokio::test]
    async fn test_error_propagation() {
        let coalescer = RequestCoalescer::new(100);

        let guard = match coalescer.try_acquire("test-key") {
            AcquireResult::Fetch(guard) => guard,
            AcquireResult::Wait(_) => panic!("Should have acquired fetch lock"),
        };

        let mut receiver = match coalescer.try_acquire("test-key") {
            AcquireResult::Wait(rx) => rx,
            AcquireResult::Fetch(_) => panic!("Should have waited"),
        };

        guard.complete_error("origin error".to_string());

        let result = receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "origin error");
    }

    #[test]
    fn test_stats() {
        let coalescer = RequestCoalescer::new(100);

        let stats = coalescer.stats();
        assert_eq!(stats.in_flight_requests, 0);
        assert_eq!(stats.total_waiters, 0);

        // Acquire a lock and keep it
        let guard = match coalescer.try_acquire("test-key") {
            AcquireResult::Fetch(guard) => guard,
            AcquireResult::Wait(_) => panic!("Should have acquired fetch lock"),
        };

        let stats = coalescer.stats();
        assert_eq!(stats.in_flight_requests, 1);

        // Complete the guard to clean up
        guard.complete(CoalescedResponse {
            body: Bytes::from("test"),
            headers: HashMap::new(),
            status_code: 200,
        });
    }
}
