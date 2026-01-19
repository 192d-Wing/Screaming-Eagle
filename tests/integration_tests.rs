//! Integration tests for Screaming Eagle CDN

use std::time::Duration;

/// Test that the cache correctly stores and retrieves entries
#[test]
fn test_cache_operations() {
    use bytes::Bytes;
    use screaming_eagle::cache::{Cache, CacheEntry, CacheStatus};
    use screaming_eagle::config::CacheConfig;
    use std::collections::HashMap;
    use std::time::Instant;

    let config = CacheConfig::default();
    let cache = Cache::new(config);

    // Create a test entry
    let body = Bytes::from("Hello, World!");
    let now = Instant::now();
    let entry = CacheEntry {
        body: body.clone(),
        headers: HashMap::new(),
        status_code: 200,
        content_type: Some("text/plain".to_string()),
        etag: Some("\"abc123\"".to_string()),
        last_modified: None,
        created_at: now,
        expires_at: now + Duration::from_secs(3600),
        size: body.len(),
        stale_if_error_secs: Some(300),
        access_count: 0,
        last_accessed: now,
        cache_tags: Vec::new(),
    };

    // Store the entry
    cache.set("test-key".to_string(), entry);

    // Retrieve it
    let result = cache.get("test-key");
    assert!(result.is_some());

    let (retrieved, status) = result.unwrap();
    assert_eq!(status, CacheStatus::Hit);
    assert_eq!(retrieved.body, body);
    assert_eq!(retrieved.status_code, 200);
}

/// Test cache invalidation
#[test]
fn test_cache_invalidation() {
    use bytes::Bytes;
    use screaming_eagle::cache::Cache;
    use screaming_eagle::config::CacheConfig;
    use std::collections::HashMap;
    use std::time::Instant;

    let config = CacheConfig::default();
    let cache = Cache::new(config);

    // Create and store entry
    let body = Bytes::from("Test data");
    let now = Instant::now();
    let entry = screaming_eagle::cache::CacheEntry {
        body,
        headers: HashMap::new(),
        status_code: 200,
        content_type: None,
        etag: None,
        last_modified: None,
        created_at: now,
        expires_at: now + Duration::from_secs(3600),
        size: 9,
        stale_if_error_secs: None,
        access_count: 0,
        last_accessed: now,
        cache_tags: Vec::new(),
    };

    cache.set("key1".to_string(), entry.clone());
    cache.set("key2".to_string(), entry);

    // Verify entries exist
    assert!(cache.get("key1").is_some());
    assert!(cache.get("key2").is_some());

    // Invalidate one entry
    assert!(cache.invalidate("key1"));
    assert!(cache.get("key1").is_none());
    assert!(cache.get("key2").is_some());

    // Purge all
    let count = cache.purge_all();
    assert_eq!(count, 1);
    assert!(cache.get("key2").is_none());
}

/// Test rate limiter
#[test]
fn test_rate_limiter() {
    use screaming_eagle::rate_limit::{RateLimitConfig, RateLimitResult, RateLimiter};
    use std::net::{IpAddr, Ipv4Addr};

    let config = RateLimitConfig {
        requests_per_window: 10,
        window_secs: 60,
        burst_size: 5,
        enabled: true,
    };

    let limiter = RateLimiter::new(config);
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    // Should allow initial requests (10 + 5 burst = 15)
    for i in 0..15 {
        match limiter.check(ip) {
            RateLimitResult::Allowed { .. } => {}
            RateLimitResult::Limited { .. } => panic!("Request {} should not be limited", i),
        }
    }

    // Next request should be limited
    match limiter.check(ip) {
        RateLimitResult::Allowed { .. } => panic!("Should be rate limited"),
        RateLimitResult::Limited { retry_after } => {
            assert!(retry_after > 0);
        }
    }
}

/// Test rate limiter when disabled
#[test]
fn test_rate_limiter_disabled() {
    use screaming_eagle::rate_limit::{RateLimitConfig, RateLimitResult, RateLimiter};
    use std::net::{IpAddr, Ipv4Addr};

    let config = RateLimitConfig {
        enabled: false,
        ..Default::default()
    };

    let limiter = RateLimiter::new(config);
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    // Should always allow when disabled
    for _ in 0..100 {
        match limiter.check(ip) {
            RateLimitResult::Allowed { remaining, .. } => {
                assert_eq!(remaining, u32::MAX);
            }
            RateLimitResult::Limited { .. } => panic!("Should not be limited when disabled"),
        }
    }
}

/// Test circuit breaker state transitions
#[test]
fn test_circuit_breaker() {
    use screaming_eagle::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        reset_timeout_secs: 0, // Immediate for testing
        success_threshold: 2,
        failure_window_secs: 60,
    };

    let cb = CircuitBreaker::new(config);

    // Initially closed
    assert_eq!(cb.state(), CircuitState::Closed);
    assert!(cb.should_allow());

    // Record failures
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Closed);

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // With timeout=0, should immediately transition to half-open
    assert!(cb.should_allow());
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    // Record successes to close
    cb.record_success();
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
}

/// Test cache control parsing
#[test]
fn test_cache_control_parsing() {
    use screaming_eagle::cache::parse_cache_control;

    let directives = parse_cache_control("max-age=3600, public");
    assert_eq!(directives.max_age, Some(3600));
    assert!(directives.public);
    assert!(!directives.private);
    assert!(directives.is_cacheable());

    let directives = parse_cache_control("no-store");
    assert!(directives.no_store);
    assert!(!directives.is_cacheable());

    let directives = parse_cache_control("private, max-age=600");
    assert!(directives.private);
    assert!(!directives.is_cacheable());

    let directives = parse_cache_control("s-maxage=300, max-age=600");
    assert_eq!(directives.s_maxage, Some(300));
    assert_eq!(directives.max_age, Some(600));

    // Test stale-while-revalidate and stale-if-error (RFC 5861)
    let directives =
        parse_cache_control("max-age=300, stale-while-revalidate=60, stale-if-error=86400");
    assert_eq!(directives.max_age, Some(300));
    assert_eq!(directives.stale_while_revalidate, Some(60));
    assert_eq!(directives.stale_if_error, Some(86400));
}

/// Test cache key generation
#[test]
fn test_cache_key_generation() {
    use screaming_eagle::cache::generate_cache_key;

    assert_eq!(
        generate_cache_key("origin1", "/path/to/resource", None),
        "origin1/path/to/resource"
    );

    assert_eq!(
        generate_cache_key("origin1", "/path", Some("foo=bar&baz=qux")),
        "origin1/path?foo=bar&baz=qux"
    );

    assert_eq!(
        generate_cache_key("origin1", "/path", Some("")),
        "origin1/path"
    );
}
