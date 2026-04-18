use axum::{
    Router, middleware,
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use screaming_eagle::auth::{AdminAuth, admin_auth_middleware};
use screaming_eagle::cache::Cache;
use screaming_eagle::circuit_breaker::{self, CircuitBreakerManager};
use screaming_eagle::coalesce::RequestCoalescer;
use screaming_eagle::config::{self, Config};
use screaming_eagle::edge::{EdgeProcessor, edge_processing_middleware};
use screaming_eagle::error::init_error_pages;
use screaming_eagle::error_pages::ErrorPages;
use screaming_eagle::handlers::{
    self, AppState, cache_stats, cdn_handler, circuit_breaker_status, coalesce_stats, health,
    metrics as metrics_handler, origin_health_status, purge_cache, warm_cache,
};
use screaming_eagle::health::{HealthChecker, spawn_health_checks};
use screaming_eagle::metrics::Metrics;
use screaming_eagle::origin::OriginFetcher;
use screaming_eagle::rate_limit::{RateLimitConfig, RateLimiter};
use screaming_eagle::content::{ContentProcessor, content_processing_middleware};
use screaming_eagle::http3::{Http3Request, Http3Response, run_http3_server};
use screaming_eagle::network::{
    BindMode, build_listener, build_rustls_config, build_rustls_config_single,
};
use screaming_eagle::protocol::{
    Http3Config, SseConfig, WebSocketConfig, alt_svc_middleware, sse_middleware,
    websocket_proxy_handler,
};
use screaming_eagle::security::{
    Security, ip_access_control_middleware, request_signing_middleware, security_headers_middleware,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = load_config()?;

    // Initialize logging
    init_logging(&config.logging);

    info!(
        "Starting Screaming Eagle CDN v{}",
        env!("CARGO_PKG_VERSION")
    );

    // Initialize error pages
    let error_pages = ErrorPages::new(&config.error_pages);
    if error_pages.is_enabled() {
        let pages = error_pages.available_pages();
        info!("Custom error pages enabled ({} pages loaded)", pages.len());
    }
    init_error_pages(error_pages);

    // Initialize rate limiter
    let rate_limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        requests_per_window: config.rate_limit.requests_per_window,
        window_secs: config.rate_limit.window_secs,
        burst_size: config.rate_limit.burst_size,
        enabled: config.rate_limit.enabled,
    }));

    // Initialize circuit breaker manager
    let circuit_breaker = Arc::new(CircuitBreakerManager::new(
        circuit_breaker::CircuitBreakerConfig {
            failure_threshold: config.circuit_breaker.failure_threshold,
            reset_timeout_secs: config.circuit_breaker.reset_timeout_secs,
            success_threshold: config.circuit_breaker.success_threshold,
            failure_window_secs: config.circuit_breaker.failure_window_secs,
        },
    ));

    // Initialize other components
    let cache = Arc::new(Cache::new(config.cache.clone()));
    let origin = Arc::new(OriginFetcher::with_pool_config(
        config.origins.clone(),
        config.connection_pool.clone(),
    )?);
    let metrics = Arc::new(Metrics::new());
    let health_checker = Arc::new(HealthChecker::new(config.origins.clone()));
    let coalescer = Arc::new(RequestCoalescer::new(config.coalesce.max_waiters));

    if config.coalesce.enabled {
        info!(
            "Request coalescing enabled (max {} waiters)",
            config.coalesce.max_waiters
        );
    }

    let state = Arc::new(AppState {
        cache: cache.clone(),
        origin,
        config: Arc::new(config.clone()),
        metrics,
        rate_limiter: rate_limiter.clone(),
        circuit_breaker: circuit_breaker.clone(),
        health_checker: health_checker.clone(),
        coalescer,
        coalesce_enabled: config.coalesce.enabled,
    });

    // Start background cache cleanup task
    let cache_clone = cache.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            cache_clone.cleanup_expired();
        }
    });

    // Start background rate limiter cleanup task
    let rate_limiter_clone = rate_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            interval.tick().await;
            rate_limiter_clone.cleanup(Duration::from_secs(600));
        }
    });

    // Start origin health check tasks
    let (health_shutdown_tx, health_shutdown_rx) = tokio::sync::watch::channel(false);
    spawn_health_checks(health_checker.clone(), health_shutdown_rx);

    // Initialize admin authentication
    let admin_auth = Arc::new(AdminAuth::new(config.admin.clone()));
    if config.admin.auth_enabled {
        info!("Admin API authentication enabled");
    }

    // Initialize security
    let security = Arc::new(Security::new(config.security.clone()));
    if security.headers_enabled() {
        info!("Security headers enabled");
    }
    if security.signing_enabled() {
        info!("Request signing validation enabled");
    }
    if security.ip_control_enabled() {
        info!("IP-based access control enabled");
    }

    // Initialize edge processor
    let edge_processor = Arc::new(EdgeProcessor::from_config(&config.edge));
    if config.edge.enabled {
        info!(
            "Edge processing enabled ({} rewrite rules, {} routing rules)",
            config.edge.rewrite_rules.len(),
            config.edge.routing_rules.len()
        );
    }

    // Initialize content processor
    let content_processor = Arc::new(ContentProcessor::new(config.content.clone()));
    if config.content.enabled {
        info!(
            "Content processing enabled (compression={}, minify={}, image={})",
            config.content.compression.enabled,
            config.content.minification.enabled,
            config.content.image.enabled
        );
    }

    // Protocol configuration Arcs
    let http3_cfg = Arc::new(config.protocol.http3.clone());
    let sse_cfg = Arc::new(config.protocol.sse.clone());
    let ws_cfg = Arc::new(config.protocol.websocket.clone());

    if config.protocol.websocket.enabled {
        info!("WebSocket proxy enabled");
    }
    if config.protocol.http3.enabled {
        info!(
            "HTTP/3 Alt-Svc advertisement enabled on port {}",
            config.protocol.http3.port
        );
    }

    // Build router
    let app = build_router(
        state,
        admin_auth,
        security,
        edge_processor,
        config.edge.enabled,
        content_processor,
        config.content.enabled,
        ws_cfg,
        sse_cfg,
        http3_cfg,
    );

    // Resolve bind address according to network policy.
    let bind_mode = config.network.bind_mode;
    match bind_mode {
        BindMode::V4Only => info!("Network bind mode: IPv4 only"),
        BindMode::V6Only => info!("Network bind mode: IPv6 only"),
        BindMode::Dual => info!("Network bind mode: dual-stack"),
    }

    // Check for TLS configuration
    if let Some(ref tls_config) = config.tls {
        info!("TLS enabled, loading certificates");
        let addr: SocketAddr = config.server_addr().parse()?;

        // Build rustls config - use SNI resolver if multi-cert is configured
        let rustls_cfg = if config.network.sni.enabled && !config.network.sni.certificates.is_empty()
        {
            info!(
                "Multi-cert SNI enabled with {} certificates",
                config.network.sni.certificates.len()
            );
            build_rustls_config(&config.network.sni.certificates)?
        } else {
            build_rustls_config_single(&tls_config.cert_path, &tls_config.key_path)?
        };

        // Clone rustls config for HTTP/3 if enabled
        let http3_rustls_cfg = if config.protocol.http3.enabled {
            Some(rustls_cfg.clone())
        } else {
            None
        };

        // Start HTTP/3 server if enabled
        let http3_shutdown_rx = health_shutdown_tx.subscribe();
        if let Some(h3_cfg) = http3_rustls_cfg {
            let h3_port = config.protocol.http3.port;
            let h3_addr = SocketAddr::new(addr.ip(), h3_port);
            let h3_config = config.protocol.http3.clone();

            info!("Starting HTTP/3 server on {}", h3_addr);
            tokio::spawn(async move {
                // Simple handler that returns 200 OK for now
                // In a full implementation, this would route to the same handlers
                let handler = |_req: Http3Request| async move {
                    Http3Response::default()
                };

                if let Err(e) = run_http3_server(h3_addr, h3_cfg, h3_config, http3_shutdown_rx, handler).await {
                    tracing::error!(error = %e, "HTTP/3 server error");
                }
            });
        }

        start_tls_server(addr, app, rustls_cfg, health_shutdown_tx).await?;
    } else {
        let listener = build_listener(&config.server.host, config.server.port, bind_mode).await?;
        let local = listener.local_addr()?;
        info!("Listening on http://{}", local);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal_with_health(health_shutdown_tx))
        .await?;
    }

    info!("Server shutdown complete");
    Ok(())
}

