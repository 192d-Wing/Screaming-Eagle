mod cache;
mod config;
mod error;
mod handlers;
mod metrics;
mod origin;

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

use cache::Cache;
use config::Config;
use handlers::{cache_stats, cdn_handler, health, metrics as metrics_handler, purge_cache, AppState};
use metrics::Metrics;
use origin::OriginFetcher;

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

    // Initialize components
    let cache = Arc::new(Cache::new(config.cache.clone()));
    let origin = Arc::new(OriginFetcher::new(config.origins.clone())?);
    let metrics = Arc::new(Metrics::new());

    let state = Arc::new(AppState {
        cache: cache.clone(),
        origin,
        config: Arc::new(config.clone()),
        metrics,
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

    // Build router
    let app = build_router(state);

    // Start server
    let addr: SocketAddr = config.server_addr().parse()?;
    info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Server shutdown complete");
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
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.level));

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
    // API routes
    let api_routes = Router::new()
        .route("/health", get(health))
        .route("/stats", get(cache_stats))
        .route("/metrics", get(metrics_handler))
        .route("/purge", post(purge_cache));

    // CDN routes
    let cdn_routes = Router::new()
        .route("/{origin}/{*path}", get(cdn_handler))
        .route("/{*path}", get(handlers::root_cdn_handler));

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
