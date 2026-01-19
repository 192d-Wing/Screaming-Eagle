# Screaming Eagle CDN

A high-performance, RFC-compliant Content Delivery Network written in Rust.

## Features

### Core CDN

- **High Performance**: Built with Tokio and Axum for async I/O and minimal latency
- **In-Memory Caching**: Fast LRU-style cache with configurable size limits
- **Multiple Origins**: Support for multiple origin servers with per-origin configuration
- **Cache Control**: Respects Cache-Control headers (max-age, s-maxage, no-cache, no-store)
- **ETag Generation**: Automatic ETag generation using xxHash for efficient validation
- **TLS/HTTPS**: Native TLS support with rustls (TLS 1.3)
- **Compression**: Gzip and Brotli compression support
- **CORS**: Built-in CORS handling
- **Docker Support**: Ready-to-use Dockerfile and docker-compose

### RFC Compliance

- **HTTP Caching (RFC 9111)**: Full Cache-Control support, Age header, conditional requests
- **Range Requests (RFC 9110)**: 206 Partial Content for video streaming and large downloads
- **Stale Content (RFC 5861)**: stale-while-revalidate and stale-if-error directives
- **HEAD Method**: Returns headers without body for cache validation tools
- **Vary Header Support**: Vary-based cache keying for content negotiation
- **Standard Headers**: Age, Date, Via, Accept-Ranges headers on all responses

### Reliability

- **Rate Limiting**: Token bucket rate limiting per client IP
- **Circuit Breaker**: Automatic origin failure detection and recovery
- **Origin Health Checks**: Periodic background health monitoring for origins
- **Stale-if-error**: Serve cached content during origin outages (5xx errors)
- **Graceful Shutdown**: Clean shutdown with in-flight request handling

### Observability

- **Prometheus Metrics**: Comprehensive metrics for monitoring
- **Cache Purge API**: Purge by key, prefix, or all entries
- **Structured Logging**: JSON logging with tracing
- **Admin API Authentication**: Token-based auth with optional IP restrictions

## Quick Start

### Building

```bash
cargo build --release
```

### Running

```bash
# With default configuration
./target/release/screaming-eagle

# With custom config file
CDN_CONFIG=/path/to/config.toml ./target/release/screaming-eagle
```

### Docker

```bash
# Build and run with docker-compose
docker-compose up -d

# Or build manually
docker build -t screaming-eagle .
docker run -p 8080:8080 -v ./config:/app/config screaming-eagle
```

## Configuration

Create a `config/cdn.toml` file:

```toml
[server]
host = "0.0.0.0"
port = 8080
workers = 4
request_timeout_secs = 30

[cache]
max_size_mb = 1024          # 1GB total cache size
max_entry_size_mb = 100      # 100MB max per entry
default_ttl_secs = 3600      # 1 hour default TTL
max_ttl_secs = 86400         # 24 hour max TTL
stale_while_revalidate_secs = 60
respect_cache_control = true

[logging]
level = "info"
json_format = false

# Rate limiting
[rate_limit]
enabled = true
requests_per_window = 1000   # Max requests per window
window_secs = 60             # Window duration
burst_size = 50              # Burst allowance

# Circuit breaker
[circuit_breaker]
failure_threshold = 5        # Failures before opening
reset_timeout_secs = 30      # Time before half-open
success_threshold = 3        # Successes to close

# TLS (optional)
# [tls]
# cert_path = "/path/to/cert.pem"
# key_path = "/path/to/key.pem"

# Origin servers
[origins.myapp]
url = "https://api.example.com"
timeout_secs = 30
max_retries = 3
health_check_path = "/health"        # Optional health check endpoint
health_check_interval_secs = 30      # Check every 30 seconds
health_check_timeout_secs = 5        # 5 second timeout

[origins.static]
url = "https://static.example.com"
host_header = "static.example.com"
timeout_secs = 60
max_retries = 3
health_check_path = "/_health"       # Optional: different origins can have different paths

# Admin API authentication (optional)
[admin]
auth_enabled = true                  # Enable authentication for admin endpoints
auth_token = "your-secret-token"     # Bearer token for authentication
allowed_ips = ["127.0.0.1", "10.0.0.0/8"]  # Optional: restrict access by IP
```

## API Endpoints

### Authentication

When admin authentication is enabled (`admin.auth_enabled = true`), the following endpoints require a bearer token:

- `/_cdn/stats` - Cache statistics
- `/_cdn/purge` - Cache purge
- `/_cdn/circuit-breakers` - Circuit breaker status
- `/_cdn/origins/health` - Origin health status

Public endpoints (no authentication required):

- `/_cdn/health` - CDN health check
- `/_cdn/metrics` - Prometheus metrics
- All CDN proxy routes

**Example authenticated request:**

```bash
curl -H "Authorization: Bearer your-secret-token" http://localhost:8080/_cdn/stats
```

### CDN Proxy

```text
GET /<origin>/<path>
```

Proxies requests to the configured origin and caches the response.

**Example:**

```bash
curl http://localhost:8080/myapp/api/users
```

### Health Check

```text
GET /_cdn/health
```

Returns CDN health status.

### Cache Statistics

```text
GET /_cdn/stats
```

Returns cache statistics including hit ratio, size, and entry count.

### Prometheus Metrics

```text
GET /_cdn/metrics
```

Returns metrics in Prometheus format.

### Circuit Breaker Status

```text
GET /_cdn/circuit-breakers
```

Returns the state of all circuit breakers for each origin.

