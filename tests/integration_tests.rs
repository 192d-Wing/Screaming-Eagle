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

/// Test degradation manager fallback handling
#[test]
fn test_degradation_fallbacks() {
    use screaming_eagle::degradation::{DegradationConfig, DegradationManager, FallbackRule};
    use std::collections::HashMap;

    let config = DegradationConfig {
        enabled: true,
        serve_stale_on_error: true,
        max_stale_age_secs: 3600,
        fallbacks: vec![
            FallbackRule {
                path_prefix: "/api/".to_string(),
                status: 503,
                body: r#"{"error":"service unavailable"}"#.to_string(),
                content_type: "application/json".to_string(),
                headers: HashMap::new(),
            },
            FallbackRule {
                path_prefix: "/static/".to_string(),
                status: 200,
                body: "Fallback content".to_string(),
                content_type: "text/plain".to_string(),
                headers: HashMap::new(),
            },
        ],
        backup_origins: HashMap::new(),
        reduced_mode_threshold: 5,
        reduced_mode_recovery_secs: 60,
    };

    let manager = DegradationManager::new(config);

    // Test fallback matching
    let api_fallback = manager.find_fallback("/api/users");
    assert!(api_fallback.is_some());
    let fb = api_fallback.unwrap();
    assert_eq!(fb.status.as_u16(), 503);

    let static_fallback = manager.find_fallback("/static/image.png");
    assert!(static_fallback.is_some());
    let fb = static_fallback.unwrap();
    assert_eq!(fb.status.as_u16(), 200);

    // No fallback for unmatched paths
    assert!(manager.find_fallback("/other/path").is_none());
}

/// Test degradation reduced mode
#[test]
fn test_degradation_reduced_mode() {
    use screaming_eagle::degradation::{DegradationConfig, DegradationManager};

    let config = DegradationConfig {
        enabled: true,
        reduced_mode_threshold: 3,
        ..Default::default()
    };

    let manager = DegradationManager::new(config);

    // Initially not in reduced mode
    assert!(!manager.is_reduced_mode("test-origin"));

    // Record failures
    manager.record_failure("test-origin");
    manager.record_failure("test-origin");
    assert!(!manager.is_reduced_mode("test-origin"));

    manager.record_failure("test-origin");
    assert!(manager.is_reduced_mode("test-origin"));

    // Success resets failure count but doesn't immediately exit reduced mode
    manager.record_success("test-origin");
    let status = manager.status();
    assert_eq!(status.origins.get("test-origin").unwrap().failures, 0);
}

/// Test consistent hash ring distribution
#[tokio::test]
async fn test_hash_ring_distribution() {
    use screaming_eagle::distributed::HashRing;
    use std::collections::HashMap;

    let ring = HashRing::new(100); // 100 virtual nodes per physical node

    ring.add_node("node-a").await;
    ring.add_node("node-b").await;
    ring.add_node("node-c").await;

    assert_eq!(ring.node_count().await, 3);

    // Test distribution across many keys
    let mut counts: HashMap<String, usize> = HashMap::new();
    for i in 0..3000 {
        let key = format!("cache-key-{}", i);
        if let Some(node) = ring.get_node(&key).await {
            *counts.entry(node).or_insert(0) += 1;
        }
    }

    // Each node should get roughly 1000 keys (1/3 of 3000)
    // Allow 30% variance for randomness
    for (node, count) in &counts {
        assert!(
            *count >= 700 && *count <= 1300,
            "Node {} has {} keys, expected ~1000",
            node,
            count
        );
    }
}

/// Test hash ring consistency after node removal
#[tokio::test]
async fn test_hash_ring_consistency() {
    use screaming_eagle::distributed::HashRing;

    let ring = HashRing::new(100);

    ring.add_node("node-a").await;
    ring.add_node("node-b").await;
    ring.add_node("node-c").await;

    // Record initial assignments
    let mut initial_assignments: Vec<(String, String)> = Vec::new();
    for i in 0..100 {
        let key = format!("key-{}", i);
        if let Some(node) = ring.get_node(&key).await {
            initial_assignments.push((key, node));
        }
    }

    // Remove one node
    ring.remove_node("node-b").await;
    assert_eq!(ring.node_count().await, 2);

    // Keys that were on node-a or node-c should stay there
    let mut moved_count = 0;
    for (key, original_node) in &initial_assignments {
        if *original_node != "node-b" {
            if let Some(new_node) = ring.get_node(key).await {
                if new_node != *original_node {
                    moved_count += 1;
                }
            }
        }
    }

    // Very few keys should have moved (consistent hashing property)
    // Only keys from node-b should redistribute
    assert!(moved_count < 5, "Too many keys moved: {}", moved_count);
}

