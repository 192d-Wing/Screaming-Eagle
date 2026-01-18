use bytes::Bytes;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::config::CacheConfig;

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub body: Bytes,
    pub headers: HashMap<String, String>,
    pub status_code: u16,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub total_entries: usize,
    pub total_size_bytes: usize,
    pub max_size_bytes: usize,
    pub hit_ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Stale,
    Bypass,
}

impl CacheStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Stale => "STALE",
            CacheStatus::Bypass => "BYPASS",
        }
    }
}

pub struct Cache {
    entries: DashMap<String, CacheEntry>,
    config: CacheConfig,
    current_size: AtomicUsize,
    hits: AtomicUsize,
    misses: AtomicUsize,
}

impl Cache {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            entries: DashMap::new(),
            config,
            current_size: AtomicUsize::new(0),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    pub fn get(&self, key: &str) -> Option<(CacheEntry, CacheStatus)> {
        if let Some(entry) = self.entries.get(key) {
            let now = Instant::now();

            if now < entry.expires_at {
                self.hits.fetch_add(1, Ordering::Relaxed);
                debug!(key = %key, "Cache HIT");
                return Some((entry.clone(), CacheStatus::Hit));
            }

            // Check stale-while-revalidate window
            let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);
            if now < entry.expires_at + stale_window {
                self.hits.fetch_add(1, Ordering::Relaxed);
                debug!(key = %key, "Cache STALE (within revalidation window)");
                return Some((entry.clone(), CacheStatus::Stale));
            }

            // Entry is expired beyond stale window
            debug!(key = %key, "Cache entry expired");
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        debug!(key = %key, "Cache MISS");
        None
    }

    pub fn set(&self, key: String, entry: CacheEntry) {
        let entry_size = entry.size;

        // Check if entry is too large
        if entry_size > self.config.max_entry_size_bytes() {
            warn!(
                key = %key,
                size = entry_size,
                max = self.config.max_entry_size_bytes(),
                "Entry too large to cache"
            );
            return;
        }

        // Evict entries if necessary
        self.evict_if_needed(entry_size);

        // Update size tracking
        if let Some(old_entry) = self.entries.get(&key) {
            self.current_size.fetch_sub(old_entry.size, Ordering::Relaxed);
        }

        self.current_size.fetch_add(entry_size, Ordering::Relaxed);
        self.entries.insert(key.clone(), entry);

        debug!(key = %key, size = entry_size, "Cached entry");
    }

    pub fn invalidate(&self, key: &str) -> bool {
        if let Some((_, entry)) = self.entries.remove(key) {
            self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
            info!(key = %key, "Invalidated cache entry");
            true
        } else {
            false
        }
    }

    pub fn invalidate_prefix(&self, prefix: &str) -> usize {
        let keys_to_remove: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.key().starts_with(prefix))
            .map(|e| e.key().clone())
            .collect();

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            self.invalidate(&key);
        }

        info!(prefix = %prefix, count = count, "Invalidated cache entries by prefix");
        count
    }

    pub fn purge_all(&self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.current_size.store(0, Ordering::Relaxed);
        info!(count = count, "Purged all cache entries");
        count
    }

    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(Ordering::Relaxed) as u64;
        let misses = self.misses.load(Ordering::Relaxed) as u64;
        let total = hits + misses;
        let hit_ratio = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        CacheStats {
            hits,
            misses,
            total_entries: self.entries.len(),
            total_size_bytes: self.current_size.load(Ordering::Relaxed),
            max_size_bytes: self.config.max_size_bytes(),
            hit_ratio,
        }
    }

    fn evict_if_needed(&self, needed_space: usize) {
        let max_size = self.config.max_size_bytes();
        let current = self.current_size.load(Ordering::Relaxed);

        if current + needed_space <= max_size {
            return;
        }

        // Simple LRU-like eviction: remove expired entries first, then oldest
        let now = Instant::now();

        // First pass: remove expired entries
        let expired_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|e| now >= e.expires_at)
            .map(|e| e.key().clone())
            .collect();

        for key in expired_keys {
            self.invalidate(&key);
        }

        // Check if we have enough space now
        if self.current_size.load(Ordering::Relaxed) + needed_space <= max_size {
            return;
        }

        // Second pass: remove oldest entries until we have space
        let mut entries_by_age: Vec<(String, Instant)> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), e.created_at))
            .collect();

        entries_by_age.sort_by_key(|(_, created)| *created);

        for (key, _) in entries_by_age {
            if self.current_size.load(Ordering::Relaxed) + needed_space <= max_size {
                break;
            }
            self.invalidate(&key);
        }
    }

    pub fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);

        let expired_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|e| now >= e.expires_at + stale_window)
            .map(|e| e.key().clone())
            .collect();

        let count = expired_keys.len();
        for key in expired_keys {
            self.invalidate(&key);
        }

        if count > 0 {
            debug!(count = count, "Cleaned up expired cache entries");
        }
        count
    }
}