### Origin Health Status

```text
GET /_cdn/origins/health
```

Returns health check status for all origins:

```json
{
  "origins": {
    "myapp": {
      "status": "healthy",
      "last_check": 1705593600,
      "last_success": 1705593600,
      "last_failure": null,
      "consecutive_failures": 0,
      "response_time_ms": 45,
      "error_message": null
    },
    "static": {
      "status": "unhealthy",
      "last_check": 1705593600,
      "last_failure": 1705593600,
      "consecutive_failures": 3,
      "response_time_ms": 5000,
      "error_message": "Connection timeout"
    }
  }
}
```

Health status values: `healthy`, `unhealthy`, `unknown`

### Cache Purge

```text
POST /_cdn/purge
Content-Type: application/json

# Purge specific keys
{"keys": ["myapp/api/users", "myapp/api/posts"]}

# Purge by prefix
{"prefix": "myapp/api/"}

# Purge all
{"all": true}
```

## Response Headers

The CDN adds these headers to responses:

| Header | Description |
| -------- | ------------- |
| `X-Cache` | Cache status: HIT, MISS, STALE, STALE-IF-ERROR, BYPASS |
| `X-Cache-Key` | Cache key used for this request |
| `Age` | Seconds since response was cached (RFC 9111) |
| `Date` | Response generation timestamp (RFC 9110) |
| `Via` | Proxy identifier: `1.1 screaming-eagle` (RFC 9110) |
| `Accept-Ranges` | Always `bytes` - indicates range request support |
| `Content-Range` | Byte range for 206 responses (e.g., `bytes 0-1023/4096`) |
| `X-RateLimit-Remaining` | Remaining requests in current window |
| `Retry-After` | Seconds until rate limit resets (when limited) |

### Range Requests

The CDN supports HTTP Range requests for partial content delivery:

```bash
# Request first 1KB of a file
curl -H "Range: bytes=0-1023" http://localhost:8080/static/video.mp4

# Response: 206 Partial Content
# Content-Range: bytes 0-1023/10485760
```

Use cases:

- Video streaming (seeking)
- Resumable downloads
- Large file transfers

## Environment Variables

- `CDN_CONFIG`: Path to configuration file (default: `config/cdn.toml`)
- `RUST_LOG`: Log level override (e.g., `debug`, `info`, `warn`)

## Rate Limiting

Rate limiting uses a token bucket algorithm:

- Each client IP gets a bucket with `requests_per_window + burst_size` tokens
- Tokens refill at `requests_per_window / window_secs` per second
- When bucket is empty, requests return 429 Too Many Requests
- X-Forwarded-For and X-Real-IP headers are respected for client IP detection

## Circuit Breaker

The circuit breaker protects against cascading failures:

| State | Description |
| ------- | ------------- |
| **Closed** | Normal operation, requests flow to origin |
| **Open** | Origin marked as failed, requests fail fast |
| **Half-Open** | Testing recovery, limited requests allowed |

Transitions:

- Closed → Open: After `failure_threshold` consecutive failures
- Open → Half-Open: After `reset_timeout_secs` seconds
- Half-Open → Closed: After `success_threshold` consecutive successes
- Half-Open → Open: On any failure

## Metrics

Available Prometheus metrics:

- `cdn_requests_total`: Total requests by origin, status, and cache status
- `cdn_cache_hits_total`: Cache hits by origin
- `cdn_cache_misses_total`: Cache misses by origin
- `cdn_request_duration_seconds`: Request duration histogram
- `cdn_origin_requests_total`: Requests to origin servers
- `cdn_bytes_served_total`: Bytes served by cache status

## Architecture

```text
                    ┌─────────────────┐
                    │   HTTP Client   │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │   Rate Limiter  │
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │   Axum Router   │
                    │  (Compression,  │
                    │   CORS, Trace)  │
                    └────────┬────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
    ┌─────────▼─────────┐    │   ┌──────────▼────────┐
    │   CDN Handler     │    │   │   Admin API       │
    │  /<origin>/<path> │    │   │   /_cdn/*         │
    └─────────┬─────────┘    │   └───────────────────┘
              │              │
    ┌─────────▼─────────┐    │
    │   Cache Layer     │    │
    │   (DashMap LRU)   │    │
    └─────────┬─────────┘    │
              │              │
    ┌─────────▼─────────┐    │
    │ Circuit Breaker   │    │
    └─────────┬─────────┘    │
              │              │
    ┌─────────▼─────────┐    │
    │  Origin Fetcher   │    │
    │   (reqwest)       │    │
    └─────────┬─────────┘    │
              │              │
    ┌─────────▼─────────┐    │
    │   Origin Server   │◄───┘
    └───────────────────┘
```

## Testing

```bash
# Run all tests
cargo test

# Run with verbose output
cargo test -- --nocapture
```

## RFC Compliance Tracking

For detailed RFC compliance status, see [docs/RFC_COMPLIANCE.md](docs/RFC_COMPLIANCE.md).

### Summary

| RFC | Title | Status |
| ----- | ------- | -------- |
| RFC 9110 | HTTP Semantics | Compliant (GET, HEAD, Range, conditional requests) |
| RFC 9111 | HTTP Caching | Compliant (Cache-Control, Age, Vary) |
| RFC 5861 | Stale Content Extensions | Compliant (stale-while-revalidate, stale-if-error) |
| RFC 8446 | TLS 1.3 | Compliant (via rustls) |

## License

MIT License - see LICENSE file for details.
