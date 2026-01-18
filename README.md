# Screaming Eagle CDN

A high-performance Content Delivery Network written in Rust.

## Features

- **High Performance**: Built with Tokio and Axum for async I/O and minimal latency
- **In-Memory Caching**: Fast LRU-style cache with configurable size limits
- **Multiple Origins**: Support for multiple origin servers with per-origin configuration
- **Cache Control**: Respects Cache-Control headers (max-age, s-maxage, no-cache, no-store)
- **Stale-While-Revalidate**: Serves stale content while refreshing in the background
- **ETag Generation**: Automatic ETag generation using xxHash for efficient validation
- **Rate Limiting**: Token bucket rate limiting per client IP
- **Circuit Breaker**: Automatic origin failure detection and recovery
- **TLS/HTTPS**: Native TLS support with rustls
- **Compression**: Gzip and Brotli compression support
- **CORS**: Built-in CORS handling
- **Prometheus Metrics**: Comprehensive metrics for monitoring
- **Cache Purge API**: Purge by key, prefix, or all entries
- **Graceful Shutdown**: Clean shutdown with in-flight request handling
- **Docker Support**: Ready-to-use Dockerfile and docker-compose

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

[origins.static]
url = "https://static.example.com"
host_header = "static.example.com"
timeout_secs = 60
max_retries = 3
```

## API Endpoints

### CDN Proxy

```
GET /<origin>/<path>
```

Proxies requests to the configured origin and caches the response.

**Example:**
```bash
curl http://localhost:8080/myapp/api/users
```

### Health Check

```
GET /_cdn/health
```

Returns CDN health status.

### Cache Statistics

```
GET /_cdn/stats
```

Returns cache statistics including hit ratio, size, and entry count.

### Prometheus Metrics

```
GET /_cdn/metrics
```

Returns metrics in Prometheus format.

### Circuit Breaker Status

```
GET /_cdn/circuit-breakers
```

Returns the state of all circuit breakers for each origin.

### Cache Purge

```
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

- `X-Cache`: Cache status (HIT, MISS, STALE, BYPASS)
- `X-CDN`: CDN identifier (Screaming-Eagle)
- `X-RateLimit-Remaining`: Remaining requests in window
- `Retry-After`: Seconds until rate limit resets (when limited)

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
|-------|-------------|
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

```
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
    ┌─────────▼─────────┐   │    ┌─────────▼─────────┐
    │   CDN Handler     │   │    │   Admin API       │
    │  /<origin>/<path> │   │    │   /_cdn/*         │
    └─────────┬─────────┘   │    └───────────────────┘
              │             │
    ┌─────────▼─────────┐   │
    │   Cache Layer     │   │
    │   (DashMap LRU)   │   │
    └─────────┬─────────┘   │
              │             │
    ┌─────────▼─────────┐   │
    │ Circuit Breaker   │   │
    └─────────┬─────────┘   │
              │             │
    ┌─────────▼─────────┐   │
    │  Origin Fetcher   │   │
    │   (reqwest)       │   │
    └─────────┬─────────┘   │
              │             │
    ┌─────────▼─────────┐   │
    │   Origin Server   │◄──┘
    └───────────────────┘
```

## Testing

```bash
# Run all tests
cargo test

# Run with verbose output
cargo test -- --nocapture
```

## License

MIT License - see LICENSE file for details.
