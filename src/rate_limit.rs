use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum requests per window
    pub requests_per_window: u32,
    /// Window duration
    pub window_secs: u64,
    /// Burst allowance (extra requests allowed in short bursts)
    pub burst_size: u32,
    /// Whether rate limiting is enabled
    pub enabled: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_window: 1000,
            window_secs: 60,
            burst_size: 50,
            enabled: true,
        }
    }
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_update: Instant,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            last_update: Instant::now(),
            max_tokens,
            refill_rate,
        }
    }

    fn try_consume(&mut self, tokens: f64) -> bool {
        self.refill();

        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_update = now;
    }

    fn tokens_available(&mut self) -> f64 {
        self.refill();
        self.tokens
    }
}

pub struct RateLimiter {
    buckets: DashMap<IpAddr, TokenBucket>,
    requests_per_window: AtomicU32,
    window_secs: AtomicU64,
    burst_size: AtomicU32,
    enabled: AtomicBool,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            buckets: DashMap::new(),
            requests_per_window: AtomicU32::new(config.requests_per_window),
            window_secs: AtomicU64::new(config.window_secs),
            burst_size: AtomicU32::new(config.burst_size),
            enabled: AtomicBool::new(config.enabled),
        }
    }

    /// Update configuration at runtime (for hot-reload)
    pub fn update_config(&self, requests_per_window: u32, window_secs: u64, burst_size: u32) {
        self.requests_per_window.store(requests_per_window, Ordering::SeqCst);
        self.window_secs.store(window_secs, Ordering::SeqCst);
        self.burst_size.store(burst_size, Ordering::SeqCst);
        info!(
            requests_per_window,
            window_secs,
            burst_size,
            "Rate limiter config updated"
        );
    }

    pub fn check(&self, ip: IpAddr) -> RateLimitResult {
        if !self.enabled.load(Ordering::Relaxed) {
            return RateLimitResult::Allowed {
                remaining: u32::MAX,
                reset_secs: 0,
            };
        }

        let requests_per_window = self.requests_per_window.load(Ordering::Relaxed);
        let window_secs = self.window_secs.load(Ordering::Relaxed);
        let burst_size = self.burst_size.load(Ordering::Relaxed);

        let max_tokens = requests_per_window as f64 + burst_size as f64;
        let refill_rate = requests_per_window as f64 / window_secs as f64;

        let mut bucket = self
            .buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(max_tokens, refill_rate));

        if bucket.try_consume(1.0) {
            let remaining = bucket.tokens_available() as u32;
            let reset_secs = if remaining == 0 {
                (1.0 / refill_rate).ceil() as u64
            } else {
                0
            };

            debug!(ip = %ip, remaining = remaining, "Rate limit check passed");

            RateLimitResult::Allowed {
                remaining,
                reset_secs,
            }
        } else {
            let retry_after = ((1.0 - bucket.tokens_available()) / refill_rate).ceil() as u64;

            warn!(ip = %ip, retry_after = retry_after, "Rate limit exceeded");

            RateLimitResult::Limited { retry_after }
        }
    }

    /// Clean up old entries that haven't been used recently
    pub fn cleanup(&self, max_age: Duration) {
        let now = Instant::now();
        self.buckets
            .retain(|_, bucket| now.duration_since(bucket.last_update) < max_age);
    }
}

#[derive(Debug)]
pub enum RateLimitResult {
    Allowed { remaining: u32, reset_secs: u64 },
    Limited { retry_after: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_token_bucket() {
        let mut bucket = TokenBucket::new(10.0, 1.0);

        // Should be able to consume initial tokens
        assert!(bucket.try_consume(5.0));
        assert!(bucket.try_consume(5.0));

        // Should be empty now
        assert!(!bucket.try_consume(1.0));
    }

    #[test]
    fn test_rate_limiter() {
        let config = RateLimitConfig {
            requests_per_window: 10,
            window_secs: 60,
            burst_size: 5,
            enabled: true,
        };

        let limiter = RateLimiter::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Should allow initial burst
        for _ in 0..15 {
            match limiter.check(ip) {
                RateLimitResult::Allowed { .. } => {}
                RateLimitResult::Limited { .. } => panic!("Should not be limited yet"),
            }
        }

        // Should be limited now
        match limiter.check(ip) {
            RateLimitResult::Allowed { .. } => panic!("Should be limited"),
            RateLimitResult::Limited { retry_after } => {
                assert!(retry_after > 0);
            }
        }
    }

    #[test]
    fn test_disabled_rate_limiter() {
        let config = RateLimitConfig {
            enabled: false,
            ..Default::default()
        };

        let limiter = RateLimiter::new(config);
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // Should always allow when disabled
        for _ in 0..1000 {
            match limiter.check(ip) {
                RateLimitResult::Allowed { .. } => {}
                RateLimitResult::Limited { .. } => panic!("Should not be limited when disabled"),
            }
        }
    }
}
