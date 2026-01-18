use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use xxhash_rust::xxh3::xxh3_64;

use crate::cache::{
    generate_cache_key_with_vary, parse_cache_control, Cache, CacheEntry, CacheStats, CacheStatus,
};
use crate::circuit_breaker::{CircuitBreakerManager, CircuitState};
use crate::coalesce::{AcquireResult, CoalescedResponse, CoalesceStats, RequestCoalescer};
use crate::config::Config;
use crate::error::{CdnError, CdnResult};
use crate::health::{HealthChecker, OriginHealth};
use crate::metrics::Metrics;
use crate::origin::OriginFetcher;
use crate::range::{extract_range, parse_range_header, ByteRange, RangeParseResult};
use crate::rate_limit::{RateLimitResult, RateLimiter};

pub struct AppState {
    pub cache: Arc<Cache>,
    pub origin: Arc<OriginFetcher>,
    pub config: Arc<Config>,
    pub metrics: Arc<Metrics>,
    pub rate_limiter: Arc<RateLimiter>,
    pub circuit_breaker: Arc<CircuitBreakerManager>,
    pub health_checker: Arc<HealthChecker>,
    pub coalescer: Arc<RequestCoalescer>,
    pub coalesce_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct CdnQuery {
    #[serde(flatten)]
    pub params: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct PurgeResponse {
    pub success: bool,
    pub message: String,
    pub purged_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct PurgeRequest {
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct CircuitBreakerStatusResponse {
    pub origins: Vec<OriginCircuitStatus>,
}

#[derive(Debug, Serialize)]
pub struct OriginCircuitStatus {
    pub origin: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct OriginHealthResponse {
    pub origins: HashMap<String, OriginHealth>,
}

#[derive(Debug, Serialize)]
pub struct CoalesceStatsResponse {
    pub enabled: bool,
    #[serde(flatten)]
    pub stats: CoalesceStats,
}

#[derive(Debug, Deserialize)]
pub struct WarmCacheRequest {
    /// List of URLs to warm (relative paths like "/origin/path")
    pub urls: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WarmCacheResponse {
    pub success: bool,
    pub message: String,
    pub warmed: usize,
    pub failed: usize,
    pub results: Vec<WarmResult>,
}

#[derive(Debug, Serialize)]
pub struct WarmResult {
    pub url: String,
    pub success: bool,
    pub cached: bool,
    pub error: Option<String>,
}

// Health check endpoint
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// Cache statistics endpoint
pub async fn cache_stats(State(state): State<Arc<AppState>>) -> Json<CacheStats> {
    Json(state.cache.stats())
}

// Metrics endpoint (Prometheus format)
pub async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let metrics = state.metrics.gather();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics,
    )
}

// Cache purge endpoint
pub async fn purge_cache(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PurgeRequest>,
) -> Json<PurgeResponse> {
    let purged_count = if request.all {
        state.cache.purge_all()
    } else if let Some(prefix) = request.prefix {
        state.cache.invalidate_prefix(&prefix)
    } else {
        let mut count = 0;
        for key in &request.keys {
            if state.cache.invalidate(key) {
                count += 1;
            }
        }
        count
    };

    Json(PurgeResponse {
        success: true,
        message: format!("Purged {} cache entries", purged_count),
        purged_count,
    })
}

// Circuit breaker status endpoint
pub async fn circuit_breaker_status(
    State(state): State<Arc<AppState>>,
) -> Json<CircuitBreakerStatusResponse> {
    let origins = state
        .circuit_breaker
        .all_states()
        .into_iter()
        .map(|(origin, state)| OriginCircuitStatus {
            origin,
            state: match state {
                CircuitState::Closed => "closed".to_string(),
                CircuitState::Open => "open".to_string(),
                CircuitState::HalfOpen => "half-open".to_string(),
            },
        })
        .collect();

    Json(CircuitBreakerStatusResponse { origins })
}

// Origin health status endpoint
pub async fn origin_health_status(
    State(state): State<Arc<AppState>>,
) -> Json<OriginHealthResponse> {
    let origins = state.health_checker.get_all_statuses();
    Json(OriginHealthResponse { origins })
}

// Coalesce statistics endpoint
pub async fn coalesce_stats(
    State(state): State<Arc<AppState>>,
) -> Json<CoalesceStatsResponse> {
    Json(CoalesceStatsResponse {
        enabled: state.coalesce_enabled,
        stats: state.coalescer.stats(),
    })
}

// Cache warming endpoint - preload content into cache
pub async fn warm_cache(
    State(state): State<Arc<AppState>>,
    Json(request): Json<WarmCacheRequest>,
) -> Json<WarmCacheResponse> {
    let mut results = Vec::with_capacity(request.urls.len());
    let mut warmed = 0;
    let mut failed = 0;

    for url in &request.urls {
        // Parse URL to extract origin and path
        // Expected format: "/origin/path" or "origin/path"
        let url = url.trim_start_matches('/');
        let parts: Vec<&str> = url.splitn(2, '/').collect();

        if parts.is_empty() {
            results.push(WarmResult {
                url: url.to_string(),
                success: false,
                cached: false,
                error: Some("Invalid URL format".to_string()),
            });
            failed += 1;
            continue;
        }

        let (origin, path) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            // Try default origin if only one configured
            let origins = state.origin.origin_names();
            if origins.len() == 1 {
                (origins[0], parts[0])
            } else {
                results.push(WarmResult {
                    url: url.to_string(),
                    success: false,
                    cached: false,
                    error: Some("Origin must be specified: /origin/path".to_string()),
                });
                failed += 1;
                continue;
            }
        };

        // Check if origin exists
        if !state.origin.has_origin(origin) {
            results.push(WarmResult {
                url: url.to_string(),
                success: false,
                cached: false,
                error: Some(format!("Unknown origin: {}", origin)),
            });
            failed += 1;
            continue;
        }

        // Generate cache key
        let cache_key = generate_cache_key_with_vary(
            origin,
            &format!("/{}", path),
            None,
            Some("accept-encoding"),
            &HashMap::new(),
        );

        // Check if already cached
        if state.cache.get(&cache_key).is_some() {
            results.push(WarmResult {
                url: url.to_string(),
                success: true,
                cached: true,
                error: None,
            });
            warmed += 1;
            continue;
        }

        // Fetch from origin
        match fetch_from_origin(&state, origin, path, None, &HeaderMap::new()).await {
            Ok((body, headers, status)) => {
                if is_cacheable(status, &headers) {
                    // Store in cache
                    let vary_header = headers.get("vary").map(|s| s.as_str());
                    let final_cache_key = generate_cache_key_with_vary(
                        origin,
                        &format!("/{}", path),
                        None,
                        vary_header.or(Some("accept-encoding")),
                        &HashMap::new(),
                    );
                    store_in_cache(&state, &final_cache_key, body, headers, status);

                    results.push(WarmResult {
                        url: url.to_string(),
                        success: true,
                        cached: false,
                        error: None,
                    });
                    warmed += 1;
                } else {
                    results.push(WarmResult {
                        url: url.to_string(),
                        success: false,
                        cached: false,
                        error: Some("Response not cacheable".to_string()),
                    });
                    failed += 1;
                }
            }
            Err(e) => {
                results.push(WarmResult {
                    url: url.to_string(),
                    success: false,
                    cached: false,
                    error: Some(e.to_string()),
                });
                failed += 1;
            }
        }
    }

    Json(WarmCacheResponse {
        success: failed == 0,
        message: format!("Warmed {} URLs, {} failed", warmed, failed),
        warmed,
        failed,
        results,
    })
}

// Main CDN handler - supports both GET and HEAD methods
pub async fn cdn_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    method: Method,
    Path((origin, path)): Path<(String, String)>,
    Query(query): Query<CdnQuery>,
    headers: HeaderMap,
) -> Result<Response, CdnError> {
    let start = Instant::now();
    let is_head_request = method == Method::HEAD;

    // Check rate limit
    let client_ip = extract_client_ip(&headers, addr.ip());
    match state.rate_limiter.check(client_ip) {
        RateLimitResult::Limited { retry_after } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
            )
                .into_response();

            response
                .headers_mut()
                .insert("Retry-After", retry_after.to_string().parse().unwrap());
            response
                .headers_mut()
                .insert("X-RateLimit-Remaining", "0".parse().unwrap());

            return Ok(response);
        }
        RateLimitResult::Allowed { remaining, .. } => {
            // Will add header to response later
            let _ = remaining;
        }
    }

