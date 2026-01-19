use bytes::Bytes;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// stale-if-error window in seconds (RFC 5861)
    pub stale_if_error_secs: Option<u64>,
    /// Access count for LRU-K tracking
    pub access_count: u32,
    /// Last access time for LRU eviction
    pub last_accessed: Instant,
}

impl CacheEntry {
    /// Record an access to this entry (for LRU-K tracking)
    pub fn record_access(&mut self) {
        self.access_count = self.access_count.saturating_add(1);
        self.last_accessed = Instant::now();
    }

    /// Get the access count
    pub fn access_count(&self) -> u32 {
        self.access_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub total_entries: usize,
    pub total_size_bytes: usize,
    pub max_size_bytes: usize,
    pub hit_ratio: f64,
    pub evictions: u64,
    pub stale_hits: u64,
    pub avg_entry_size_bytes: usize,
    pub hot_entries: usize, // Entries with access_count > threshold
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Stale,
    StaleIfError,
    Bypass,
}

impl CacheStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Stale => "STALE",
            CacheStatus::StaleIfError => "STALE-IF-ERROR",
            CacheStatus::Bypass => "BYPASS",
        }
    }
}

/// Threshold for considering an entry "hot" (frequently accessed)
const HOT_ENTRY_THRESHOLD: u32 = 3;

pub struct Cache {
    entries: DashMap<String, CacheEntry>,
    config: CacheConfig,
    current_size: AtomicUsize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    stale_hits: AtomicU64,
}

impl Cache {
    pub fn new(config: CacheConfig) -> Self {
        // Configure DashMap with optimal shard count based on CPU cores
        let shard_count = (num_cpus::get() * 4).next_power_of_two();
        let entries = DashMap::with_capacity_and_shard_amount(10000, shard_count);

        info!(
            shards = shard_count,
            "Initialized cache with {} shards", shard_count
        );

        Self {
            entries,
            config,
            current_size: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            stale_hits: AtomicU64::new(0),
        }
    }

    pub fn get(&self, key: &str) -> Option<(CacheEntry, CacheStatus)> {
        if let Some(mut entry) = self.entries.get_mut(key) {
            let now = Instant::now();

            if now < entry.expires_at {
                // Record access for LRU-K tracking
                entry.record_access();
                self.hits.fetch_add(1, Ordering::Relaxed);
                debug!(key = %key, access_count = entry.access_count(), "Cache HIT");
                return Some((entry.clone(), CacheStatus::Hit));
            }

            // Check stale-while-revalidate window
            let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);
            if now < entry.expires_at + stale_window {
                entry.record_access();
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.stale_hits.fetch_add(1, Ordering::Relaxed);
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

    /// Get a stale entry for stale-if-error handling (RFC 5861)
    /// Returns the entry if it's within the stale-if-error window
    pub fn get_stale_for_error(&self, key: &str) -> Option<CacheEntry> {
        if let Some(entry) = self.entries.get(key) {
            let now = Instant::now();

            // Check if within stale-if-error window
            if let Some(stale_if_error_secs) = entry.stale_if_error_secs {
                let stale_if_error_window = Duration::from_secs(stale_if_error_secs);
                if now < entry.expires_at + stale_if_error_window {
                    debug!(key = %key, "Cache STALE-IF-ERROR (within error window)");
                    return Some(entry.clone());
                }
            }

            // Also check the configured stale_while_revalidate as fallback for errors
            let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);
            if now < entry.expires_at + stale_window {
                debug!(key = %key, "Cache STALE-IF-ERROR (within revalidate window)");
                return Some(entry.clone());
            }
        }

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
            self.current_size
                .fetch_sub(old_entry.size, Ordering::Relaxed);
        }

        self.current_size.fetch_add(entry_size, Ordering::Relaxed);
        self.entries.insert(key.clone(), entry);

        debug!(key = %key, size = entry_size, "Cached entry");
    }

    pub fn invalidate(&self, key: &str) -> bool {
        let removed = self.invalidate_internal(key, false);
        if removed {
            info!(key = %key, "Invalidated cache entry");
        }
        removed
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
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_ratio = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        let total_entries = self.entries.len();
        let total_size_bytes = self.current_size.load(Ordering::Relaxed);
        let avg_entry_size_bytes = if total_entries > 0 {
            total_size_bytes / total_entries
        } else {
            0
        };

        // Count hot entries (frequently accessed)
        let hot_entries = self
            .entries
            .iter()
            .filter(|e| e.access_count() >= HOT_ENTRY_THRESHOLD)
            .count();

        CacheStats {
            hits,
            misses,
            total_entries,
            total_size_bytes,
            max_size_bytes: self.config.max_size_bytes(),
            hit_ratio,
            evictions: self.evictions.load(Ordering::Relaxed),
            stale_hits: self.stale_hits.load(Ordering::Relaxed),
            avg_entry_size_bytes,
            hot_entries,
        }
    }

    fn evict_if_needed(&self, needed_space: usize) {
        let max_size = self.config.max_size_bytes();
        let current = self.current_size.load(Ordering::Relaxed);

        if current + needed_space <= max_size {
            return;
        }

        // LRU-K eviction strategy:
        // 1. Remove expired entries first
        // 2. Remove cold entries (low access count) before hot entries
        // 3. Within same access count, remove oldest entries

        let now = Instant::now();

        // First pass: remove expired entries
        let expired_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|e| now >= e.expires_at)
            .map(|e| e.key().clone())
            .collect();

