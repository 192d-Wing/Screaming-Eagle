use axum::{
    routing::{get, post},
    Router,
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
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use screaming_eagle::cache::Cache;
use screaming_eagle::circuit_breaker::{self, CircuitBreakerManager};
use screaming_eagle::config::{self, Config};
use screaming_eagle::handlers::{
    self, cache_stats, cdn_handler, circuit_breaker_status, health, metrics as metrics_handler,
    purge_cache, AppState,
};
use screaming_eagle::metrics::Metrics;
use screaming_eagle::origin::OriginFetcher;
use screaming_eagle::rate_limit::{RateLimitConfig, RateLimiter};

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
    let origin = Arc::new(OriginFetcher::new(config.origins.clone())?);
    let metrics = Arc::new(Metrics::new());

    let state = Arc::new(AppState {
        cache: cache.clone(),
        origin,
        config: Arc::new(config.clone()),
        metrics,
        rate_limiter: rate_limiter.clone(),
        circuit_breaker: circuit_breaker.clone(),
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

    // Build router
    let app = build_router(state);

    // Start server
    let addr: SocketAddr = config.server_addr().parse()?;

    // Check for TLS configuration
    if let Some(ref tls_config) = config.tls {
        info!("TLS enabled, loading certificates");
        start_tls_server(addr, app, tls_config).await?;
    } else {
        info!("Listening on http://{}", addr);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    }

    info!("Server shutdown complete");
    Ok(())
}

async fn start_tls_server(
    addr: SocketAddr,
    app: Router,
    tls_config: &config::TlsConfig,
) -> anyhow::Result<()> {
    use axum_server::tls_rustls::RustlsConfig;

    let rustls_config =
        RustlsConfig::from_pem_file(&tls_config.cert_path, &tls_config.key_path).await?;

    info!("Listening on https://{}", addr);

    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    // Spawn shutdown handler
    tokio::spawn(async move {
        shutdown_signal().await;
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

fn build_router(state: Arc<AppState>) -> Router {
    // API routes (not rate limited)
    let api_routes = Router::new()
        .route("/health", get(health))
        .route("/stats", get(cache_stats))
        .route("/metrics", get(metrics_handler))
        .route("/purge", post(purge_cache))
        .route("/circuit-breakers", get(circuit_breaker_status));

    // CDN routes - support both GET and HEAD methods (RFC 9110)
    let cdn_routes = Router::new()
        .route("/{origin}/{*path}", get(cdn_handler).head(cdn_handler))
        .route("/{*path}", get(handlers::root_cdn_handler).head(handlers::root_cdn_handler));

    // Combine routes
    Router::new()
        .nest("/_cdn", api_routes)
        .merge(cdn_routes)
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
        .with_state(state)
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
