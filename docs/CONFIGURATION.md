# Configuration Reference

Complete reference for all configuration options in Screaming Eagle CDN.

## Table of Contents

- [Configuration File](#configuration-file)
- [Server Configuration](#server-configuration)
- [Cache Configuration](#cache-configuration)
- [Logging Configuration](#logging-configuration)
- [Rate Limiting](#rate-limiting)
- [Circuit Breaker](#circuit-breaker)
- [TLS/HTTPS](#tlshttps)
- [Origins](#origins)
- [Admin Configuration](#admin-configuration)
- [Security](#security)
- [Edge Processing](#edge-processing)
- [Connection Pool](#connection-pool)
- [Health Checks](#health-checks)
- [Metrics](#metrics)
- [Environment Variables](#environment-variables)
- [Complete Example](#complete-example)

## Configuration File

The CDN is configured via a TOML file, typically `config/cdn.toml`.

**Load configuration:**

```bash
./screaming-eagle-cdn --config config/cdn.toml
```

**Default location:** `config/cdn.toml` (if not specified)

## Server Configuration

Controls the HTTP server behavior.

```toml
[server]
host = "0.0.0.0"
port = 8080
workers = 4
request_timeout_secs = 30
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `host` | string | `"0.0.0.0"` | IP address to bind to. Use `0.0.0.0` for all interfaces, `127.0.0.1` for localhost only |
| `port` | integer | `8080` | Port to listen on. Must be > 1024 for non-root or use authbind/capabilities |
| `workers` | integer | CPU cores | Number of Tokio worker threads. Should match CPU cores for best performance |
| `request_timeout_secs` | integer | `30` | Maximum time to process a request before timing out |

### Examples

**Development (localhost only):**
```toml
[server]
host = "127.0.0.1"
port = 8080
workers = 2
```

**Production (all interfaces):**
```toml
[server]
host = "0.0.0.0"
port = 8080
workers = 8
request_timeout_secs = 60
```

**Behind load balancer:**
```toml
[server]
host = "0.0.0.0"
port = 8080
workers = 4
request_timeout_secs = 30
```

## Cache Configuration

Controls in-memory cache behavior.

```toml
[cache]
max_size_mb = 1024
max_entry_size_mb = 100
default_ttl_secs = 3600
max_ttl_secs = 86400
stale_while_revalidate_secs = 60
respect_cache_control = true
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_size_mb` | integer | `1024` | Maximum total cache size in megabytes. Cache will evict entries when this limit is reached |
| `max_entry_size_mb` | integer | `100` | Maximum size of a single cache entry in megabytes. Larger responses won't be cached |
| `default_ttl_secs` | integer | `3600` | Default time-to-live in seconds when origin doesn't specify Cache-Control |
| `max_ttl_secs` | integer | `86400` | Maximum TTL to honor, even if origin specifies higher |
| `stale_while_revalidate_secs` | integer | `60` | How long to serve stale content while fetching fresh version (RFC 5861) |
| `respect_cache_control` | boolean | `true` | Whether to honor Cache-Control headers from origin |

### Cache Sizing Guidelines

**Small deployment (< 1000 req/s):**
```toml
[cache]
max_size_mb = 512
max_entry_size_mb = 50
```

**Medium deployment (1000-10000 req/s):**
```toml
[cache]
max_size_mb = 2048
max_entry_size_mb = 100
```

**Large deployment (> 10000 req/s):**
```toml
[cache]
max_size_mb = 8192
max_entry_size_mb = 200
```

### TTL Strategies

**Aggressive caching:**
```toml
[cache]
default_ttl_secs = 7200      # 2 hours
max_ttl_secs = 604800        # 1 week
stale_while_revalidate_secs = 300
```

**Conservative caching:**
```toml
[cache]
default_ttl_secs = 300       # 5 minutes
max_ttl_secs = 3600          # 1 hour
stale_while_revalidate_secs = 30
```

**Dynamic content:**
```toml
[cache]
default_ttl_secs = 60        # 1 minute
max_ttl_secs = 300           # 5 minutes
stale_while_revalidate_secs = 15
```

## Logging Configuration

Controls logging output and format.

```toml
[logging]
level = "info"
json_format = false
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `level` | string | `"info"` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `json_format` | boolean | `false` | Output logs in JSON format for log aggregation systems |

### Log Levels

**Development:**
```toml
[logging]
level = "debug"
json_format = false
```

**Production:**
```toml
[logging]
level = "info"
json_format = true
```

**Troubleshooting:**
```toml
[logging]
level = "trace"
json_format = false
```

### Log Output Examples

**Standard format:**
```
2026-01-18T12:00:00Z INFO screaming_eagle::handlers: Processing request request_id=550e8400-e29b-41d4-a716-446655440000
2026-01-18T12:00:00Z INFO screaming_eagle::cache: Cache hit cache_key=example:/index.html
```

**JSON format:**
```json
{"timestamp":"2026-01-18T12:00:00Z","level":"INFO","target":"screaming_eagle::handlers","message":"Processing request","request_id":"550e8400-e29b-41d4-a716-446655440000"}
{"timestamp":"2026-01-18T12:00:00Z","level":"INFO","target":"screaming_eagle::cache","message":"Cache hit","cache_key":"example:/index.html"}
```

## Rate Limiting

Controls request rate limiting per client IP.

```toml
[rate_limit]
enabled = true
requests_per_window = 1000
window_secs = 60
burst_size = 50
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable or disable rate limiting |
| `requests_per_window` | integer | `1000` | Maximum requests allowed per window per IP |
| `window_secs` | integer | `60` | Window duration in seconds |
| `burst_size` | integer | `50` | Additional burst allowance above steady rate |

### Rate Calculation

Effective rate limit = `requests_per_window / window_secs` requests per second

**Example:**
```toml
requests_per_window = 1000
window_secs = 60
```
= 16.67 requests/second with burst up to 50 extra requests

### Common Configurations

**Restrictive (API protection):**
```toml
[rate_limit]
enabled = true
requests_per_window = 100
window_secs = 60
burst_size = 10
```

**Moderate (normal CDN):**
```toml
[rate_limit]
enabled = true
requests_per_window = 1000
window_secs = 60
burst_size = 50
```

**Permissive (high-traffic):**
```toml
[rate_limit]
enabled = true
requests_per_window = 10000
window_secs = 60
burst_size = 500
```

**Disabled:**
```toml
[rate_limit]
enabled = false
```

## Circuit Breaker

Protects origins from cascading failures.

```toml
[circuit_breaker]
failure_threshold = 5
reset_timeout_secs = 30
success_threshold = 3
failure_window_secs = 60
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `failure_threshold` | integer | `5` | Number of failures before opening circuit |
| `reset_timeout_secs` | integer | `30` | Seconds to wait before attempting half-open |
| `success_threshold` | integer | `3` | Consecutive successes needed to close circuit from half-open |
| `failure_window_secs` | integer | `60` | Time window for counting failures |

### Behavior

**Closed (Normal):**
- Requests pass through
- Failures counted
- Opens after `failure_threshold` failures in `failure_window_secs`

**Open (Failing):**
- Requests fail immediately with 503
- Stale content served if available
- Transitions to Half-Open after `reset_timeout_secs`

**Half-Open (Testing):**
- Limited test requests allowed
- Closes after `success_threshold` successes
- Opens immediately on any failure

### Tuning Guidelines

**Sensitive (quick to protect):**
```toml
[circuit_breaker]
failure_threshold = 3
reset_timeout_secs = 60
success_threshold = 2
failure_window_secs = 30
```

**Balanced:**
```toml
[circuit_breaker]
failure_threshold = 5
reset_timeout_secs = 30
success_threshold = 3
failure_window_secs = 60
```

**Tolerant (slow to open):**
```toml
[circuit_breaker]
failure_threshold = 10
reset_timeout_secs = 15
success_threshold = 5
failure_window_secs = 120
```

## TLS/HTTPS

Enable HTTPS with TLS certificates.

```toml
[tls]
cert_path = "/path/to/cert.pem"
key_path = "/path/to/key.pem"
```

### Options

| Option | Type | Required | Description |
|--------|------|----------|-------------|
| `cert_path` | string | yes | Path to TLS certificate (PEM format) |
| `key_path` | string | yes | Path to private key (PEM format) |

### Examples

**Let's Encrypt:**
```toml
[tls]
cert_path = "/etc/letsencrypt/live/cdn.example.com/fullchain.pem"
key_path = "/etc/letsencrypt/live/cdn.example.com/privkey.pem"
```

**Self-signed (development):**
```toml
[tls]
cert_path = "config/cert.pem"
key_path = "config/key.pem"
```

**Disabled (HTTP only):**
```toml
# Comment out or remove [tls] section
```

### Certificate Requirements

- Format: PEM
- Certificate should include full chain
- Private key should be unencrypted
- Permissions: Readable by CDN process user

### Generating Self-Signed Certificate

```bash
openssl req -x509 -newkey rsa:4096 \
  -keyout config/key.pem \
  -out config/cert.pem \
  -days 365 -nodes \
  -subj "/CN=localhost"
```

## Origins

Configure upstream origin servers.

Each origin is defined with a name used in the URL path: `/<origin-name>/<path>`

```toml
[origins.example]
url = "https://example.com"
timeout_secs = 30
max_retries = 3

[origins.api]
url = "https://api.example.com"
host_header = "api.example.com"
timeout_secs = 10
max_retries = 2

[origins.api.headers]
Authorization = "Bearer token"
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `url` | string | required | Base URL of origin server (must include scheme) |
| `timeout_secs` | integer | `30` | Request timeout in seconds |
| `max_retries` | integer | `3` | Number of retry attempts on failure |
| `host_header` | string | from URL | Override Host header sent to origin |
| `headers` | table | `{}` | Default headers to include in origin requests |

### Examples

**Simple origin:**
```toml
[origins.website]
url = "https://www.example.com"
timeout_secs = 30
max_retries = 3
```

**API with authentication:**
```toml
[origins.api]
url = "https://api.example.com"
timeout_secs = 10
max_retries = 2

[origins.api.headers]
Authorization = "Bearer sk_live_abc123"
X-API-Version = "2024-01-01"
```

**S3 bucket:**
```toml
[origins.assets]
url = "https://my-bucket.s3.amazonaws.com"
timeout_secs = 60
max_retries = 3

[origins.assets.headers]
X-Amz-Content-Sha256 = "UNSIGNED-PAYLOAD"
```

**Multiple origins:**
```toml
[origins.web]
url = "https://web.example.com"
timeout_secs = 30

[origins.api]
url = "https://api.example.com"
timeout_secs = 10

[origins.media]
url = "https://media.example.com"
timeout_secs = 60

[origins.static]
url = "https://static.example.com"
timeout_secs = 30
```

### Usage

Access origins via:
- `GET http://cdn.example.com/web/index.html` → `https://web.example.com/index.html`
- `GET http://cdn.example.com/api/users` → `https://api.example.com/users`
- `GET http://cdn.example.com/media/video.mp4` → `https://media.example.com/video.mp4`

## Admin Configuration

Configure admin API access.

```toml
[admin]
token = "your-secret-token-here"
allowed_ips = ["10.0.0.0/8", "192.168.1.100"]
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `token` | string | required | Bearer token for admin API authentication |
| `allowed_ips` | array | `[]` | IP addresses/networks allowed to access admin API (empty = all) |

### Examples

**Token only (any IP):**
```toml
[admin]
token = "super-secret-token-12345"
```

**Token + IP allowlist:**
```toml
[admin]
token = "super-secret-token-12345"
allowed_ips = [
    "10.0.0.0/8",           # Private network
    "192.168.1.100",        # Specific admin IP
    "203.0.113.0/24"        # Office network
]
```

**Multiple tokens (workaround - use different deployments):**
Admin API only supports one token. For multiple tokens, use a reverse proxy with authentication.

### Security Best Practices

1. Use a strong, random token (32+ characters)
2. Rotate tokens regularly
3. Use IP allowlist in production
4. Never commit tokens to version control
5. Use environment variables for tokens

```bash
# Generate secure token
openssl rand -base64 32
```

## Security

Additional security features.

```toml
[security]
enable_security_headers = true
allowed_origins = ["https://example.com"]
blocked_ips = ["203.0.113.50"]
enable_request_signing = false
signing_secret = "secret-key"

[security.headers]
X-Content-Type-Options = "nosniff"
X-Frame-Options = "DENY"
X-XSS-Protection = "1; mode=block"
Strict-Transport-Security = "max-age=31536000"
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_security_headers` | boolean | `true` | Add security headers to responses |
| `allowed_origins` | array | `[]` | CORS allowed origins (empty = all) |
| `blocked_ips` | array | `[]` | IP addresses to block |
| `enable_request_signing` | boolean | `false` | Require HMAC request signatures |
| `signing_secret` | string | required if enabled | Secret key for HMAC signature verification |
| `headers` | table | default set | Custom security headers |

### Examples

**Basic security:**
```toml
[security]
enable_security_headers = true
```

**CORS restriction:**
```toml
[security]
enable_security_headers = true
allowed_origins = [
    "https://app.example.com",
    "https://www.example.com"
]
```

**IP blocking:**
```toml
[security]
blocked_ips = [
    "203.0.113.50",
    "198.51.100.0/24"
]
```

**Request signing:**
```toml
[security]
enable_request_signing = true
signing_secret = "your-hmac-secret-key"
```

**Custom headers:**
```toml
[security.headers]
X-Content-Type-Options = "nosniff"
X-Frame-Options = "SAMEORIGIN"
Content-Security-Policy = "default-src 'self'"
Permissions-Policy = "geolocation=(), microphone=()"
```

## Edge Processing

Configure URL rewriting and request transformation.

```toml
[[edge.rewrites]]
pattern = "^/old/(.*)$"
replacement = "/new/$1"

[[edge.header_transforms]]
action = "add"
name = "X-CDN-Version"
value = "1.0"

[[edge.routes]]
condition = { path = "^/api/.*" }
origin = "api"
```

### URL Rewrites

| Field | Type | Description |
|-------|------|-------------|
| `pattern` | regex | Regular expression to match against path |
| `replacement` | string | Replacement pattern (supports capture groups) |

**Examples:**

```toml
# Redirect old paths
[[edge.rewrites]]
pattern = "^/old/(.*)$"
replacement = "/new/$1"

# Add prefix
[[edge.rewrites]]
pattern = "^/images/(.*)$"
replacement = "/cdn/v2/images/$1"

# Remove prefix
[[edge.rewrites]]
pattern = "^/api/v1/(.*)$"
replacement = "/$1"
```

### Header Transformations

| Field | Type | Description |
|-------|------|-------------|
| `action` | string | Action: `add`, `remove`, `replace` |
| `name` | string | Header name |
| `value` | string | Header value (not used for `remove`) |

**Examples:**

```toml
# Add custom header
[[edge.header_transforms]]
action = "add"
name = "X-CDN-Provider"
value = "Screaming-Eagle"

# Remove header
[[edge.header_transforms]]
action = "remove"
name = "Server"

# Replace header
[[edge.header_transforms]]
action = "replace"
name = "Cache-Control"
value = "public, max-age=3600"
```

### Conditional Routing

| Field | Type | Description |
|-------|------|-------------|
| `condition` | object | Routing condition |
| `origin` | string | Origin to route to |

**Condition types:**

```toml
# Path-based
[[edge.routes]]
condition = { path = "^/api/.*" }
origin = "api"

# Header-based
[[edge.routes]]
condition = { header = { name = "X-Mobile", value = "true" } }
origin = "mobile"

# Method-based
[[edge.routes]]
condition = { method = "POST" }
origin = "api"
```

## Connection Pool

Configure HTTP client connection pooling.

```toml
[connection_pool]
max_idle_per_host = 32
idle_timeout_secs = 90
connect_timeout_secs = 10
pool_max_idle_per_host = 32
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_idle_per_host` | integer | `32` | Maximum idle connections per host |
| `idle_timeout_secs` | integer | `90` | Idle connection timeout |
| `connect_timeout_secs` | integer | `10` | Connection establishment timeout |
| `pool_max_idle_per_host` | integer | `32` | Per-host connection pool size |

### Tuning

**Low traffic:**
```toml
[connection_pool]
max_idle_per_host = 8
idle_timeout_secs = 60
```

**High traffic:**
```toml
[connection_pool]
max_idle_per_host = 64
idle_timeout_secs = 120
```

## Health Checks

Configure origin health checking.

```toml
[health_checks]
enabled = true
interval_secs = 30
timeout_secs = 5
unhealthy_threshold = 3
healthy_threshold = 2

[health_checks.endpoints]
example = "/health"
api = "/api/health"
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable background health checks |
| `interval_secs` | integer | `30` | Check interval in seconds |
| `timeout_secs` | integer | `5` | Health check timeout |
| `unhealthy_threshold` | integer | `3` | Failures before marking unhealthy |
| `healthy_threshold` | integer | `2` | Successes before marking healthy |
| `endpoints` | table | `{}` | Health check paths per origin |

### Example

```toml
[health_checks]
enabled = true
interval_secs = 15
timeout_secs = 3
unhealthy_threshold = 2
healthy_threshold = 3

[health_checks.endpoints]
api = "/health"
web = "/healthz"
media = "/ping"
```

## Metrics

Configure Prometheus metrics.

```toml
[metrics]
enabled = true
prefix = "cdn"
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable metrics collection |
| `prefix` | string | `"cdn"` | Metric name prefix |

### Metrics Exposed

With prefix `cdn`:
- `cdn_requests_total`
- `cdn_cache_hits_total`
- `cdn_cache_misses_total`
- `cdn_request_duration_seconds`
- `cdn_cache_size_bytes`
- `cdn_origin_bytes_total`

## Environment Variables

Override configuration with environment variables.

### Format

```bash
CDN_<SECTION>_<KEY>=value
```

### Examples

```bash
# Server configuration
export CDN_SERVER_HOST="0.0.0.0"
export CDN_SERVER_PORT="8080"

# Cache configuration
export CDN_CACHE_MAX_SIZE_MB="2048"
export CDN_CACHE_DEFAULT_TTL_SECS="7200"

# Admin token
export CDN_ADMIN_TOKEN="secret-token"

# Logging
export CDN_LOGGING_LEVEL="debug"
export CDN_LOGGING_JSON_FORMAT="true"

# Rate limiting
export CDN_RATE_LIMIT_ENABLED="false"
```

### Precedence

Environment variables override configuration file values.

## Complete Example

Full configuration with all sections:

```toml
# Server configuration
[server]
host = "0.0.0.0"
port = 8080
workers = 8
request_timeout_secs = 30

# Cache configuration
[cache]
max_size_mb = 4096
max_entry_size_mb = 200
default_ttl_secs = 3600
max_ttl_secs = 86400
stale_while_revalidate_secs = 300
respect_cache_control = true

# Logging
[logging]
level = "info"
json_format = true

# Rate limiting
[rate_limit]
enabled = true
requests_per_window = 5000
window_secs = 60
burst_size = 200

# Circuit breaker
[circuit_breaker]
failure_threshold = 5
reset_timeout_secs = 30
success_threshold = 3
failure_window_secs = 60

# TLS
[tls]
cert_path = "/etc/letsencrypt/live/cdn.example.com/fullchain.pem"
key_path = "/etc/letsencrypt/live/cdn.example.com/privkey.pem"

# Admin API
[admin]
token = "super-secret-token-12345"
allowed_ips = ["10.0.0.0/8", "192.168.1.100"]

# Security
[security]
enable_security_headers = true
allowed_origins = ["https://example.com"]
enable_request_signing = false

[security.headers]
X-Content-Type-Options = "nosniff"
X-Frame-Options = "DENY"
Strict-Transport-Security = "max-age=31536000"

# Origins
[origins.web]
url = "https://www.example.com"
timeout_secs = 30
max_retries = 3

[origins.api]
url = "https://api.example.com"
timeout_secs = 10
max_retries = 2

[origins.api.headers]
Authorization = "Bearer token"

[origins.media]
url = "https://media.example.com"
timeout_secs = 60
max_retries = 3

# Edge processing
[[edge.rewrites]]
pattern = "^/old/(.*)$"
replacement = "/new/$1"

[[edge.header_transforms]]
action = "add"
name = "X-CDN-Version"
value = "1.0"

[[edge.routes]]
condition = { path = "^/api/.*" }
origin = "api"

# Connection pool
[connection_pool]
max_idle_per_host = 32
idle_timeout_secs = 90
connect_timeout_secs = 10

# Health checks
[health_checks]
enabled = true
interval_secs = 30
timeout_secs = 5
unhealthy_threshold = 3
healthy_threshold = 2

[health_checks.endpoints]
web = "/health"
api = "/api/health"

# Metrics
[metrics]
enabled = true
prefix = "cdn"
```

## Validation

The CDN validates configuration on startup. Common errors:

**Missing required field:**
```
Error: Missing required configuration: origins
```

**Invalid value:**
```
Error: Invalid port number: must be between 1 and 65535
```

**Invalid URL:**
```
Error: Invalid origin URL 'not-a-url': missing scheme
```

**File not found:**
```
Error: TLS certificate not found: /path/to/cert.pem
```

## Configuration Tips

1. **Start with defaults**: Use example config and modify as needed
2. **Test changes**: Validate config before deploying
3. **Monitor metrics**: Adjust based on actual usage patterns
4. **Use version control**: Track configuration changes
5. **Document customizations**: Note why settings differ from defaults
6. **Separate environments**: Use different configs for dev/staging/prod
7. **Secure secrets**: Never commit tokens/keys to version control
8. **Regular review**: Revisit config as traffic patterns change
