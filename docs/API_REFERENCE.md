# API Reference

This document provides a comprehensive reference for all Screaming Eagle CDN API endpoints.

## Table of Contents

- [Authentication](#authentication)
- [Public Endpoints](#public-endpoints)
- [Admin Endpoints](#admin-endpoints)
- [Proxy Endpoints](#proxy-endpoints)
- [Response Headers](#response-headers)
- [Error Responses](#error-responses)

## Authentication

Admin endpoints require Bearer token authentication. Include the token in the `Authorization` header:

```
Authorization: Bearer <your-token-here>
```

The token is configured in `cdn.toml` under `[admin]`.

### IP Allowlist

Admin endpoints can be restricted to specific IP addresses via the `allowed_ips` configuration in `cdn.toml`.

## Public Endpoints

These endpoints are accessible without authentication.

### Health Check

Returns the CDN's health status.

**Endpoint:** `GET /_cdn/health`

**Response:** `200 OK`

```json
{
  "status": "healthy",
  "uptime_seconds": 3600,
  "cache_entries": 1234,
  "memory_usage_mb": 256.5
}
```

**Use Case:** Load balancer health checks, monitoring systems

---

### Metrics

Returns Prometheus-formatted metrics for monitoring.

**Endpoint:** `GET /_cdn/metrics`

**Response:** `200 OK`

**Content-Type:** `text/plain; version=0.0.4`

**Metrics Exposed:**

- `cdn_requests_total{method, status}` - Total HTTP requests
- `cdn_cache_hits_total{origin}` - Cache hits per origin
- `cdn_cache_misses_total{origin}` - Cache misses per origin
- `cdn_request_duration_seconds{method, status}` - Request latency histogram
- `cdn_cache_size_bytes` - Current cache size in bytes
- `cdn_origin_bytes_total{origin}` - Bytes fetched from origins

**Example:**

```
# HELP cdn_requests_total Total number of HTTP requests
# TYPE cdn_requests_total counter
cdn_requests_total{method="GET",status="200"} 45678
cdn_requests_total{method="GET",status="404"} 234

# HELP cdn_cache_hits_total Total cache hits
# TYPE cdn_cache_hits_total counter
cdn_cache_hits_total{origin="example"} 12345
```

**Use Case:** Prometheus scraping, Grafana dashboards, alerting

## Admin Endpoints

These endpoints require Bearer token authentication.

### Cache Statistics

Returns detailed cache statistics and hit ratios.

**Endpoint:** `GET /_cdn/stats`

**Authentication:** Required

**Response:** `200 OK`

```json
{
  "total_entries": 5432,
  "total_size_bytes": 536870912,
  "max_size_bytes": 1073741824,
  "utilization_percent": 50.0,
  "hit_count": 98765,
  "miss_count": 12345,
  "hit_ratio": 0.889,
  "eviction_count": 567,
  "origins": {
    "example": {
      "entries": 4000,
      "size_bytes": 419430400,
      "hits": 80000,
      "misses": 8000,
      "hit_ratio": 0.909
    },
    "api": {
      "entries": 1432,
      "size_bytes": 117440512,
      "hits": 18765,
      "misses": 4345,
      "hit_ratio": 0.812
    }
  }
}
```

**Use Case:** Performance monitoring, capacity planning

---

### Cache Purging

Invalidates cached entries.

**Endpoint:** `POST /_cdn/purge`

**Authentication:** Required

**Request Body:**

```json
{
  "key": "/path/to/resource",           // Purge specific key (optional)
  "prefix": "/images/",                  // Purge by prefix (optional)
  "origin": "example",                   // Purge by origin (optional)
  "purge_all": false                     // Purge all entries (optional)
}
```

**Response:** `200 OK`

```json
{
  "purged_count": 42,
  "message": "Successfully purged 42 cache entries"
}
```

**Examples:**

Purge a specific resource:
```bash
curl -X POST http://localhost:8080/_cdn/purge \
  -H "Authorization: Bearer secret-token" \
  -H "Content-Type: application/json" \
  -d '{"key": "/images/logo.png"}'
```

Purge by prefix:
```bash
curl -X POST http://localhost:8080/_cdn/purge \
  -H "Authorization: Bearer secret-token" \
  -H "Content-Type: application/json" \
  -d '{"prefix": "/images/"}'
```

Purge all entries from an origin:
```bash
curl -X POST http://localhost:8080/_cdn/purge \
  -H "Authorization: Bearer secret-token" \
  -H "Content-Type: application/json" \
  -d '{"origin": "example"}'
```

Purge everything:
```bash
curl -X POST http://localhost:8080/_cdn/purge \
  -H "Authorization: Bearer secret-token" \
  -H "Content-Type: application/json" \
  -d '{"purge_all": true}'
```

**Use Case:** Content updates, deployments, invalidation after errors

---

### Cache Warming

Pre-populates the cache with specified URLs.

**Endpoint:** `POST /_cdn/warm`

**Authentication:** Required

**Request Body:**

```json
{
  "urls": [
    "http://localhost:8080/example/index.html",
    "http://localhost:8080/example/styles.css",
    "http://localhost:8080/api/users"
  ]
}
```

**Response:** `200 OK`

```json
{
  "total_requested": 3,
  "successful": 2,
  "failed": 1,
  "results": [
    {
      "url": "http://localhost:8080/example/index.html",
      "status": "success",
      "cached": true
    },
    {
      "url": "http://localhost:8080/example/styles.css",
      "status": "success",
      "cached": true
    },
    {
      "url": "http://localhost:8080/api/users",
      "status": "error",
      "error": "Origin timeout"
    }
  ]
}
```

**Use Case:** Post-deployment cache warming, reducing cold-start latency

---

### Circuit Breaker Status

Returns the status of circuit breakers for all origins.

**Endpoint:** `GET /_cdn/circuit-breakers`

**Authentication:** Required

**Response:** `200 OK`

```json
{
  "circuit_breakers": {
    "example": {
      "state": "Closed",
      "failure_count": 2,
      "success_count": 1543,
      "last_failure_time": "2026-01-18T10:30:00Z",
      "half_open_attempts": 0
    },
    "api": {
      "state": "Open",
      "failure_count": 5,
      "success_count": 234,
      "last_failure_time": "2026-01-18T11:45:23Z",
      "reset_time": "2026-01-18T11:46:23Z"
    }
  }
}
```

**Circuit Breaker States:**

- `Closed` - Normal operation, requests pass through
- `Open` - Too many failures, requests fail-fast
- `HalfOpen` - Testing if origin has recovered

**Use Case:** Monitoring origin health, debugging outages

---

### Origin Health Status

Returns health check results for all configured origins.

**Endpoint:** `GET /_cdn/origins/health`

**Authentication:** Required

**Response:** `200 OK`

```json
{
  "origins": {
    "example": {
      "healthy": true,
      "last_check": "2026-01-18T12:00:00Z",
      "response_time_ms": 45,
      "consecutive_failures": 0,
      "consecutive_successes": 120
    },
    "api": {
      "healthy": false,
      "last_check": "2026-01-18T12:00:05Z",
      "response_time_ms": null,
      "error": "Connection refused",
      "consecutive_failures": 3,
      "consecutive_successes": 0
    }
  }
}
```

**Use Case:** Origin monitoring, alerting on origin failures

---

### Request Coalescing Statistics

Returns statistics about request coalescing (deduplication).

**Endpoint:** `GET /_cdn/coalesce`

**Authentication:** Required

**Response:** `200 OK`

```json
{
  "active_requests": 12,
  "total_coalesced": 4567,
  "savings_percent": 23.5,
  "current_requests": {
    "/images/hero.jpg": 5,
    "/api/popular": 3,
    "/videos/demo.mp4": 4
  }
}
```

**Fields:**

- `active_requests` - Number of unique resources currently being fetched
- `total_coalesced` - Total requests that were coalesced (not re-fetched)
- `savings_percent` - Percentage of origin requests saved
- `current_requests` - Active fetches with waiting request count

**Use Case:** Understanding thundering herd prevention effectiveness

## Proxy Endpoints

These are the main CDN endpoints that proxy requests to origins.

### Proxy Request

Proxies a request to the configured origin, with caching.

**Endpoint:** `GET|HEAD /<origin>/<path>`

**Parameters:**

- `<origin>` - Origin name from `cdn.toml`
- `<path>` - Resource path to fetch

**Request Headers:**

- `Range` - Request partial content (RFC 9110)
- `If-None-Match` - Conditional request using ETag
- `If-Modified-Since` - Conditional request using Last-Modified
- `Accept-Encoding` - Compression preferences (gzip, br)
- `X-Forwarded-For` - Client IP forwarding
- `X-Request-ID` - Request tracking ID

**Response:** Varies (proxied from origin)

**Response Headers:** See [Response Headers](#response-headers) section

**Examples:**

Basic request:
```bash
curl http://localhost:8080/example/index.html
```

Range request (partial content):
```bash
curl -H "Range: bytes=0-1023" http://localhost:8080/example/video.mp4
```

Conditional request:
```bash
curl -H 'If-None-Match: "abc123"' http://localhost:8080/example/data.json
```

**Use Case:** Main CDN functionality - content delivery

## Response Headers

All responses include standard HTTP headers plus CDN-specific headers.

### Standard Headers

- `Content-Type` - MIME type of the response
- `Content-Length` - Size of the response body
- `Content-Encoding` - Compression applied (gzip, br)
- `Cache-Control` - Caching directives from origin
- `ETag` - Entity tag for cache validation (xxHash3)
- `Last-Modified` - Last modification time from origin
- `Expires` - Expiration time
- `Vary` - Headers that affect caching

### CDN-Specific Headers

- `X-Cache` - Cache status: `HIT`, `MISS`, `STALE`, `BYPASS`, `EXPIRED`
- `X-Cache-Key` - Cache key used for this request
- `Age` - Time in seconds the object has been in cache
- `Date` - Response generation time
- `Via` - CDN identifier (e.g., "1.1 screaming-eagle-cdn")
- `X-Origin` - Origin server that provided the content
- `X-Request-ID` - Unique request identifier for tracing

### Range Request Headers

For 206 Partial Content responses:

- `Content-Range` - Byte range being returned (e.g., "bytes 0-1023/5000")
- `Accept-Ranges` - Indicates range support ("bytes")

### Security Headers

Configurable security headers:

- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `X-XSS-Protection: 1; mode=block`
- `Strict-Transport-Security: max-age=31536000`
- `Content-Security-Policy` - Custom CSP if configured

## Error Responses

All errors return JSON with consistent structure.

### Standard Error Format

```json
{
  "error": "Error message",
  "status": 500,
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-01-18T12:00:00Z"
}
```

### Common Error Codes

#### 400 Bad Request

Invalid request syntax or parameters.

```json
{
  "error": "Invalid Range header format",
  "status": 400,
  "request_id": "..."
}
```

#### 401 Unauthorized

Missing or invalid authentication token.

```json
{
  "error": "Missing or invalid authentication token",
  "status": 401,
  "request_id": "..."
}
```

#### 403 Forbidden

IP not in allowlist or request signature invalid.

```json
{
  "error": "Access denied: IP not in allowlist",
  "status": 403,
  "request_id": "..."
}
```

#### 404 Not Found

Origin not configured or resource not found.

```json
{
  "error": "Origin 'unknown' not found",
  "status": 404,
  "request_id": "..."
}
```

#### 429 Too Many Requests

Rate limit exceeded.

```json
{
  "error": "Rate limit exceeded",
  "status": 429,
  "retry_after": 30,
  "request_id": "..."
}
```

**Response Headers:**

- `Retry-After: 30` - Seconds to wait before retrying
- `X-RateLimit-Limit: 100` - Requests allowed per window
- `X-RateLimit-Remaining: 0` - Remaining requests in current window
- `X-RateLimit-Reset: 1705579200` - Unix timestamp when limit resets

#### 500 Internal Server Error

CDN internal error.

```json
{
  "error": "Internal server error",
  "status": 500,
  "request_id": "..."
}
```

#### 502 Bad Gateway

Origin server returned invalid response.

```json
{
  "error": "Origin server returned invalid response",
  "status": 502,
  "origin": "example",
  "request_id": "..."
}
```

#### 503 Service Unavailable

Circuit breaker open or origin unreachable.

```json
{
  "error": "Circuit breaker open for origin 'example'",
  "status": 503,
  "retry_after": 60,
  "request_id": "..."
}
```

#### 504 Gateway Timeout

Origin request timed out.

```json
{
  "error": "Origin request timeout",
  "status": 504,
  "timeout_ms": 5000,
  "origin": "example",
  "request_id": "..."
}
```

### Stale Content Delivery

When `stale-if-error` is configured and origin fails, the CDN may return stale cached content with:

- `X-Cache: STALE`
- `Warning: 110 - "Response is Stale"`
- Standard caching headers from original response

## Rate Limiting

Rate limiting is applied per client IP address using a token bucket algorithm.

### Configuration

```toml
[rate_limit]
enabled = true
requests_per_window = 100
window_seconds = 60
burst_size = 20
```

### Headers

All responses include rate limit information:

- `X-RateLimit-Limit` - Maximum requests per window
- `X-RateLimit-Remaining` - Requests remaining in current window
- `X-RateLimit-Reset` - Unix timestamp when the limit resets

### Exceeded Limit

When the rate limit is exceeded, the CDN returns `429 Too Many Requests` with a `Retry-After` header indicating how long to wait.

## Caching Behavior

### Cache Key Generation

Cache keys are generated from:

1. Origin name
2. Request path
3. Query parameters (normalized)
4. Vary headers (if present)

Example: `example:/images/logo.png?v=2`

### TTL Determination

TTL is determined in this order:

1. `Cache-Control: max-age` from origin response
2. `Expires` header from origin response
3. Default TTL from `cdn.toml` configuration

### Stale Content

The CDN supports RFC 5861 directives:

- `stale-while-revalidate` - Serve stale content while fetching fresh
- `stale-if-error` - Serve stale content if origin fails

### Cache Bypass

Requests with these headers bypass cache:

- `Cache-Control: no-cache`
- `Cache-Control: no-store`
- `Pragma: no-cache`

Admin endpoints (`/_cdn/*`) are never cached.

## Request Coalescing

When multiple clients request the same uncached resource simultaneously, the CDN:

1. Makes a single request to origin
2. Broadcasts the response to all waiting clients
3. Caches the response for future requests

This prevents the "thundering herd" problem and reduces origin load.

## Circuit Breaker Behavior

The circuit breaker protects origins from cascading failures.

### States

1. **Closed** (Normal)
   - Requests pass through normally
   - Failures are counted
   - When failure threshold reached, transitions to Open

2. **Open** (Failing)
   - Requests fail immediately with `503 Service Unavailable`
   - Stale content served if available (`stale-if-error`)
   - After timeout period, transitions to HalfOpen

3. **HalfOpen** (Testing)
   - Limited test requests allowed through
   - If success threshold reached, transitions to Closed
   - If any failure occurs, transitions back to Open

### Configuration

```toml
[circuit_breaker]
failure_threshold = 5        # Failures before opening
timeout_seconds = 60         # Time to wait before testing
success_threshold = 2        # Successes needed to close
```

## Examples

### Complete Request Flow

```bash
# First request (cache miss)
curl -v http://localhost:8080/example/data.json
# < X-Cache: MISS
# < Age: 0
# < ETag: "abc123"

# Second request (cache hit)
curl -v http://localhost:8080/example/data.json
# < X-Cache: HIT
# < Age: 5
# < ETag: "abc123"

# Conditional request (not modified)
curl -H 'If-None-Match: "abc123"' http://localhost:8080/example/data.json
# < 304 Not Modified
# < X-Cache: HIT

# Purge the cache
curl -X POST http://localhost:8080/_cdn/purge \
  -H "Authorization: Bearer secret-token" \
  -d '{"key": "/data.json"}'

# Next request is a miss again
curl -v http://localhost:8080/example/data.json
# < X-Cache: MISS
```

### Monitoring Setup

```bash
# Check health
curl http://localhost:8080/_cdn/health

# View metrics
curl http://localhost:8080/_cdn/metrics

# Check cache stats (requires auth)
curl -H "Authorization: Bearer secret-token" \
  http://localhost:8080/_cdn/stats

# Monitor circuit breakers
curl -H "Authorization: Bearer secret-token" \
  http://localhost:8080/_cdn/circuit-breakers
```

## API Versioning

The current API is version 1.0. Future versions will be introduced with:

- Path-based versioning (e.g., `/_cdn/v2/stats`)
- Backward compatibility maintenance for at least one major version
- Deprecation notices in response headers