async fn start_tls_server(
    addr: SocketAddr,
    app: Router,
    rustls_cfg: rustls::ServerConfig,
    health_shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    use axum_server::tls_rustls::RustlsConfig;

    let rustls_config = RustlsConfig::from_config(Arc::new(rustls_cfg));

    info!("Listening on https://{}", addr);

    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    // Spawn shutdown handler
    tokio::spawn(async move {
        shutdown_signal().await;
        // Signal health checks to stop
        let _ = health_shutdown_tx.send(true);
        handle_clone.graceful_shutdown(Some(Duration::from_secs(30)));
    });

    axum_server::bind_rustls(addr, rustls_config)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;

    Ok(())
}

fn load_config() -> anyhow::Result<Config> {
    // Try loading from config file first
    let config_path = std::env::var("CDN_CONFIG").unwrap_or_else(|_| "config/cdn.toml".to_string());

    if std::path::Path::new(&config_path).exists() {
        info!("Loading configuration from {}", config_path);
        Config::load(&config_path).map_err(|e| anyhow::anyhow!("{}", e))
    } else {
        info!("No config file found, using default configuration");
        Ok(Config::default())
    }
}

fn init_logging(config: &config::LoggingConfig) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));

    if config.json_format {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    }
}

fn build_router(
    state: Arc<AppState>,
    admin_auth: Arc<AdminAuth>,
    security: Arc<Security>,
    edge_processor: Arc<EdgeProcessor>,
    edge_enabled: bool,
    content_processor: Arc<ContentProcessor>,
    content_enabled: bool,
    ws_cfg: Arc<WebSocketConfig>,
    sse_cfg: Arc<SseConfig>,
    http3_cfg: Arc<Http3Config>,
) -> Router {
    // Public API routes (no auth required)
    let public_api_routes = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler));

    // Protected admin routes (auth required when enabled)
    let protected_api_routes = Router::new()
        .route("/stats", get(cache_stats))
        .route("/purge", post(purge_cache))
        .route("/warm", post(warm_cache))
        .route("/circuit-breakers", get(circuit_breaker_status))
        .route("/origins/health", get(origin_health_status))
        .route("/coalesce", get(coalesce_stats))
        .route_layer(middleware::from_fn_with_state(
            admin_auth.clone(),
            admin_auth_middleware,
        ));

    // Combined API routes
    let api_routes = Router::new()
        .merge(public_api_routes)
        .merge(protected_api_routes);

    // CDN routes - support both GET and HEAD methods (RFC 9110)
    let cdn_routes = Router::new()
        .route("/{origin}/{*path}", get(cdn_handler).head(cdn_handler))
        .route(
            "/{*path}",
            get(handlers::root_cdn_handler).head(handlers::root_cdn_handler),
        );

    // WebSocket proxy route (separate, does not go through HTTP middlewares
    // that would buffer or rewrite the upgrade).
    let ws_routes = if ws_cfg.enabled {
        Router::new()
            .route(
                "/_ws/{origin}/{*path}",
                get(websocket_proxy_handler).with_state(ws_cfg.clone()),
            )
    } else {
        Router::new()
    };

    // Build router with middleware layers
    let mut router = Router::new()
        .nest("/_cdn", api_routes)
        .merge(cdn_routes)
        .merge(ws_routes)
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CompressionLayer::new())
                .layer(
                    CorsLayer::new()
                        .allow_origin(Any)
                        .allow_methods(Any)
                        .allow_headers(Any),
                ),
        )
        // Security middleware layers (applied to all routes)
        .layer(middleware::from_fn_with_state(
            security.clone(),
            security_headers_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            security.clone(),
            request_signing_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            security.clone(),
            ip_access_control_middleware,
        ));

    // Add edge processing middleware if enabled
    if edge_enabled {
        router = router.layer(middleware::from_fn_with_state(
            edge_processor,
            edge_processing_middleware,
        ));
    }

    // Add content processing middleware if enabled
    if content_enabled {
        router = router.layer(middleware::from_fn_with_state(
            content_processor,
            content_processing_middleware,
        ));
    }

    // SSE headers fix-up runs after content processing so it can strip
    // compression hints that would break streaming.
    if sse_cfg.enabled {
        router = router.layer(middleware::from_fn_with_state(sse_cfg, sse_middleware));
    }

    // Alt-Svc advertising layer (HTTP/3 hint)
    if http3_cfg.enabled && http3_cfg.advertise_alt_svc {
        router = router.layer(middleware::from_fn_with_state(http3_cfg, alt_svc_middleware));
    }

    router.with_state(state)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received, starting graceful shutdown");
}

async fn shutdown_signal_with_health(health_shutdown_tx: tokio::sync::watch::Sender<bool>) {
    shutdown_signal().await;
    // Signal health checks to stop
    let _ = health_shutdown_tx.send(true);
}