/// Test distributed invalidation deduplication
#[tokio::test]
async fn test_invalidation_deduplication() {
    use screaming_eagle::distributed::{Invalidation, InvalidationManager};

    let (manager, _rx) = InvalidationManager::new("node-1".to_string());

    // Create an invalidation
    let msg = manager.invalidate(Invalidation::Key("/api/users".to_string())).await;

    // Same message should be deduplicated
    let accepted = manager.receive(msg.clone()).await;
    assert!(!accepted, "Duplicate message should be rejected");

    // Different message should be accepted
    let msg2 = screaming_eagle::distributed::InvalidationMessage {
        id: "node-2-1".to_string(),
        origin_node: "node-2".to_string(),
        timestamp: 12345,
        invalidation: Invalidation::Prefix("/api/".to_string()),
    };
    let accepted = manager.receive(msg2).await;
    assert!(accepted, "New message should be accepted");
}

/// Test shadow manager rule matching
#[test]
fn test_shadow_rule_matching() {
    use screaming_eagle::shadow::{ShadowConfig, ShadowManager, ShadowRule};
    use std::collections::HashMap;

    let config = ShadowConfig {
        enabled: true,
        rules: vec![
            ShadowRule {
                name: "api-shadow".to_string(),
                primary_origin: "prod".to_string(),
                shadow_url: "http://staging.example.com".to_string(),
                sample_percent: 100,
                include_paths: vec!["^/api/".to_string()],
                exclude_paths: vec!["^/api/health".to_string()],
                methods: vec!["GET".to_string(), "POST".to_string()],
                forward_headers: vec!["authorization".to_string()],
                add_headers: HashMap::new(),
            },
        ],
        max_concurrent: 10,
        timeout_secs: 5,
        log_differences: false,
    };

    let manager = ShadowManager::new(config);
    assert!(manager.is_enabled());

    // Should match API paths
    let rules = manager.find_rules("prod", "/api/users", &http::Method::GET);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].name, "api-shadow");

    // Should exclude health endpoint
    let rules = manager.find_rules("prod", "/api/health", &http::Method::GET);
    assert!(rules.is_empty());

    // Should not match different origin
    let rules = manager.find_rules("staging", "/api/users", &http::Method::GET);
    assert!(rules.is_empty());

    // Should not match different method
    let rules = manager.find_rules("prod", "/api/users", &http::Method::DELETE);
    assert!(rules.is_empty());
}

/// Test rate limiter config update (hot-reload)
#[test]
fn test_rate_limiter_hot_reload() {
    use screaming_eagle::rate_limit::{RateLimitConfig, RateLimitResult, RateLimiter};
    use std::net::{IpAddr, Ipv4Addr};

    let config = RateLimitConfig {
        requests_per_window: 5,
        window_secs: 60,
        burst_size: 2,
        enabled: true,
    };

    let limiter = RateLimiter::new(config);
    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    // Use up initial tokens (5 + 2 = 7)
    for _ in 0..7 {
        match limiter.check(ip) {
            RateLimitResult::Allowed { .. } => {}
            RateLimitResult::Limited { .. } => panic!("Should be allowed"),
        }
    }

    // Should be limited now
    match limiter.check(ip) {
        RateLimitResult::Allowed { .. } => panic!("Should be limited"),
        RateLimitResult::Limited { .. } => {}
    }

    // Hot-reload with higher limits
    limiter.update_config(100, 60, 50);

    // New client should get higher limits
    let ip2 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
    for _ in 0..150 {
        match limiter.check(ip2) {
            RateLimitResult::Allowed { .. } => {}
            RateLimitResult::Limited { .. } => panic!("Should be allowed with new config"),
        }
    }
}
