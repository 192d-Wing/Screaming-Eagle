# Architecture Deep Dive

This document provides a comprehensive technical overview of Screaming Eagle CDN's architecture, design decisions, and implementation details.

## Table of Contents

- [High-Level Architecture](#high-level-architecture)
- [Request Flow](#request-flow)
- [Core Components](#core-components)
- [Concurrency Model](#concurrency-model)
- [Cache Implementation](#cache-implementation)
- [Circuit Breaker Pattern](#circuit-breaker-pattern)
- [Rate Limiting](#rate-limiting)
- [Request Coalescing](#request-coalescing)
- [Edge Processing](#edge-processing)
- [Security Architecture](#security-architecture)
- [Observability](#observability)
- [Performance Optimizations](#performance-optimizations)
- [Design Decisions](#design-decisions)
- [Future Enhancements](#future-enhancements)

## High-Level Architecture

Screaming Eagle CDN follows a **layered reverse proxy architecture** built on Rust's async ecosystem.

```
                    ┌─────────────────────────────────────┐
                    │        HTTP Client Request          │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │      Rate Limiter Middleware        │
                    │   (Token Bucket per Client IP)      │
                    └──────────────┬──────────────────────┘
                                   │
                    ┌──────────────▼──────────────────────┐
                    │         Axum Router Layer           │
                    │  ┌──────────────────────────────┐   │
                    │  │  Tower HTTP Middleware:      │   │
                    │  │  - Compression (gzip/br)     │   │
                    │  │  - CORS Handling             │   │
                    │  │  - Security Headers          │   │
                    │  │  - Tracing & Request ID      │   │
                    │  └──────────────────────────────┘   │
                    └──────────────┬──────────────────────┘
                                   │
              ┌────────────────────┴────────────────────┐
              │                                         │
    ┌─────────▼──────────┐              ┌─────────────▼─────────┐
    │   CDN Handler      │              │    Admin API          │
    │  (Proxy Logic)     │              │  (/_cdn/* endpoints)  │
    └─────────┬──────────┘              └───────────────────────┘
              │
    ┌─────────▼──────────┐
    │   Edge Processing  │
    │  - URL Rewriting   │
    │  - Header Xform    │
    │  - Routing Logic   │
    └─────────┬──────────┘
              │
    ┌─────────▼──────────┐
    │    Cache Layer     │
    │   (DashMap LRU-K)  │
    │  - Hash Lookup     │
    │  - TTL Check       │
    │  - Vary Support    │
    └─────────┬──────────┘
              │
         Cache Hit? ────Yes──> Return Cached Response
              │
              No
              │
    ┌─────────▼──────────┐
    │ Request Coalescer  │
    │  (Deduplication)   │
    └─────────┬──────────┘
              │
    ┌─────────▼──────────┐
    │  Circuit Breaker   │
    │   (Per Origin)     │
    └─────────┬──────────┘
              │
         CB Closed?
              │
              Yes
              │
    ┌─────────▼──────────┐
    │  Origin Fetcher    │
    │  (reqwest client)  │
    │  - HTTP/2          │
    │  - Conn Pooling    │
    │  - Compression     │
    └─────────┬──────────┘
              │
    ┌─────────▼──────────┐
    │   Origin Server    │
    └────────────────────┘
```

### Technology Stack

**Runtime:**

- **Tokio**: Async runtime with work-stealing scheduler
- **Axum**: Web framework built on Tower and Hyper
- **Tower**: Middleware composition via Service trait

**Data Structures:**

- **DashMap**: Lock-free concurrent HashMap
- **Arc**: Atomic reference counting for shared state
- **RwLock**: Read-write locks where needed

**HTTP:**

- **reqwest**: Async HTTP client
- **Hyper**: Underlying HTTP server
- **tower-http**: HTTP-specific middleware

## Request Flow

Detailed flow through the CDN for a typical GET request:

### 1. Connection Acceptance

```rust
// In main.rs
let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
let listener = TcpListener::bind(addr).await?;

axum::serve(listener, app.into_make_service())
    .await?;
```

**Process:**

- Tokio accepts TCP connection
- TLS handshake if HTTPS enabled
- HTTP/1.1 or HTTP/2 negotiation

### 2. Rate Limiting

```rust
// In rate_limit.rs
let mut bucket = self.buckets.entry(client_ip)
    .or_insert_with(|| TokenBucket::new(config));

if !bucket.allow() {
    return Err(StatusCode::TOO_MANY_REQUESTS);
}
```

**Process:**

- Extract client IP from connection/X-Forwarded-For
- Token bucket lookup (or create)
- Check if tokens available
- Consume token or reject with 429

### 3. Middleware Pipeline

```rust
// In main.rs
let app = Router::new()
    .layer(TraceLayer::new_for_http())
    .layer(CorsLayer::permissive())
    .layer(CompressionLayer::new())
    .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
    .layer(SetResponseHeaderLayer::if_not_present(...));
```

**Process:**

- Request ID generation (UUID v4)
- Tracing span creation
- CORS preflight handling
- Security header preparation

### 4. Routing

```rust
// In main.rs
let app = Router::new()
    .route("/_cdn/health", get(health_handler))
    .route("/_cdn/stats", get(stats_handler))
    .route("/*path", get(cdn_handler).head(cdn_handler));
```

**Process:**

- Pattern match on path
- Admin endpoints require auth
- Proxy endpoints go to cdn_handler

### 5. Edge Processing

```rust
// In edge.rs
pub fn apply_edge_rules(req: &Request, config: &EdgeConfig) -> EdgeResult {
    // URL rewriting
    let rewritten_url = apply_rewrites(&req.uri(), &config.rewrites)?;

    // Header transformations
    let headers = transform_headers(&req.headers(), &config.transforms)?;

    // Conditional routing
    let origin = select_origin(&req, &config.routes)?;

    Ok(EdgeResult { url: rewritten_url, headers, origin })
}
```

**Process:**

- Apply URL rewrite rules (regex-based)
- Transform headers (add/remove/modify)
- Normalize query parameters
- Select origin based on conditions

### 6. Cache Lookup

```rust
// In cache.rs
pub fn get(&self, key: &str) -> Option<CachedResponse> {
    let entry = self.store.get(key)?;

    // Check expiration
    if entry.is_expired() {
        self.store.remove(key);
        return None;
    }

    // Update LRU-K tracking
    self.update_access_count(key);

    Some(entry.response.clone())
}
```

**Process:**

- Generate cache key from request (see [Cache Key Generation](#cache-key-generation))
- DashMap lookup (O(1) amortized)
- TTL expiration check
- Update access count for LRU-K
- Return cached response or None

### 7. Request Coalescing

```rust
// In coalesce.rs
pub async fn fetch_or_wait(&self, key: &str, fetcher: F) -> Result<Response>
where
    F: Future<Output = Result<Response>>,
{
    // Check if already fetching
    if let Some(tx) = self.inflight.get(key) {
        // Subscribe to broadcast channel
        let mut rx = tx.subscribe();
        return rx.recv().await;
    }

    // Create broadcast channel
    let (tx, _) = broadcast::channel(1024);
    self.inflight.insert(key.to_string(), tx.clone());

    // Perform fetch
    let response = fetcher.await?;

    // Broadcast to all waiters
    let _ = tx.send(response.clone());

    // Remove from inflight
    self.inflight.remove(key);

    Ok(response)
}
```

**Process:**

- Check if request for same resource in-flight
- If yes, subscribe to broadcast channel and wait
- If no, create channel and perform fetch
- Broadcast response to all waiting tasks
- All tasks receive same response

### 8. Circuit Breaker Check

```rust
// In circuit_breaker.rs
pub fn call<F, Fut>(&self, f: F) -> Result<Fut::Output>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Response>>,
{
    let state = self.state.read().unwrap();

    match *state {
        State::Open => {
            // Check if timeout elapsed
            if state.should_attempt_reset() {
                *self.state.write().unwrap() = State::HalfOpen;
            } else {
                return Err(Error::CircuitBreakerOpen);
            }
        }
        State::HalfOpen => {
            // Limited attempts allowed
        }
        State::Closed => {
            // Normal operation
        }
    }

    let result = f().await;
    self.record_result(&result);
    result
}
```

**Process:**

- Check circuit breaker state
- If Open, fail-fast or serve stale content
- If HalfOpen, allow test request
- If Closed, proceed normally
- Record success/failure
- Update state if thresholds crossed

### 9. Origin Fetch

```rust
// In origin.rs
pub async fn fetch(&self, origin: &str, path: &str) -> Result<Response> {
    let config = self.origins.get(origin)?;
    let url = format!("{}{}", config.url, path);

    let response = self.client
        .get(&url)
        .timeout(Duration::from_secs(config.timeout))
        .headers(config.default_headers.clone())
        .send()
        .await?;

    Ok(response)
}
```

**Process:**

- Build full URL from origin + path
- Use reqwest client (connection pooled)
- Apply timeout from config
- Add default headers
- Send HTTP/2 request if supported
- Receive and parse response

### 10. Response Processing

```rust
// In handlers.rs
async fn process_origin_response(response: Response) -> CdnResponse {
    // Parse Cache-Control
    let ttl = parse_cache_control(&response.headers());

    // Generate ETag
    let body = response.bytes().await?;
    let etag = generate_etag(&body);

    // Create cache entry
    let cached = CachedResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: body.clone(),
        etag,
        cached_at: Utc::now(),
        expires_at: Utc::now() + Duration::seconds(ttl),
    };

    // Store in cache
    cache.insert(cache_key, cached);

    // Build response
    build_response(body, headers, etag)
}
```

**Process:**

- Parse Cache-Control header
- Determine TTL
- Generate ETag using xxHash3
- Create cache entry
- Store in cache with TTL
- Add CDN headers (X-Cache, Age, Via)
- Return to client

## Core Components

### Main Application (main.rs)

**Responsibilities:**

- Configuration loading
- Tokio runtime initialization
- Middleware stack assembly
- Route registration
- Graceful shutdown handling

**Key Code:**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // Load config
    let config = Config::from_file("config/cdn.toml")?;

    // Initialize shared state
    let cache = Arc::new(Cache::new(config.cache.clone()));
    let metrics = Arc::new(Metrics::new());
    let rate_limiter = Arc::new(RateLimiter::new(config.rate_limit.clone()));

    // Build router
    let app = Router::new()
        .route("/*path", get(cdn_handler))
        .layer(/* middleware */)
        .with_state(AppState { cache, metrics, config });

    // Serve
    axum::serve(listener, app).await
}
```

### Handler Layer (handlers.rs)

**Handlers:**

- `cdn_handler`: Main proxy logic
- `health_handler`: Health check endpoint
- `stats_handler`: Cache statistics
- `purge_handler`: Cache invalidation
- `metrics_handler`: Prometheus metrics export

**cdn_handler Flow:**

```rust
pub async fn cdn_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response> {
    // Extract origin and path
    let (origin, path) = parse_request(&req)?;

    // Apply edge processing
    let edge_result = edge::apply_rules(&req, &state.config.edge)?;

    // Check cache
    let cache_key = generate_cache_key(&origin, &path, &req.headers());
    if let Some(cached) = state.cache.get(&cache_key) {
        return Ok(cached.into_response().with_header("X-Cache", "HIT"));
    }

    // Coalesce requests
    let response = state.coalescer.fetch_or_wait(cache_key, async {
        // Use circuit breaker
        state.circuit_breakers.get(origin).call(async {
            // Fetch from origin
            state.origin_fetcher.fetch(origin, &path).await
        }).await
    }).await?;

    // Process and cache
    let cached_response = process_response(response).await?;
    state.cache.insert(cache_key, cached_response.clone());

    Ok(cached_response.into_response().with_header("X-Cache", "MISS"))
}
```

### Cache (cache.rs)

**Data Structure:**

```rust
pub struct Cache {
    store: Arc<DashMap<String, CacheEntry>>,
    max_size: usize,
    current_size: AtomicUsize,
    config: CacheConfig,
}

struct CacheEntry {
    response: CachedResponse,
    cached_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    access_count: AtomicU64,
    last_accessed: AtomicI64,
    size: usize,
}
```

**Key Operations:**

1. **Insert:**

   ```rust
   pub fn insert(&self, key: String, response: CachedResponse) {
       let size = response.body.len();

       // Evict if necessary
       while self.current_size.load(Ordering::Relaxed) + size > self.max_size {
           self.evict_lru_k();
       }

       let entry = CacheEntry {
           response,
           cached_at: Utc::now(),
           expires_at: Utc::now() + Duration::seconds(ttl),
           access_count: AtomicU64::new(1),
           last_accessed: AtomicI64::new(Utc::now().timestamp()),
           size,
       };

       self.store.insert(key, entry);
       self.current_size.fetch_add(size, Ordering::Relaxed);
   }
   ```

2. **Eviction (LRU-K):**

   ```rust
   fn evict_lru_k(&self) {
       let mut candidates: Vec<_> = self.store.iter()
           .map(|entry| {
               let score = entry.access_count.load(Ordering::Relaxed) as f64
                   / (Utc::now().timestamp() - entry.last_accessed.load(Ordering::Relaxed)) as f64;
               (entry.key().clone(), score)
           })
           .collect();

       candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

       if let Some((key, _)) = candidates.first() {
           self.remove(key);
       }
   }
   ```

3. **TTL Management:**

   ```rust
   fn is_expired(&self, entry: &CacheEntry) -> bool {
       Utc::now() > entry.expires_at
   }

   fn is_stale(&self, entry: &CacheEntry) -> bool {
       Utc::now() > entry.expires_at
           && Utc::now() < entry.expires_at + Duration::seconds(self.config.stale_window)
   }
   ```

## Concurrency Model

### Async Runtime

Screaming Eagle uses Tokio's **multi-threaded work-stealing scheduler**:

```rust
#[tokio::main]
async fn main() {
    // Tokio automatically configures worker threads
    // based on CPU cores
}
```

**Worker Thread Pool:**

- Default: Number of CPU cores
- Configurable via `TOKIO_WORKER_THREADS` env var
- Work-stealing for load balancing

### Lock-Free Data Structures

**DashMap for Cache:**

```rust
// DashMap uses internal sharding to minimize contention
// Each shard has its own RwLock
// Hash determines shard (lock-free for different shards)

pub struct DashMap<K, V> {
    shards: Box<[RwLock<HashMap<K, V>>]>,  // Typically 16-64 shards
    hasher: RandomState,
}
```

**Benefits:**

- Lock-free for operations on different shards
- Fine-grained locking within shards
- Better scaling than single lock

**Atomics for Counters:**

```rust
// Metrics use atomic counters for lock-free updates
pub struct Metrics {
    requests: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

impl Metrics {
    pub fn inc_requests(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
}
```

### Task Spawning

**Background Tasks:**

```rust
// Health checks run as background tasks
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        health_checker.check_all().await;
    }
});
```

**Request Handling:**

```rust
// Each request is handled in its own task (via Axum)
// No explicit spawning needed - Axum handles this
```

## Cache Implementation

### Cache Key Generation

```rust
pub fn generate_cache_key(
    origin: &str,
    path: &str,
    headers: &HeaderMap,
) -> String {
    let mut key = format!("{}:{}", origin, path);

    // Include query parameters (normalized)
    if let Some(query) = parse_query(path) {
        let normalized = normalize_query(query);
        key.push('?');
        key.push_str(&normalized);
    }

    // Include Vary headers
    if let Some(vary) = headers.get("Vary") {
        for header_name in vary.to_str().unwrap().split(',') {
            if let Some(value) = headers.get(header_name.trim()) {
                key.push('|');
                key.push_str(header_name);
                key.push('=');
                key.push_str(value.to_str().unwrap());
            }
        }
    }

    key
}
```

**Example Keys:**

- `example:/index.html`
- `api:/users?id=123&sort=name`
- `cdn:/image.jpg|Accept-Encoding=gzip`

### TTL Calculation

```rust
pub fn calculate_ttl(headers: &HeaderMap, config: &CacheConfig) -> u64 {
    // 1. Check Cache-Control: max-age
    if let Some(cc) = headers.get("Cache-Control") {
        if let Some(max_age) = parse_max_age(cc) {
            return max_age;
        }
    }

    // 2. Check Expires header
    if let Some(expires) = headers.get("Expires") {
        if let Ok(exp_time) = parse_http_date(expires) {
            return (exp_time - Utc::now()).num_seconds() as u64;
        }
    }

    // 3. Use default from config
    config.default_ttl
}
```

### Stale Content Handling

```rust
pub fn handle_stale(
    entry: &CacheEntry,
    config: &CacheConfig,
) -> StaleAction {
    let age = (Utc::now() - entry.cached_at).num_seconds();
    let ttl = (entry.expires_at - entry.cached_at).num_seconds();

    // Check stale-while-revalidate
    if age < ttl + config.stale_while_revalidate {
        return StaleAction::ServeAndRevalidate;
    }

    // Check stale-if-error (only on origin failure)
    if age < ttl + config.stale_if_error {
        return StaleAction::ServeIfError;
    }

    StaleAction::DoNotServe
}
```

## Circuit Breaker Pattern

### State Machine

```rust
pub enum CircuitBreakerState {
    Closed {
        failure_count: u32,
    },
    Open {
        opened_at: DateTime<Utc>,
    },
    HalfOpen {
        success_count: u32,
        test_requests: u32,
    },
}
```

**State Transitions:**

```
         Failure threshold reached
    Closed ───────────────────────> Open
      ▲                               │
      │                               │
      │                               │ Timeout elapsed
      │                               │
      │                               ▼
      └─── Success threshold ──── HalfOpen
                                      │
                                      │ Failure
                                      └──────> Open
```

### Implementation

```rust
impl CircuitBreaker {
    pub async fn call<F, Fut>(&self, f: F) -> Result<Fut::Output>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Response>>,
    {
        // Check state
        let state = self.state.read().unwrap().clone();

        match state {
            State::Closed { failure_count } => {
                let result = f().await;

                if result.is_err() {
                    let new_count = failure_count + 1;
                    if new_count >= self.config.failure_threshold {
                        *self.state.write().unwrap() = State::Open {
                            opened_at: Utc::now(),
                        };
                    } else {
                        *self.state.write().unwrap() = State::Closed {
                            failure_count: new_count,
                        };
                    }
                } else {
                    // Reset on success
                    *self.state.write().unwrap() = State::Closed {
                        failure_count: 0,
                    };
                }

                result
            }

            State::Open { opened_at } => {
                // Check if should transition to HalfOpen
                if Utc::now() - opened_at > Duration::seconds(self.config.timeout) {
                    *self.state.write().unwrap() = State::HalfOpen {
                        success_count: 0,
                        test_requests: 0,
                    };
                    return self.call(f).await;
                }

                // Fail fast
                Err(Error::CircuitBreakerOpen)
            }

            State::HalfOpen { success_count, test_requests } => {
                // Limit concurrent test requests
                if test_requests >= self.config.half_open_max_requests {
                    return Err(Error::CircuitBreakerOpen);
                }

                let result = f().await;

                if result.is_ok() {
                    let new_success = success_count + 1;
                    if new_success >= self.config.success_threshold {
                        *self.state.write().unwrap() = State::Closed {
                            failure_count: 0,
                        };
                    } else {
                        *self.state.write().unwrap() = State::HalfOpen {
                            success_count: new_success,
                            test_requests: test_requests + 1,
                        };
                    }
                } else {
                    *self.state.write().unwrap() = State::Open {
                        opened_at: Utc::now(),
                    };
                }

                result
            }
        }
    }
}
```

## Rate Limiting

### Token Bucket Algorithm

```rust
pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64,  // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    pub fn allow(&mut self) -> bool {
        // Refill tokens based on elapsed time
        let now = Instant::now();
        let elapsed = (now - self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;

        // Try to consume a token
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
```

### Per-IP Tracking

```rust
pub struct RateLimiter {
    buckets: DashMap<IpAddr, TokenBucket>,
    config: RateLimitConfig,
}

impl RateLimiter {
    pub fn check(&self, ip: IpAddr) -> Result<()> {
        let mut bucket = self.buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(
                self.config.burst_size,
                self.config.requests_per_window as f64 / self.config.window_seconds as f64,
            ));

        if bucket.allow() {
            Ok(())
        } else {
            Err(Error::RateLimitExceeded)
        }
    }
}
```

## Request Coalescing

### Broadcast Channel Pattern

```rust
pub struct RequestCoalescer {
    // Map of cache key -> broadcast sender
    inflight: Arc<DashMap<String, broadcast::Sender<Response>>>,
}

impl RequestCoalescer {
    pub async fn fetch_or_wait<F>(
        &self,
        key: String,
        fetcher: F,
    ) -> Result<Response>
    where
        F: Future<Output = Result<Response>>,
    {
        // Try to get existing fetch
        if let Some(tx) = self.inflight.get(&key) {
            let mut rx = tx.subscribe();

            match rx.recv().await {
                Ok(response) => return Ok(response),
                Err(_) => {
                    // Channel closed, fall through to fetch
                }
            }
        }

        // Create new fetch
        let (tx, _rx) = broadcast::channel(1024);

        // Insert into inflight (may race with another task)
        if let Some(existing) = self.inflight.insert(key.clone(), tx.clone()) {
            // Another task won the race, subscribe to theirs
            let mut rx = existing.subscribe();
            return rx.recv().await.map_err(|_| Error::CoalesceError);
        }

        // We won the race, perform fetch
        let response = fetcher.await?;

        // Broadcast to all waiters
        let _ = tx.send(response.clone());

        // Remove from inflight
        self.inflight.remove(&key);

        Ok(response)
    }
}
```

### Thundering Herd Prevention

**Without Coalescing:**

```
100 clients request /popular.jpg (not cached)
    ↓
100 requests to origin server
    ↓
Origin overloaded
```

**With Coalescing:**

```
100 clients request /popular.jpg (not cached)
    ↓
1 request to origin server
99 requests wait on broadcast channel
    ↓
1 response fetched
    ↓
100 clients receive same response
```

## Edge Processing

### URL Rewriting

```rust
pub struct RewriteRule {
    pattern: Regex,
    replacement: String,
}

pub fn apply_rewrites(path: &str, rules: &[RewriteRule]) -> String {
    let mut result = path.to_string();

    for rule in rules {
        if rule.pattern.is_match(&result) {
            result = rule.pattern.replace(&result, &rule.replacement).to_string();
        }
    }

    result
}
```

**Example Rules:**

```toml
[[edge.rewrites]]
pattern = "^/old/(.*)$"
replacement = "/new/$1"

[[edge.rewrites]]
pattern = "^/images/(.+)\\.(jpg|png)$"
replacement = "/cdn/images/$1.$2"
```

### Header Transformations

```rust
pub fn transform_headers(
    headers: &HeaderMap,
    transforms: &[HeaderTransform],
) -> HeaderMap {
    let mut result = headers.clone();

    for transform in transforms {
        match transform.action {
            Action::Add => {
                result.insert(&transform.name, transform.value.clone());
            }
            Action::Remove => {
                result.remove(&transform.name);
            }
            Action::Replace => {
                result.remove(&transform.name);
                result.insert(&transform.name, transform.value.clone());
            }
        }
    }

    result
}
```

### Conditional Routing

```rust
pub fn select_origin(
    req: &Request,
    routes: &[Route],
) -> Option<String> {
    for route in routes {
        if route.condition.matches(req) {
            return Some(route.origin.clone());
        }
    }

    None
}

impl Condition {
    fn matches(&self, req: &Request) -> bool {
        match self {
            Condition::Path(pattern) => pattern.is_match(req.uri().path()),
            Condition::Header { name, value } => {
                req.headers().get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v == value)
                    .unwrap_or(false)
            }
            Condition::Method(method) => req.method() == method,
        }
    }
}
```

## Security Architecture

### Authentication

```rust
pub fn verify_bearer_token(
    headers: &HeaderMap,
    config: &AdminConfig,
) -> Result<()> {
    let auth_header = headers
        .get("Authorization")
        .ok_or(Error::Unauthorized)?
        .to_str()
        .map_err(|_| Error::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(Error::Unauthorized)?;

    if token == config.token {
        Ok(())
    } else {
        Err(Error::Unauthorized)
    }
}
```

### IP Access Control

```rust
pub fn check_ip_allowlist(
    ip: IpAddr,
    allowlist: &[IpNetwork],
) -> Result<()> {
    if allowlist.is_empty() {
        return Ok(());  // No restrictions
    }

    for network in allowlist {
        if network.contains(ip) {
            return Ok(());
        }
    }

    Err(Error::Forbidden)
}
```

### Request Signing

```rust
pub fn verify_signature(
    req: &Request,
    secret: &[u8],
) -> Result<()> {
    let signature = req.headers()
        .get("X-Signature")
        .ok_or(Error::InvalidSignature)?;

    let body = req.body();
    let timestamp = req.headers()
        .get("X-Timestamp")
        .ok_or(Error::InvalidSignature)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|_| Error::InvalidSignature)?;
    mac.update(timestamp.as_bytes());
    mac.update(body);

    let expected = mac.finalize().into_bytes();
    let provided = base64::decode(signature.as_bytes())
        .map_err(|_| Error::InvalidSignature)?;

    if expected.as_slice() == provided.as_slice() {
        Ok(())
    } else {
        Err(Error::InvalidSignature)
    }
}
```

## Observability

### Metrics Collection

```rust
pub struct Metrics {
    requests: Counter,
    cache_hits: Counter,
    cache_misses: Counter,
    duration: Histogram,
    cache_size: Gauge,
    origin_bytes: Counter,
}

impl Metrics {
    pub fn record_request(&self, method: &str, status: u16, duration: f64) {
        self.requests
            .with_label_values(&[method, &status.to_string()])
            .inc();

        self.duration
            .with_label_values(&[method, &status.to_string()])
            .observe(duration);
    }
}
```

### Structured Logging

```rust
#[instrument(skip(state))]
pub async fn cdn_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response> {
    let request_id = Uuid::new_v4();
    let span = info_span!("cdn_request", request_id = %request_id);

    async move {
        info!("Processing request");

        // ... handler logic ...

        info!(
            cache_status = ?cache_status,
            duration_ms = duration.as_millis(),
            "Request completed"
        );

        Ok(response)
    }
    .instrument(span)
    .await
}
```

### Distributed Tracing

```rust
// OpenTelemetry integration (optional)
use opentelemetry::trace::Tracer;

pub fn init_tracing() -> Result<()> {
    let tracer = opentelemetry_jaeger::new_pipeline()
        .with_service_name("screaming-eagle-cdn")
        .install_batch(opentelemetry::runtime::Tokio)?;

    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();

    Ok(())
}
```

## Performance Optimizations

### Connection Pooling

```rust
pub fn create_http_client(config: &ConnectionPoolConfig) -> Client {
    Client::builder()
        .pool_max_idle_per_host(config.max_idle_per_host)
        .pool_idle_timeout(Duration::from_secs(config.idle_timeout))
        .tcp_keepalive(Some(Duration::from_secs(60)))
        .http2_prior_knowledge()
        .build()
        .unwrap()
}
```

### Zero-Copy Operations

```rust
// Use Bytes for zero-copy body handling
pub struct CachedResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,  // Reference-counted, zero-copy
}

// Clone is cheap (just increments reference count)
let cached = cached_response.clone();
```

### Compile-Time Optimizations

```toml
# Cargo.toml
[profile.release]
lto = true              # Link-time optimization
codegen-units = 1       # Better optimization, slower compile
opt-level = 3           # Maximum optimization
strip = true            # Strip symbols
panic = 'abort'         # Smaller binary
```

## Design Decisions

### Why Rust?

1. **Memory Safety**: No GC pauses, no null pointer exceptions
2. **Performance**: Zero-cost abstractions, minimal runtime
3. **Concurrency**: Fearless concurrency with ownership system
4. **Ecosystem**: Excellent async ecosystem (Tokio, Axum)

### Why DashMap over RwLock<HashMap>?

**DashMap:**

- Sharded locks (better concurrency)
- Lock-free for operations on different shards
- Better scalability under high load

**RwLock<HashMap>:**

- Single lock (contention bottleneck)
- Readers block on writer
- Simpler but less scalable

### Why In-Memory Cache?

**Pros:**

- Sub-millisecond latency
- No network overhead
- Simple implementation
- No external dependencies

**Cons:**

- Limited capacity
- Not shared across instances
- Lost on restart

**Trade-off:** Optimized for edge caching where speed > capacity

### Why Token Bucket for Rate Limiting?

**Alternatives:**

- Fixed Window: Can have burst at window boundary
- Sliding Window: More memory overhead
- Leaky Bucket: Similar but less intuitive

**Token Bucket:**

- Allows controlled burst (UX friendly)
- Simple implementation
- Well-understood algorithm

## Future Enhancements

### Distributed Cache

Replace in-memory cache with distributed backend:

```rust
pub trait CacheBackend {
    async fn get(&self, key: &str) -> Option<CachedResponse>;
    async fn set(&self, key: &str, value: CachedResponse, ttl: u64);
    async fn delete(&self, key: &str);
}

// Implementations
impl CacheBackend for RedisCache { ... }
impl CacheBackend for MemcachedCache { ... }
impl CacheBackend for InMemoryCache { ... }
```

### Adaptive TTL

Machine learning-based TTL adjustment:

```rust
pub struct AdaptiveTTL {
    model: Box<dyn MLModel>,
}

impl AdaptiveTTL {
    pub fn predict_optimal_ttl(&self, features: &Features) -> u64 {
        // Features: request frequency, content type, time of day, etc.
        self.model.predict(features)
    }
}
```

### Content Prefetching

Predictive cache warming:

```rust
pub struct Prefetcher {
    predictor: Box<dyn Predictor>,
}

impl Prefetcher {
    pub async fn prefetch_related(&self, url: &str) {
        let related = self.predictor.predict_next(url);
        for url in related {
            self.warm_cache(url).await;
        }
    }
}
```

### Advanced Eviction Policies

- **LIRS**: Low Inter-reference Recency Set
- **ARC**: Adaptive Replacement Cache
- **2Q**: Two-Queue algorithm

### WebAssembly Edge Functions

Run custom logic at the edge:

```rust
pub async fn execute_edge_function(
    wasm: &[u8],
    request: &Request,
) -> Result<Response> {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, wasm)?;
    // ... execute WASM with request context
}
```

## Conclusion

Screaming Eagle CDN's architecture prioritizes:

1. **Performance**: Rust + async I/O + lock-free data structures
2. **Reliability**: Circuit breakers + health checks + stale content
3. **Scalability**: Horizontal scaling + connection pooling + efficient caching
4. **Observability**: Comprehensive metrics + structured logging + tracing
5. **Simplicity**: Clear separation of concerns + minimal dependencies

The design is production-ready while remaining extensible for future enhancements.
