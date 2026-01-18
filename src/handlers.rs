use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use xxhash_rust::xxh3::xxh3_64;

use crate::cache::{
    generate_cache_key, parse_cache_control, Cache, CacheEntry, CacheStats, CacheStatus,
};
use crate::config::Config;
use crate::error::{CdnError, CdnResult};
use crate::metrics::Metrics;
use crate::origin::OriginFetcher;

pub struct AppState {
    pub cache: Arc<Cache>,
    pub origin: Arc<OriginFetcher>,
    pub config: Arc<Config>,
    pub metrics: Arc<Metrics>,
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

// Main CDN handler
pub async fn cdn_handler(
    State(state): State<Arc<AppState>>,
    Path((origin, path)): Path<(String, String)>,
    Query(query): Query<CdnQuery>,
    headers: HeaderMap,
) -> Result<Response, CdnError> {
    let start = Instant::now();

    // Validate origin exists
    if !state.origin.has_origin(&origin) {
        return Err(CdnError::NotFound(format!("Unknown origin: {}", origin)));
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

    // Generate cache key
    let cache_key = generate_cache_key(&origin, &format!("/{}", path), query_string.as_deref());

    // Check request cache control
    let bypass_cache = headers
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("no-cache") || v.contains("no-store"))
        .unwrap_or(false);

    let cache_status;
    let response_body;
    let response_headers;
    let response_status;

    if bypass_cache {
        // Client requested bypass
        cache_status = CacheStatus::Bypass;
        let origin_response = fetch_from_origin(&state, &origin, &path, query_string.as_deref(), &headers).await?;
        response_body = origin_response.0;
        response_headers = origin_response.1;
        response_status = origin_response.2;
    } else {
        // Try cache first
        match state.cache.get(&cache_key) {
            Some((entry, status)) => {
                cache_status = status;
                response_body = entry.body;
                response_headers = entry.headers;
                response_status = StatusCode::from_u16(entry.status_code).unwrap_or(StatusCode::OK);

                // If stale, trigger background revalidation
                if status == CacheStatus::Stale {
                    let state_clone = state.clone();
                    let origin_clone = origin.clone();
                    let path_clone = path.clone();
                    let query_clone = query_string.clone();
                    let cache_key_clone = cache_key.clone();

                    tokio::spawn(async move {
                        if let Ok((body, headers, status)) = fetch_from_origin(
                            &state_clone,
                            &origin_clone,
                            &path_clone,
                            query_clone.as_deref(),
                            &HeaderMap::new(),
                        )
                        .await
                        {
                            store_in_cache(&state_clone, &cache_key_clone, body, headers, status);
                        }
                    });
                }
            }
            None => {
                // Cache miss - fetch from origin
                cache_status = CacheStatus::Miss;
                let origin_response = fetch_from_origin(&state, &origin, &path, query_string.as_deref(), &headers).await?;
                response_body = origin_response.0.clone();
                response_headers = origin_response.1.clone();
                response_status = origin_response.2;

                // Store in cache if cacheable
                if is_cacheable(response_status, &response_headers) {
                    store_in_cache(&state, &cache_key, origin_response.0, origin_response.1, response_status);
                }
            }
        }
    }

    // Update metrics
    let duration = start.elapsed();
    state.metrics.record_request(&origin, cache_status, response_status, duration);

    // Build response
    build_response(response_body, response_headers, response_status, cache_status)
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

    // Determine TTL
    let ttl = if let Some(cc) = headers.get("cache-control") {
        let directives = parse_cache_control(cc);
        directives.ttl(config.default_ttl(), config.max_ttl())
    } else {
        config.default_ttl()
    };

    let now = Instant::now();

    // Generate ETag if not present
    let etag = headers
        .get("etag")
        .cloned()
        .or_else(|| {
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
    };

    state.cache.set(cache_key.to_string(), entry);
}

fn build_response(
    body: Bytes,
    headers: HashMap<String, String>,
    status: StatusCode,
    cache_status: CacheStatus,
) -> CdnResult<Response> {
    let mut response = Response::builder().status(status);

    // Add headers from origin/cache
    for (key, value) in &headers {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            response = response.header(key.as_str(), header_value);
        }
    }

    // Add CDN-specific headers
    response = response.header("X-Cache", cache_status.as_str());
    response = response.header("X-CDN", "Screaming-Eagle");

    response
        .body(Body::from(body))
        .map_err(|e| CdnError::Internal(format!("Failed to build response: {}", e)))
}

// Catch-all handler for root origin requests
pub async fn root_cdn_handler(
    State(state): State<Arc<AppState>>,
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