pub fn generate_cache_key(host: &str, path: &str, query: Option<&str>) -> String {
    match query {
        Some(q) if !q.is_empty() => format!("{}{}?{}", host, path, q),
        _ => format!("{}{}", host, path),
    }
}

pub fn parse_cache_control(header: &str) -> CacheControlDirectives {
    let mut directives = CacheControlDirectives::default();

    for part in header.split(',') {
        let part = part.trim().to_lowercase();

        if part == "no-cache" {
            directives.no_cache = true;
        } else if part == "no-store" {
            directives.no_store = true;
        } else if part == "private" {
            directives.private = true;
        } else if part == "public" {
            directives.public = true;
        } else if part == "must-revalidate" {
            directives.must_revalidate = true;
        } else if let Some(value) = part.strip_prefix("max-age=") {
            if let Ok(secs) = value.parse() {
                directives.max_age = Some(secs);
            }
        } else if let Some(value) = part.strip_prefix("s-maxage=") {
            if let Ok(secs) = value.parse() {
                directives.s_maxage = Some(secs);
            }
        } else if let Some(value) = part.strip_prefix("stale-while-revalidate=") {
            if let Ok(secs) = value.parse() {
                directives.stale_while_revalidate = Some(secs);
            }
        }
    }

    directives
}

#[derive(Debug, Default, Clone)]
pub struct CacheControlDirectives {
    pub no_cache: bool,
    pub no_store: bool,
    pub private: bool,
    pub public: bool,
    pub must_revalidate: bool,
    pub max_age: Option<u64>,
    pub s_maxage: Option<u64>,
    pub stale_while_revalidate: Option<u64>,
}

impl CacheControlDirectives {
    pub fn is_cacheable(&self) -> bool {
        !self.no_store && !self.private
    }

    pub fn ttl(&self, default_ttl: Duration, max_ttl: Duration) -> Duration {
        // s-maxage takes precedence for shared caches (CDN)
        let ttl_secs = self.s_maxage
            .or(self.max_age)
            .map(Duration::from_secs)
            .unwrap_or(default_ttl);

        std::cmp::min(ttl_secs, max_ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cache_control() {
        let directives = parse_cache_control("max-age=3600, public");
        assert_eq!(directives.max_age, Some(3600));
        assert!(directives.public);
        assert!(!directives.private);

        let directives = parse_cache_control("no-store, no-cache");
        assert!(directives.no_store);
        assert!(directives.no_cache);

        let directives = parse_cache_control("s-maxage=600, max-age=300");
        assert_eq!(directives.s_maxage, Some(600));
        assert_eq!(directives.max_age, Some(300));
    }

    #[test]
    fn test_generate_cache_key() {
        assert_eq!(
            generate_cache_key("example.com", "/path", Some("foo=bar")),
            "example.com/path?foo=bar"
        );
        assert_eq!(
            generate_cache_key("example.com", "/path", None),
            "example.com/path"
        );
    }
}
