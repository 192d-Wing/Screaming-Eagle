//! Configuration hot-reload with file watching.
//!
//! Watches the configuration file for changes and applies updates to
//! reloadable components without restarting the server.

use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch, RwLock};
use tracing::{debug, error, info, warn};

use crate::config::Config;

/// Components that can be hot-reloaded.
#[derive(Debug, Clone)]
pub struct ReloadableConfig {
    /// Rate limit settings
    pub rate_limit: crate::config::RateLimitConfig,
    /// Circuit breaker settings
    pub circuit_breaker: crate::config::CircuitBreakerConfig,
    /// Cache settings (TTLs, not size - size requires restart)
    pub cache_ttl: CacheTtlConfig,
    /// Edge processing rules
    pub edge: crate::config::EdgeConfig,
    /// Security settings
    pub security: crate::config::SecurityConfig,
    /// Content processing settings
    pub content: crate::content::ContentConfig,
    /// Origins (can add/remove/modify)
    pub origins: std::collections::HashMap<String, crate::config::OriginConfig>,
}

/// Cache TTL settings that can be reloaded.
#[derive(Debug, Clone)]
pub struct CacheTtlConfig {
    pub default_ttl_secs: u64,
    pub max_ttl_secs: u64,
    pub stale_while_revalidate_secs: u64,
}

impl From<&Config> for ReloadableConfig {
    fn from(config: &Config) -> Self {
        Self {
            rate_limit: config.rate_limit.clone(),
            circuit_breaker: config.circuit_breaker.clone(),
            cache_ttl: CacheTtlConfig {
                default_ttl_secs: config.cache.default_ttl_secs,
                max_ttl_secs: config.cache.max_ttl_secs,
                stale_while_revalidate_secs: config.cache.stale_while_revalidate_secs,
            },
            edge: config.edge.clone(),
            security: config.security.clone(),
            content: config.content.clone(),
            origins: config.origins.clone(),
        }
    }
}

/// Configuration reload event.
#[derive(Debug, Clone)]
pub enum ReloadEvent {
    /// Full configuration reload
    Full(Arc<ReloadableConfig>),
    /// Configuration reload failed
    Failed(String),
}

/// Hot-reload manager that watches config files and broadcasts changes.
pub struct HotReloader {
    config_path: PathBuf,
    current: Arc<RwLock<ReloadableConfig>>,
    event_tx: broadcast::Sender<ReloadEvent>,
    shutdown: watch::Receiver<bool>,
}

impl HotReloader {
    /// Create a new hot-reloader for the given config path.
    pub fn new(
        config_path: impl AsRef<Path>,
        initial_config: &Config,
        shutdown: watch::Receiver<bool>,
    ) -> (Self, broadcast::Receiver<ReloadEvent>) {
        let (event_tx, event_rx) = broadcast::channel(16);
        let reloadable = ReloadableConfig::from(initial_config);

        (
            Self {
                config_path: config_path.as_ref().to_path_buf(),
                current: Arc::new(RwLock::new(reloadable)),
                event_tx,
                shutdown,
            },
            event_rx,
        )
    }

    /// Get the current reloadable configuration.
    pub async fn current(&self) -> ReloadableConfig {
        self.current.read().await.clone()
    }

    /// Subscribe to reload events.
    pub fn subscribe(&self) -> broadcast::Receiver<ReloadEvent> {
        self.event_tx.subscribe()
    }

    /// Start watching the configuration file.
    pub async fn watch(self) -> anyhow::Result<()> {
        let config_path = self.config_path.clone();
        let current = self.current.clone();
        let event_tx = self.event_tx.clone();
        let mut shutdown = self.shutdown.clone();

        // Create a channel for file events
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);

        // Spawn blocking watcher
        let watcher_path = config_path.clone();
        std::thread::spawn(move || {
            let rt_tx = tx;
            let mut watcher = match RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    if let Ok(event) = res {
                        if event.kind.is_modify() || event.kind.is_create() {
                            let _ = rt_tx.blocking_send(());
                        }
                    }
                },
                NotifyConfig::default().with_poll_interval(Duration::from_secs(2)),
            ) {
                Ok(w) => w,
                Err(e) => {
                    error!(error = %e, "Failed to create file watcher");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&watcher_path, RecursiveMode::NonRecursive) {
                error!(error = %e, path = %watcher_path.display(), "Failed to watch config file");
                return;
            }

            info!(path = %watcher_path.display(), "Watching config file for changes");

            // Keep watcher alive
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        });

        // Debounce and reload
        let mut last_reload = std::time::Instant::now();
        let debounce = Duration::from_millis(500);

        loop {
            tokio::select! {
                Some(()) = rx.recv() => {
                    // Debounce rapid changes
                    if last_reload.elapsed() < debounce {
                        debug!("Debouncing config reload");
                        continue;
                    }
                    last_reload = std::time::Instant::now();

                    // Reload configuration
                    match reload_config(&config_path).await {
                        Ok(new_config) => {
                            let reloadable = ReloadableConfig::from(&new_config);
                            *current.write().await = reloadable.clone();
                            info!("Configuration reloaded successfully");
                            let _ = event_tx.send(ReloadEvent::Full(Arc::new(reloadable)));
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to reload configuration");
                            let _ = event_tx.send(ReloadEvent::Failed(e.to_string()));
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("Hot-reloader shutting down");
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Manually trigger a reload.
    pub async fn reload(&self) -> Result<(), String> {
        match reload_config(&self.config_path).await {
            Ok(new_config) => {
                let reloadable = ReloadableConfig::from(&new_config);
                *self.current.write().await = reloadable.clone();
                info!("Configuration reloaded manually");
                let _ = self
                    .event_tx
                    .send(ReloadEvent::Full(Arc::new(reloadable)));
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                let _ = self.event_tx.send(ReloadEvent::Failed(msg.clone()));
                Err(msg)
            }
        }
    }
}

async fn reload_config(path: &Path) -> anyhow::Result<Config> {
    let content = tokio::fs::read_to_string(path).await?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}

/// Handle for components to receive reload notifications.
#[derive(Clone)]
pub struct ReloadHandle {
    current: Arc<RwLock<ReloadableConfig>>,
    events: broadcast::Sender<ReloadEvent>,
}

impl ReloadHandle {
    pub fn new(
        current: Arc<RwLock<ReloadableConfig>>,
        events: broadcast::Sender<ReloadEvent>,
    ) -> Self {
        Self { current, events }
    }

    pub async fn current(&self) -> ReloadableConfig {
        self.current.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ReloadEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloadable_config_from_config() {
        let config = Config::default();
        let reloadable = ReloadableConfig::from(&config);
        assert_eq!(
            reloadable.rate_limit.requests_per_window,
            config.rate_limit.requests_per_window
        );
    }

    #[test]
    fn cache_ttl_config_extracts_ttls() {
        let config = Config::default();
        let reloadable = ReloadableConfig::from(&config);
        assert_eq!(
            reloadable.cache_ttl.default_ttl_secs,
            config.cache.default_ttl_secs
        );
    }
}