        let expired_count = expired_keys.len();
        for key in expired_keys {
            self.invalidate_internal(&key, true);
        }

        // Check if we have enough space now
        if self.current_size.load(Ordering::Relaxed) + needed_space <= max_size {
            return;
        }

        // Second pass: LRU-K eviction - prioritize cold entries
        // Score = access_count * 1000 + recency_score
        // Lower score = more likely to evict
        let mut entries_by_score: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|e| {
                let recency = e.last_accessed.elapsed().as_secs().min(1000);
                // Lower access count and older access = lower score = evict first
                let score = (e.access_count() as u64 * 1000).saturating_sub(recency);
                (e.key().clone(), score)
            })
            .collect();

        // Sort by score ascending (lowest score = evict first)
        entries_by_score.sort_by_key(|(_, score)| *score);

        let mut evict_count = 0;
        for (key, _) in entries_by_score {
            if self.current_size.load(Ordering::Relaxed) + needed_space <= max_size {
                break;
            }
            self.invalidate_internal(&key, true);
            evict_count += 1;
        }

        if expired_count > 0 || evict_count > 0 {
            debug!(
                expired = expired_count,
                evicted = evict_count,
                "Cache eviction completed"
            );
        }
    }

    /// Internal invalidation that optionally tracks evictions
    fn invalidate_internal(&self, key: &str, is_eviction: bool) -> bool {
        if let Some((_, entry)) = self.entries.remove(key) {
            self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
            if is_eviction {
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            true
        } else {
            false
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
            self.invalidate_internal(&key, true);
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

/// Generate a cache key that includes Vary header values (RFC 9111)
/// This ensures different content variants are cached separately
pub fn generate_cache_key_with_vary(
    host: &str,
    path: &str,
    query: Option<&str>,
    vary_header: Option<&str>,
    request_headers: &std::collections::HashMap<String, String>,
) -> String {
    let base_key = generate_cache_key(host, path, query);

    // If no Vary header, use base key
    let vary = match vary_header {
        Some(v) => v,
        None => return base_key,
    };

    // Handle Vary: * (never cache)
    if vary.trim() == "*" {
        return format!("{}|vary=*|{}", base_key, uuid_simple());
    }

    // Extract relevant request header values based on Vary header
    let mut vary_values: Vec<String> = Vec::new();

    for header_name in vary.split(',') {
        let header_name = header_name.trim().to_lowercase();
        // Skip Vary: * in a list
        if header_name == "*" {
            continue;
        }

        let value = request_headers
            .get(&header_name)
            .or_else(|| request_headers.get(&header_name.to_uppercase()))
            .map(|s| s.as_str())
            .unwrap_or("");

        vary_values.push(format!("{}={}", header_name, value));
    }

    if vary_values.is_empty() {
        base_key
    } else {
        format!("{}|vary:{}", base_key, vary_values.join("|"))
    }
}

/// Simple UUID-like generator for unique keys (for Vary: *)
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", nanos)
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
        } else if let Some(value) = part.strip_prefix("stale-if-error=") {
            if let Ok(secs) = value.parse() {
                directives.stale_if_error = Some(secs);
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
    pub stale_if_error: Option<u64>,
}

impl CacheControlDirectives {
    pub fn is_cacheable(&self) -> bool {
        !self.no_store && !self.private
    }

    pub fn ttl(&self, default_ttl: Duration, max_ttl: Duration) -> Duration {
        // s-maxage takes precedence for shared caches (CDN)
        let ttl_secs = self
            .s_maxage
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

    #[test]
    fn test_generate_cache_key_with_vary() {
        let mut headers = HashMap::new();
        headers.insert("accept-encoding".to_string(), "gzip, br".to_string());
        headers.insert("accept-language".to_string(), "en-US".to_string());

        // No Vary header - should return base key
        assert_eq!(
            generate_cache_key_with_vary("example.com", "/path", None, None, &headers),
            "example.com/path"
        );

        // Single Vary header
        let key = generate_cache_key_with_vary(
            "example.com",
            "/path",
            None,
            Some("accept-encoding"),
            &headers,
        );
        assert!(key.contains("example.com/path"));
        assert!(key.contains("accept-encoding=gzip, br"));

        // Multiple Vary headers
        let key = generate_cache_key_with_vary(
            "example.com",
            "/path",
            None,
            Some("accept-encoding, accept-language"),
            &headers,
        );
        assert!(key.contains("accept-encoding=gzip, br"));
        assert!(key.contains("accept-language=en-US"));

        // Vary header for non-existent request header
        let key = generate_cache_key_with_vary(
            "example.com",
            "/path",
            None,
            Some("x-custom-header"),
            &headers,
        );
        assert!(key.contains("x-custom-header="));

        // Vary: * should generate unique key
        let key1 = generate_cache_key_with_vary("example.com", "/path", None, Some("*"), &headers);
        let key2 = generate_cache_key_with_vary("example.com", "/path", None, Some("*"), &headers);
        assert!(key1.contains("vary=*"));
        assert_ne!(key1, key2); // Each should be unique
    }
}
