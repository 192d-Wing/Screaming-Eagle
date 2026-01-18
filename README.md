# Screaming Eagle CDN

A high-performance Content Delivery Network written in Rust.

## Features

- **High Performance**: Built with Tokio and Axum for async I/O and minimal latency
- **In-Memory Caching**: Fast LRU-style cache with configurable size limits
- **Multiple Origins**: Support for multiple origin servers with per-origin configuration
- **Cache Control**: Respects Cache-Control headers (max-age, s-maxage, no-cache, no-store)
- **Stale-While-Revalidate**: Serves stale content while refreshing in the background
- **ETag Generation**: Automatic ETag generation using xxHash for efficient validation
- **Compression**: Gzip and Brotli compression support
- **CORS**: Built-in CORS handling
- **Prometheus Metrics**: Comprehensive metrics for monitoring
- **Cache Purge API**: Purge by key, prefix, or all entries
- **Graceful Shutdown**: Clean shutdown with in-flight request handling

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

## Environment Variables

- `CDN_CONFIG`: Path to configuration file (default: `config/cdn.toml`)
- `RUST_LOG`: Log level override (e.g., `debug`, `info`, `warn`)

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
    │  Origin Fetcher   │   │
    │   (reqwest)       │   │
    └─────────┬─────────┘   │
              │             │
    ┌─────────▼─────────┐   │
    │   Origin Server   │◄──┘
    └───────────────────┘
```

## License

MIT License - see LICENSE file for details.