    // Validate origin exists
    if !state.origin.has_origin(&origin) {
        return Err(CdnError::NotFound(format!("Unknown origin: {}", origin)));
    }

    // Check circuit breaker
    if !state.circuit_breaker.should_allow(&origin) {
        return Err(CdnError::OriginUnreachable(format!(
            "Origin {} circuit breaker is open",
            origin
        )));
    }

    // Build query string
    let query_string = if query.params.is_empty() {
        None
    } else {
        Some(
            query
                .params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&"),
        )
    };

    // Extract request headers for Vary-based cache keying (RFC 9111)
    let request_headers_map = extract_request_headers(&headers);

    // Check request cache control
    let bypass_cache = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("no-cache") || v.contains("no-store"))
        .unwrap_or(false);

    let mut cache_status;
    let response_body;
    let response_headers;
    let response_status;
    let mut cache_age_secs: Option<u64> = None;

    if bypass_cache {
        // Client requested bypass
        cache_status = CacheStatus::Bypass;
        match fetch_from_origin_with_circuit_breaker(&state, &origin, &path, query_string.as_deref(), &headers).await {
            Ok(origin_response) => {
                response_body = origin_response.0;
                response_headers = origin_response.1;
                response_status = origin_response.2;
            }
            Err(e) => return Err(e),
        }
    } else {
        // Generate Vary-aware cache key using common Vary headers (Accept-Encoding)
        // This ensures different compression variants are cached separately
        let cache_key = generate_cache_key_with_vary(
            &origin,
            &format!("/{}", path),
            query_string.as_deref(),
            Some("accept-encoding"), // Default Vary for compression support
            &request_headers_map,
        );

        // Try cache first
        match state.cache.get(&cache_key) {
            Some((entry, status)) => {
                cache_status = status;
                // Calculate Age header value (RFC 9111)
                cache_age_secs = Some(entry.created_at.elapsed().as_secs());
                response_body = entry.body;
                response_headers = entry.headers;
                response_status = StatusCode::from_u16(entry.status_code).unwrap_or(StatusCode::OK);

                // If stale, trigger background revalidation
                if status == CacheStatus::Stale {
                    let state_clone = state.clone();
                    let origin_clone = origin.clone();
                    let path_clone = path.clone();
                    let query_clone = query_string.clone();
                    let request_headers_clone = request_headers_map.clone();

                    tokio::spawn(async move {
                        if let Ok((body, headers, status)) = fetch_from_origin_with_circuit_breaker(
                            &state_clone,
                            &origin_clone,
                            &path_clone,
                            query_clone.as_deref(),
                            &HeaderMap::new(),
                        )
                        .await
                        {
                            // Generate cache key with actual Vary header from response
                            let vary_header = headers.get("vary").map(|s| s.as_str());
                            let final_cache_key = generate_cache_key_with_vary(
                                &origin_clone,
                                &format!("/{}", path_clone),
                                query_clone.as_deref(),
                                vary_header.or(Some("accept-encoding")),
                                &request_headers_clone,
                            );
                            store_in_cache(&state_clone, &final_cache_key, body, headers, status);
                        }
                    });
                }
            }
            None => {
                // Cache miss - fetch from origin (with optional coalescing)
                cache_status = CacheStatus::Miss;

                // Use coalescing to prevent thundering herd
                let fetch_result = if state.coalesce_enabled {
                    match state.coalescer.try_acquire(&cache_key) {
                        AcquireResult::Fetch(guard) => {
                            // We are the leader - fetch from origin
                            match fetch_from_origin_with_circuit_breaker(&state, &origin, &path, query_string.as_deref(), &headers).await {
                                Ok((body, hdrs, status)) => {
                                    // Complete the coalesce to notify waiters
                                    guard.complete(CoalescedResponse {
                                        body: body.clone(),
                                        headers: hdrs.clone(),
                                        status_code: status.as_u16(),
                                    });
                                    Ok((body, hdrs, status))
                                }
                                Err(e) => {
                                    // Complete with error to notify waiters
                                    guard.complete_error(e.to_string());
                                    Err(e)
                                }
                            }
                        }
                        AcquireResult::Wait(mut receiver) => {
                            // Another request is already fetching - wait for result
                            tracing::debug!(cache_key = %cache_key, "Waiting for coalesced request");
                            match receiver.recv().await {
                                Ok(Ok(coalesced)) => {
                                    let status = StatusCode::from_u16(coalesced.status_code)
                                        .unwrap_or(StatusCode::OK);
                                    Ok((coalesced.body, coalesced.headers, status))
                                }
                                Ok(Err(err)) => Err(CdnError::OriginError(err)),
                                Err(_) => Err(CdnError::Internal("Coalesced request was cancelled".to_string())),
                            }
                        }
                    }
                } else {
                    // Coalescing disabled - direct fetch
                    fetch_from_origin_with_circuit_breaker(&state, &origin, &path, query_string.as_deref(), &headers).await
                };

                match fetch_result {
                    Ok(origin_response) => {
                        // Check if origin returned 5xx error - try stale-if-error
                        if origin_response.2.is_server_error() {
                            // RFC 5861: Try to serve stale content on 5xx errors
                            if let Some(stale_entry) = state.cache.get_stale_for_error(&cache_key) {
                                cache_status = CacheStatus::StaleIfError;
                                cache_age_secs = Some(stale_entry.created_at.elapsed().as_secs());
                                response_body = stale_entry.body;
                                response_headers = stale_entry.headers;
                                response_status = StatusCode::from_u16(stale_entry.status_code).unwrap_or(StatusCode::OK);
                                tracing::info!(
                                    origin = %origin,
                                    path = %path,
                                    origin_status = %origin_response.2,
                                    "Serving stale content due to origin 5xx error (stale-if-error)"
                                );
                            } else {
                                // No stale content available, return the 5xx response
                                response_body = origin_response.0.clone();
                                response_headers = origin_response.1.clone();
                                response_status = origin_response.2;
                            }
                        } else {
                            response_body = origin_response.0.clone();
                            response_headers = origin_response.1.clone();
                            response_status = origin_response.2;

                            // Store in cache if cacheable
                            if is_cacheable(response_status, &response_headers) {
                                // Generate cache key with actual Vary header from response (RFC 9111)
                                let vary_header = response_headers.get("vary").map(|s| s.as_str());
                                let final_cache_key = generate_cache_key_with_vary(
                                    &origin,
                                    &format!("/{}", path),
                                    query_string.as_deref(),
                                    vary_header.or(Some("accept-encoding")),
                                    &request_headers_map,
                                );
                                store_in_cache(
                                    &state,
                                    &final_cache_key,
                                    origin_response.0,
                                    origin_response.1,
                                    response_status,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // RFC 5861: Try stale-if-error on connection/fetch errors too
                        if let Some(stale_entry) = state.cache.get_stale_for_error(&cache_key) {
                            cache_status = CacheStatus::StaleIfError;
                            cache_age_secs = Some(stale_entry.created_at.elapsed().as_secs());
                            response_body = stale_entry.body;
                            response_headers = stale_entry.headers;
                            response_status = StatusCode::from_u16(stale_entry.status_code).unwrap_or(StatusCode::OK);
                            tracing::info!(
                                origin = %origin,
                                path = %path,
                                error = %e,
                                "Serving stale content due to origin error (stale-if-error)"
                            );
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    // Update metrics
    let duration = start.elapsed();
    state
        .metrics
        .record_request(&origin, cache_status, response_status, duration);

    // RFC 9110 Section 14: Handle Range requests
    // Only process Range header for successful responses and GET requests
    let range_request: Option<ByteRange> = if !is_head_request && response_status.is_success() {
        if let Some(range_header) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
            let content_length = response_body.len() as u64;
            match parse_range_header(range_header, content_length) {
                RangeParseResult::Single(range) => Some(range),
                RangeParseResult::Multiple(_) => {
                    // Multi-range not supported, serve full content
                    None
                }
                RangeParseResult::Invalid => {
                    // Return 416 Range Not Satisfiable
                    return build_range_not_satisfiable_response(content_length);
                }
                RangeParseResult::None => None,
            }
        } else {
            None
        }
    } else {
        None
    };

    // Build response with RFC-compliant headers
    build_response(
        response_body,
        response_headers,
        response_status,
        cache_status,
        cache_age_secs,
        is_head_request,
        range_request.as_ref(),
    )
}

async fn fetch_from_origin_with_circuit_breaker(
    state: &Arc<AppState>,
    origin: &str,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
) -> CdnResult<(Bytes, HashMap<String, String>, StatusCode)> {
    match fetch_from_origin(state, origin, path, query, headers).await {
        Ok(result) => {
            state.circuit_breaker.record_success(origin);
            Ok(result)
        }
        Err(e) => {
            state.circuit_breaker.record_failure(origin);
            Err(e)
        }
    }
}

async fn fetch_from_origin(
    state: &Arc<AppState>,
    origin: &str,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
) -> CdnResult<(Bytes, HashMap<String, String>, StatusCode)> {
    let request_headers = extract_request_headers(headers);

    let response = state
        .origin
        .fetch(origin, path, query, &request_headers)
        .await?;

    let status = StatusCode::from_u16(response.status_code).unwrap_or(StatusCode::OK);
    Ok((response.body, response.headers, status))
}

fn extract_request_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (key, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            map.insert(key.to_string(), v.to_string());
        }
    }
    map
}

fn extract_client_ip(headers: &HeaderMap, fallback: std::net::IpAddr) -> std::net::IpAddr {
    // Check X-Forwarded-For header
    if let Some(forwarded) = headers.get("X-Forwarded-For") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first_ip) = value.split(',').next() {
                if let Ok(ip) = first_ip.trim().parse() {
                    return ip;
                }
            }
        }
    }

    // Check X-Real-IP header
    if let Some(real_ip) = headers.get("X-Real-IP") {
        if let Ok(value) = real_ip.to_str() {
            if let Ok(ip) = value.trim().parse() {
                return ip;
            }
        }
    }

    fallback
}

fn is_cacheable(status: StatusCode, headers: &HashMap<String, String>) -> bool {
    // Only cache successful responses
    if !status.is_success() && status != StatusCode::NOT_MODIFIED {
        return false;
    }

    // Check Cache-Control header
    if let Some(cc) = headers.get("cache-control") {
        let directives = parse_cache_control(cc);
        return directives.is_cacheable();
    }

    // Default to cacheable for successful responses
    true
}

fn store_in_cache(
    state: &Arc<AppState>,
    cache_key: &str,
    body: Bytes,
    headers: HashMap<String, String>,
    status: StatusCode,
) {
    let config = &state.config.cache;

    // Parse Cache-Control directives
    let directives = headers
        .get("cache-control")
        .map(|cc| parse_cache_control(cc))
        .unwrap_or_default();

    // Determine TTL
    let ttl = directives.ttl(config.default_ttl(), config.max_ttl());

    let now = Instant::now();

    // Generate ETag if not present
    let etag = headers.get("etag").cloned().or_else(|| {
        let hash = xxh3_64(&body);
        Some(format!("\"{}\"", BASE64.encode(hash.to_be_bytes())))
    });

    let entry = CacheEntry {
        size: body.len(),
        body,
        headers: headers.clone(),
        status_code: status.as_u16(),
        content_type: headers.get("content-type").cloned(),
        etag,
        last_modified: headers.get("last-modified").cloned(),
        created_at: now,
        expires_at: now + ttl,
        stale_if_error_secs: directives.stale_if_error,
    };

    state.cache.set(cache_key.to_string(), entry);
}

fn build_response(
    body: Bytes,
    headers: HashMap<String, String>,
    status: StatusCode,
    cache_status: CacheStatus,
    cache_age_secs: Option<u64>,
    is_head_request: bool,
    range_request: Option<&ByteRange>,
) -> CdnResult<Response> {
    let content_length = body.len() as u64;

    // Determine if we're serving a range response
    let (final_status, final_body, content_range) = if let Some(range) = range_request {
        // Serve partial content (206)
        let range_body = extract_range(&body, range);
        let content_range = range.content_range_header(content_length);
        (StatusCode::PARTIAL_CONTENT, range_body, Some(content_range))
    } else {
        (status, body, None)
    };

    let mut response = Response::builder().status(final_status);

    // Add headers from origin/cache
    for (key, value) in &headers {
        // Skip Content-Length for range responses - we'll set it correctly
        if range_request.is_some() && key.to_lowercase() == "content-length" {
            continue;
        }
        if let Ok(header_value) = HeaderValue::from_str(value) {
            response = response.header(key.as_str(), header_value);
        }
    }

    // Add CDN-specific headers
    response = response.header("X-Cache", cache_status.as_str());
    response = response.header("X-CDN", "Screaming-Eagle");

    // RFC 9110: Date header - indicates when the message was generated
    let date = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    response = response.header(header::DATE, date);

    // RFC 9110: Via header - identifies intermediate proxies
    response = response.header(header::VIA, "1.1 screaming-eagle");

    // RFC 9111: Age header - indicates time in cache (only for cached responses)
    if let Some(age) = cache_age_secs {
        response = response.header(header::AGE, age.to_string());
    }

    // RFC 9110: Accept-Ranges header - indicate we support byte ranges
    response = response.header(header::ACCEPT_RANGES, "bytes");

    // RFC 9110: Content-Range header for partial responses
    if let Some(cr) = content_range {
        response = response.header(header::CONTENT_RANGE, cr);
        // Set correct Content-Length for the partial response
        response = response.header(header::CONTENT_LENGTH, final_body.len().to_string());
    }

    // For HEAD requests, return empty body but keep Content-Length from original
    let response_body = if is_head_request {
        Body::empty()
    } else {
        Body::from(final_body)
    };

    response
        .body(response_body)
        .map_err(|e| CdnError::Internal(format!("Failed to build response: {}", e)))
}

/// Build a 416 Range Not Satisfiable response
fn build_range_not_satisfiable_response(content_length: u64) -> CdnResult<Response> {
    let mut response = Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE);

    // RFC 9110: Content-Range header with unsatisfied-range
    response = response.header(header::CONTENT_RANGE, format!("bytes */{}", content_length));

    // Add standard headers
    let date = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    response = response.header(header::DATE, date);
    response = response.header(header::VIA, "1.1 screaming-eagle");
    response = response.header("X-CDN", "Screaming-Eagle");
    response = response.header(header::ACCEPT_RANGES, "bytes");

    response
        .body(Body::empty())
        .map_err(|e| CdnError::Internal(format!("Failed to build response: {}", e)))
}

// Catch-all handler for root origin requests - supports both GET and HEAD
pub async fn root_cdn_handler(
    State(state): State<Arc<AppState>>,
    connect_info: ConnectInfo<SocketAddr>,
    method: Method,
    Path(path): Path<String>,
    Query(query): Query<CdnQuery>,
    headers: HeaderMap,
) -> Result<Response, CdnError> {
    // Use default origin if only one is configured
    let origins = state.origin.origin_names();
    if origins.len() == 1 {
        let origin = origins[0].to_string();
        return cdn_handler(
            State(state),
            connect_info,
            method,
            Path((origin, path)),
            Query(query),
            headers,
        )
        .await;
    }

    Err(CdnError::InvalidRequest(
        "Origin must be specified in path: /<origin>/<path>".to_string(),
    ))
}
