use bytes::Bytes;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
    /// Cache tags for tag-based invalidation
    pub cache_tags: Vec<String>,
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
    pub total_tags: usize,
    pub tagged_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagStats {
    pub tag: String,
    pub entry_count: usize,
    pub total_size_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HierarchyStats {
    pub enabled: bool,
    pub l1_entries: usize,
    pub l2_entries: usize,
    pub l1_size_bytes: usize,
    pub l2_size_bytes: usize,
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub promotions: u64,
    pub demotions: u64,
    pub l1_hit_ratio: f64,
    pub l2_hit_ratio: f64,
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
    /// L1 cache (hot tier) - frequently accessed entries
    l1_cache: Arc<DashMap<String, CacheEntry>>,
    /// L2 cache (cold tier) - less frequently accessed entries
    l2_cache: Arc<DashMap<String, CacheEntry>>,
    /// Legacy entries field for backwards compatibility (now unused)
    entries: DashMap<String, CacheEntry>,
    config: CacheConfig,
    l1_current_size: AtomicUsize,
    l2_current_size: AtomicUsize,
    current_size: AtomicUsize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    stale_hits: AtomicU64,
    l1_hits: AtomicU64,
    l2_hits: AtomicU64,
    promotions: AtomicU64,
    demotions: AtomicU64,
    /// Tag to cache keys mapping for tag-based invalidation
    tag_to_keys: Arc<DashMap<String, HashSet<String>>>,
}

impl Cache {
    pub fn new(config: CacheConfig) -> Self {
        // Configure DashMap with optimal shard count based on CPU cores
        let shard_count = (num_cpus::get() * 4).next_power_of_two();

        // Calculate L1 and L2 capacities based on configuration
        let hierarchy_enabled = config.hierarchy.enabled;
        let l1_percent = config.hierarchy.l1_size_percent;
        let l2_percent = config.hierarchy.l2_size_percent;

        let total_capacity = 10000;
        let l1_capacity = if hierarchy_enabled {
            (total_capacity * l1_percent) / 100
        } else {
            0
        };
        let l2_capacity = if hierarchy_enabled {
            (total_capacity * l2_percent) / 100
        } else {
            total_capacity
        };

        let l1_cache = Arc::new(DashMap::with_capacity_and_shard_amount(
            l1_capacity.max(100),
            shard_count,
        ));
        let l2_cache = Arc::new(DashMap::with_capacity_and_shard_amount(
            l2_capacity.max(1000),
            shard_count,
        ));
        let entries = DashMap::with_capacity_and_shard_amount(10000, shard_count);
        let tag_to_keys = Arc::new(DashMap::with_capacity_and_shard_amount(1000, shard_count));

        if hierarchy_enabled {
            info!(
                shards = shard_count,
                l1_percent = l1_percent,
                l2_percent = l2_percent,
                "Initialized L1/L2 cache hierarchy with {} shards",
                shard_count
            );
        } else {
            info!(
                shards = shard_count,
                "Initialized cache with {} shards (hierarchy disabled)", shard_count
            );
        }

        Self {
            l1_cache,
            l2_cache,
            entries,
            config,
            l1_current_size: AtomicUsize::new(0),
            l2_current_size: AtomicUsize::new(0),
            current_size: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            stale_hits: AtomicU64::new(0),
            l1_hits: AtomicU64::new(0),
            l2_hits: AtomicU64::new(0),
            promotions: AtomicU64::new(0),
            demotions: AtomicU64::new(0),
            tag_to_keys,
        }
    }

    pub fn get(&self, key: &str) -> Option<(CacheEntry, CacheStatus)> {
        let now = Instant::now();

        // If hierarchy is enabled, check L1 then L2
        if self.config.hierarchy.enabled {
            // Check L1 cache first
            if let Some(mut entry) = self.l1_cache.get_mut(key) {
                if now < entry.expires_at {
                    entry.record_access();
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    self.l1_hits.fetch_add(1, Ordering::Relaxed);
                    debug!(key = %key, tier = "L1", access_count = entry.access_count(), "Cache HIT");
                    return Some((entry.clone(), CacheStatus::Hit));
                }

                // Check stale-while-revalidate window
                let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);
                if now < entry.expires_at + stale_window {
                    entry.record_access();
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    self.stale_hits.fetch_add(1, Ordering::Relaxed);
                    self.l1_hits.fetch_add(1, Ordering::Relaxed);
                    debug!(key = %key, tier = "L1", "Cache STALE (within revalidation window)");
                    return Some((entry.clone(), CacheStatus::Stale));
                }
            }

            // Check L2 cache
            if let Some(mut entry) = self.l2_cache.get_mut(key) {
                if now < entry.expires_at {
                    entry.record_access();
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    self.l2_hits.fetch_add(1, Ordering::Relaxed);

                    let should_promote =
                        entry.access_count() >= self.config.hierarchy.promotion_threshold;
                    let entry_clone = entry.clone();

                    // Drop the mutable reference before promotion
                    drop(entry);

                    if should_promote {
                        // Promote to L1
                        self.promote_to_l1(key, entry_clone.clone());
                    }

                    debug!(
                        key = %key,
                        tier = "L2",
                        access_count = entry_clone.access_count(),
                        promoted = should_promote,
                        "Cache HIT"
                    );
                    return Some((entry_clone, CacheStatus::Hit));
                }

                // Check stale-while-revalidate window
                let stale_window = Duration::from_secs(self.config.stale_while_revalidate_secs);
                if now < entry.expires_at + stale_window {
                    entry.record_access();
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    self.stale_hits.fetch_add(1, Ordering::Relaxed);
                    self.l2_hits.fetch_add(1, Ordering::Relaxed);
                    debug!(key = %key, tier = "L2", "Cache STALE (within revalidation window)");
                    return Some((entry.clone(), CacheStatus::Stale));
                }
            }
        } else {
            // Legacy single-tier cache lookup (hierarchy disabled)
            if let Some(mut entry) = self.entries.get_mut(key) {
                if now < entry.expires_at {
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
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        debug!(key = %key, "Cache MISS");
        None
    }

    /// Get a stale entry for stale-if-error handling (RFC 5861)
    /// Returns the entry if it's within the stale-if-error window
    pub fn get_stale_for_error(&self, key: &str) -> Option<CacheEntry> {
        let now = Instant::now();

        // Helper function to check stale windows
        let check_stale = |entry: &CacheEntry| -> Option<CacheEntry> {
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

            None
        };

        if self.config.hierarchy.enabled {
            // Check L1 first, then L2
            if let Some(entry) = self.l1_cache.get(key) {
                if let Some(result) = check_stale(&entry) {
                    return Some(result);
                }
            }

            if let Some(entry) = self.l2_cache.get(key) {
                return check_stale(&entry);
            }
        } else {
            // Legacy single-tier lookup
            if let Some(entry) = self.entries.get(key) {
                return check_stale(&entry);
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

        if self.config.hierarchy.enabled {
            // Determine which tier based on access count
            let is_hot = entry.access_count >= self.config.hierarchy.promotion_threshold;

            // Remove from old locations if exists
            self.remove_from_both_tiers(&key);

            if is_hot {
                // Store in L1 (hot tier)
                self.l1_current_size
                    .fetch_add(entry_size, Ordering::Relaxed);
                self.l1_cache.insert(key.clone(), entry);
                debug!(key = %key, size = entry_size, tier = "L1", "Cached entry");
            } else {
                // Store in L2 (cold tier)
                self.l2_current_size
                    .fetch_add(entry_size, Ordering::Relaxed);
                self.l2_cache.insert(key.clone(), entry);
                debug!(key = %key, size = entry_size, tier = "L2", "Cached entry");
            }

            self.current_size.fetch_add(entry_size, Ordering::Relaxed);
        } else {
            // Legacy single-tier storage (hierarchy disabled)
            if let Some(old_entry) = self.entries.get(&key) {
                self.current_size
                    .fetch_sub(old_entry.size, Ordering::Relaxed);
            }

            self.current_size.fetch_add(entry_size, Ordering::Relaxed);
            self.entries.insert(key.clone(), entry);
            debug!(key = %key, size = entry_size, "Cached entry");
        }
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
        let count = if self.config.hierarchy.enabled {
            let l1_count = self.l1_cache.len();
            let l2_count = self.l2_cache.len();
            self.l1_cache.clear();
            self.l2_cache.clear();
            self.l1_current_size.store(0, Ordering::Relaxed);
            self.l2_current_size.store(0, Ordering::Relaxed);
            l1_count + l2_count
        } else {
            let count = self.entries.len();
            self.entries.clear();
            count
        };

        self.current_size.store(0, Ordering::Relaxed);
        self.tag_to_keys.clear(); // Also clear tag index
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

        let (total_entries, hot_entries, tagged_entries) = if self.config.hierarchy.enabled {
            let l1_count = self.l1_cache.len();
            let l2_count = self.l2_cache.len();
            let total = l1_count + l2_count;

            // Count hot entries (in L1 or high access count in L2)
            let l1_hot = self.l1_cache.iter().count(); // All L1 entries are hot by definition
            let l2_hot = self
                .l2_cache
                .iter()
                .filter(|e| e.access_count() >= HOT_ENTRY_THRESHOLD)
                .count();

            // Count tagged entries
            let l1_tagged = self
                .l1_cache
                .iter()
                .filter(|e| !e.cache_tags.is_empty())
                .count();
            let l2_tagged = self
                .l2_cache
                .iter()
                .filter(|e| !e.cache_tags.is_empty())
                .count();

            (total, l1_hot + l2_hot, l1_tagged + l2_tagged)
        } else {
            let total = self.entries.len();
            let hot = self
                .entries
                .iter()
                .filter(|e| e.access_count() >= HOT_ENTRY_THRESHOLD)
                .count();
            let tagged = self
                .entries
                .iter()
                .filter(|e| !e.cache_tags.is_empty())
                .count();

            (total, hot, tagged)
        };

        let total_size_bytes = self.current_size.load(Ordering::Relaxed);
        let avg_entry_size_bytes = if total_entries > 0 {
            total_size_bytes / total_entries
        } else {
            0
        };

        let total_tags = self.tag_to_keys.len();

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
            total_tags,
            tagged_entries,
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
        // Helper to remove tags from index
        let remove_tags = |entry: &CacheEntry| {
            for tag in &entry.cache_tags {
                if let Some(mut keys_set) = self.tag_to_keys.get_mut(tag) {
                    keys_set.remove(key);
                    if keys_set.is_empty() {
                        drop(keys_set);
                        self.tag_to_keys.remove(tag);
                    }
                }
            }
        };

        let mut removed = false;

        if self.config.hierarchy.enabled {
            // Try removing from L1
            if let Some((_, entry)) = self.l1_cache.remove(key) {
                self.l1_current_size
                    .fetch_sub(entry.size, Ordering::Relaxed);
                self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
                remove_tags(&entry);
                removed = true;
            }

            // Try removing from L2
            if let Some((_, entry)) = self.l2_cache.remove(key) {
                self.l2_current_size
                    .fetch_sub(entry.size, Ordering::Relaxed);
                self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
                remove_tags(&entry);
                removed = true;
            }
        } else {
            // Legacy single-tier removal
            if let Some((_, entry)) = self.entries.remove(key) {
                self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
                remove_tags(&entry);
                removed = true;
            }
        }

        if removed && is_eviction {
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }

        removed
    }

    /// Remove an entry from both L1 and L2 tiers (used during tier transitions)
    fn remove_from_both_tiers(&self, key: &str) {
        if let Some((_, entry)) = self.l1_cache.remove(key) {
            self.l1_current_size
                .fetch_sub(entry.size, Ordering::Relaxed);
            self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
        }

        if let Some((_, entry)) = self.l2_cache.remove(key) {
            self.l2_current_size
                .fetch_sub(entry.size, Ordering::Relaxed);
            self.current_size.fetch_sub(entry.size, Ordering::Relaxed);
        }
    }

    /// Promote an entry from L2 to L1
    fn promote_to_l1(&self, key: &str, entry: CacheEntry) {
        // Remove from L2
        if let Some((_, old_entry)) = self.l2_cache.remove(key) {
            self.l2_current_size
                .fetch_sub(old_entry.size, Ordering::Relaxed);

            // Check if L1 has space or needs eviction
            let max_l1_size =
                (self.config.max_size_bytes() * self.config.hierarchy.l1_size_percent) / 100;
            let current_l1_size = self.l1_current_size.load(Ordering::Relaxed);

            if current_l1_size + entry.size > max_l1_size {
                // Evict from L1 to make space
                self.evict_from_l1_to_l2();
            }

            // Add to L1
            self.l1_current_size
                .fetch_add(entry.size, Ordering::Relaxed);
            self.l1_cache.insert(key.to_string(), entry);
            self.promotions.fetch_add(1, Ordering::Relaxed);

            debug!(key = %key, "Promoted entry from L2 to L1");
        }
    }

    /// Demote an entry from L1 to L2
    fn demote_to_l2(&self, key: &str, entry: CacheEntry) {
        // Remove from L1
        if let Some((_, old_entry)) = self.l1_cache.remove(key) {
            self.l1_current_size
                .fetch_sub(old_entry.size, Ordering::Relaxed);

            // Add to L2
            self.l2_current_size
                .fetch_add(entry.size, Ordering::Relaxed);
            self.l2_cache.insert(key.to_string(), entry);
            self.demotions.fetch_add(1, Ordering::Relaxed);

            debug!(key = %key, "Demoted entry from L1 to L2");
        }
    }

    /// Evict entries from L1 to L2 (used when L1 is full)
    fn evict_from_l1_to_l2(&self) {
        // Find coldest entries in L1 (lowest access count)
        let mut entries_by_score: Vec<(String, u64, CacheEntry)> = self
            .l1_cache
            .iter()
            .map(|e| {
                let recency = e.last_accessed.elapsed().as_secs().min(1000);
                let score = (e.access_count() as u64 * 1000).saturating_sub(recency);
                (e.key().clone(), score, e.value().clone())
            })
            .collect();

        // Sort by score ascending (lowest score = coldest = demote first)
        entries_by_score.sort_by_key(|(_, score, _)| *score);

        // Demote the coldest 10% of L1 entries to L2
        let demote_count = (entries_by_score.len() / 10).max(1);
        for (key, _, entry) in entries_by_score.into_iter().take(demote_count) {
            self.demote_to_l2(&key, entry);
        }
    }

    /// Add tags to a cache entry
    /// This updates both the entry's tags and the tag->keys index
    pub fn add_tags(&self, key: &str, tags: Vec<String>) {
        if tags.is_empty() {
            return;
        }

        // Limit number of tags per entry if configured
        let max_tags = if self.config.tags.enabled {
            self.config.tags.max_tags_per_entry
        } else {
            return; // Tags disabled
        };

        let tags_to_add: Vec<String> = tags.into_iter().take(max_tags).collect();

        // Helper to update entry tags
        let update_tags = |entry: &mut CacheEntry, tags: Vec<String>| {
            entry.cache_tags = tags.clone();
            for tag in tags {
                self.tag_to_keys
                    .entry(tag)
                    .or_insert_with(HashSet::new)
                    .insert(key.to_string());
            }
        };

        // Update the entry with tags (check both tiers if hierarchy enabled)
        let mut updated = false;

        if self.config.hierarchy.enabled {
            if let Some(mut entry) = self.l1_cache.get_mut(key) {
                update_tags(&mut entry, tags_to_add.clone());
                updated = true;
                debug!(key = %key, tier = "L1", tag_count = entry.cache_tags.len(), "Added tags to cache entry");
            } else if let Some(mut entry) = self.l2_cache.get_mut(key) {
                update_tags(&mut entry, tags_to_add.clone());
                updated = true;
                debug!(key = %key, tier = "L2", tag_count = entry.cache_tags.len(), "Added tags to cache entry");
            }
        } else {
            if let Some(mut entry) = self.entries.get_mut(key) {
                update_tags(&mut entry, tags_to_add.clone());
                updated = true;
                debug!(key = %key, tag_count = entry.cache_tags.len(), "Added tags to cache entry");
            }
        }

        if !updated {
            warn!(key = %key, "Failed to add tags: entry not found in cache");
        }
    }

    /// Invalidate all cache entries with a specific tag
    /// Returns the number of entries invalidated
    pub fn invalidate_by_tag(&self, tag: &str) -> usize {
        // Get all keys associated with this tag
        let keys_to_remove: Vec<String> = self
            .tag_to_keys
            .get(tag)
            .map(|keys_set| keys_set.iter().cloned().collect())
            .unwrap_or_default();

        let count = keys_to_remove.len();
        for key in keys_to_remove {
            self.invalidate(&key);
        }

        info!(tag = %tag, count = count, "Invalidated cache entries by tag");
        count
    }

    /// Get all tags currently in the cache
    pub fn get_all_tags(&self) -> Vec<String> {
        self.tag_to_keys
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get statistics about a specific tag
    pub fn get_tag_stats(&self, tag: &str) -> Option<TagStats> {
        self.tag_to_keys.get(tag).map(|keys_set| {
            let entry_count = keys_set.len();
            let total_size: usize = keys_set
                .iter()
                .filter_map(|key| self.entries.get(key))
                .map(|entry| entry.size)
                .sum();

            TagStats {
                tag: tag.to_string(),
                entry_count,
                total_size_bytes: total_size,
            }
        })
    }

    /// Get L1/L2 hierarchy statistics
    pub fn get_hierarchy_stats(&self) -> HierarchyStats {
        if !self.config.hierarchy.enabled {
            return HierarchyStats {
                enabled: false,
                l1_entries: 0,
                l2_entries: 0,
                l1_size_bytes: 0,
                l2_size_bytes: 0,
                l1_hits: 0,
                l2_hits: 0,
                promotions: 0,
                demotions: 0,
                l1_hit_ratio: 0.0,
                l2_hit_ratio: 0.0,
            };
        }

        let l1_hits = self.l1_hits.load(Ordering::Relaxed);
        let l2_hits = self.l2_hits.load(Ordering::Relaxed);
        let total_hits = l1_hits + l2_hits;

        HierarchyStats {
            enabled: true,
            l1_entries: self.l1_cache.len(),
            l2_entries: self.l2_cache.len(),
            l1_size_bytes: self.l1_current_size.load(Ordering::Relaxed),
            l2_size_bytes: self.l2_current_size.load(Ordering::Relaxed),
            l1_hits,
            l2_hits,
            promotions: self.promotions.load(Ordering::Relaxed),
            demotions: self.demotions.load(Ordering::Relaxed),
            l1_hit_ratio: if total_hits > 0 {
                l1_hits as f64 / total_hits as f64
            } else {
                0.0
            },
            l2_hit_ratio: if total_hits > 0 {
                l2_hits as f64 / total_hits as f64
            } else {
                0.0
            },
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

    #[test]
    fn test_cache_tags_basic() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default();
        let cache = Cache::new(config);

        // Create a cache entry
        let entry = CacheEntry {
            body: Bytes::from("test body"),
            headers: HashMap::new(),
            status_code: 200,
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            size: 9,
            stale_if_error_secs: None,
            access_count: 0,
            last_accessed: Instant::now(),
            cache_tags: Vec::new(),
        };

        // Store entry
        cache.set("test-key".to_string(), entry);

        // Add tags
        let tags = vec!["product-123".to_string(), "category-shoes".to_string()];
        cache.add_tags("test-key", tags);

        // Verify tags were added
        let stats = cache.get_tag_stats("product-123");
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.tag, "product-123");

        // Get all tags
        let all_tags = cache.get_all_tags();
        assert_eq!(all_tags.len(), 2);
        assert!(all_tags.contains(&"product-123".to_string()));
        assert!(all_tags.contains(&"category-shoes".to_string()));
    }

    #[test]
    fn test_cache_tags_invalidation() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default();
        let cache = Cache::new(config);

        // Create multiple entries with shared tags
        for i in 1..=3 {
            let entry = CacheEntry {
                body: Bytes::from(format!("body {}", i)),
                headers: HashMap::new(),
                status_code: 200,
                content_type: Some("text/html".to_string()),
                etag: None,
                last_modified: None,
                created_at: Instant::now(),
                expires_at: Instant::now() + Duration::from_secs(3600),
                size: 10,
                stale_if_error_secs: None,
                access_count: 0,
                last_accessed: Instant::now(),
                cache_tags: Vec::new(),
            };

            cache.set(format!("key-{}", i), entry);
            cache.add_tags(&format!("key-{}", i), vec!["common-tag".to_string()]);
        }

        // Verify all entries are cached
        let initial_stats = cache.stats();
        assert_eq!(initial_stats.total_entries, 3);
        assert_eq!(initial_stats.tagged_entries, 3);

        // Invalidate by tag
        let purged = cache.invalidate_by_tag("common-tag");
        assert_eq!(purged, 3);

        // Verify all entries were removed
        let final_stats = cache.stats();
        assert_eq!(final_stats.total_entries, 0);
        assert_eq!(final_stats.tagged_entries, 0);
    }

    #[test]
    fn test_l1_l2_hierarchy_disabled() {
        use crate::config::{CacheConfig, CacheHierarchyConfig};

        let mut config = CacheConfig::default();
        config.hierarchy = CacheHierarchyConfig {
            enabled: false,
            l1_size_percent: 20,
            l2_size_percent: 80,
            promotion_threshold: 3,
        };

        let cache = Cache::new(config);

        // Create entry
        let entry = CacheEntry {
            body: Bytes::from("test body"),
            headers: HashMap::new(),
            status_code: 200,
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            size: 9,
            stale_if_error_secs: None,
            access_count: 0,
            last_accessed: Instant::now(),
            cache_tags: Vec::new(),
        };

        cache.set("test-key".to_string(), entry);

        // Verify hierarchy is disabled
        let hierarchy_stats = cache.get_hierarchy_stats();
        assert!(!hierarchy_stats.enabled);

        // Verify we can still get the entry
        let result = cache.get("test-key");
        assert!(result.is_some());
    }

    #[test]
    fn test_l1_l2_hierarchy_enabled() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default(); // hierarchy enabled by default
        let cache = Cache::new(config);

        // Create a cold entry (access_count < threshold)
        let cold_entry = CacheEntry {
            body: Bytes::from("cold body"),
            headers: HashMap::new(),
            status_code: 200,
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            size: 9,
            stale_if_error_secs: None,
            access_count: 1, // Below threshold
            last_accessed: Instant::now(),
            cache_tags: Vec::new(),
        };

        cache.set("cold-key".to_string(), cold_entry);

        // Create a hot entry (access_count >= threshold)
        let hot_entry = CacheEntry {
            body: Bytes::from("hot body"),
            headers: HashMap::new(),
            status_code: 200,
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            size: 8,
            stale_if_error_secs: None,
            access_count: 3, // At threshold
            last_accessed: Instant::now(),
            cache_tags: Vec::new(),
        };

        cache.set("hot-key".to_string(), hot_entry);

        // Check hierarchy stats
        let hierarchy_stats = cache.get_hierarchy_stats();
        assert!(hierarchy_stats.enabled);
        assert_eq!(hierarchy_stats.l1_entries + hierarchy_stats.l2_entries, 2);

        // Hot entry should be in L1
        assert!(hierarchy_stats.l1_entries >= 1);
    }

    #[test]
    fn test_l1_l2_promotion() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default();
        let cache = Cache::new(config);

        // Create entry that starts in L2
        let entry = CacheEntry {
            body: Bytes::from("test body"),
            headers: HashMap::new(),
            status_code: 200,
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            size: 9,
            stale_if_error_secs: None,
            access_count: 1, // Below promotion threshold
            last_accessed: Instant::now(),
            cache_tags: Vec::new(),
        };

        cache.set("test-key".to_string(), entry);

        // Access it multiple times to trigger promotion
        for _ in 0..3 {
            let _ = cache.get("test-key");
        }

        // Check if promotions occurred
        let hierarchy_stats = cache.get_hierarchy_stats();
        // Promotion should have happened (access count incremented on each get)
        assert!(hierarchy_stats.promotions > 0 || hierarchy_stats.l1_hits > 0);
    }

    #[test]
    fn test_cache_stats_with_hierarchy() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default();
        let cache = Cache::new(config);

        // Add some entries
        for i in 0..5 {
            let entry = CacheEntry {
                body: Bytes::from(format!("body {}", i)),
                headers: HashMap::new(),
                status_code: 200,
                content_type: Some("text/html".to_string()),
                etag: None,
                last_modified: None,
                created_at: Instant::now(),
                expires_at: Instant::now() + Duration::from_secs(3600),
                size: 10,
                stale_if_error_secs: None,
                access_count: i as u32, // Varying access counts
                last_accessed: Instant::now(),
                cache_tags: Vec::new(),
            };

            cache.set(format!("key-{}", i), entry);
        }

        // Get stats
        let stats = cache.stats();
        assert_eq!(stats.total_entries, 5);
        assert!(stats.total_size_bytes > 0);

        // Some entries should be hot (access_count >= 3)
        assert!(stats.hot_entries >= 2); // keys 3 and 4
    }

    #[test]
    fn test_tag_and_hierarchy_integration() {
        use crate::config::CacheConfig;

        let config = CacheConfig::default();
        let cache = Cache::new(config);

        // Create entries with tags in both L1 and L2
        for i in 0..4 {
            let entry = CacheEntry {
                body: Bytes::from(format!("body {}", i)),
                headers: HashMap::new(),
                status_code: 200,
                content_type: Some("text/html".to_string()),
                etag: None,
                last_modified: None,
                created_at: Instant::now(),
                expires_at: Instant::now() + Duration::from_secs(3600),
                size: 10,
                stale_if_error_secs: None,
                access_count: if i >= 2 { 3 } else { 1 }, // Half hot, half cold
                last_accessed: Instant::now(),
                cache_tags: Vec::new(),
            };

            cache.set(format!("key-{}", i), entry);
            cache.add_tags(&format!("key-{}", i), vec!["test-tag".to_string()]);
        }

        // Verify tags work across both tiers
        let tag_stats = cache.get_tag_stats("test-tag");
        assert!(tag_stats.is_some());
        assert_eq!(tag_stats.unwrap().entry_count, 4);

        // Invalidate by tag should remove from both tiers
        let purged = cache.invalidate_by_tag("test-tag");
        assert_eq!(purged, 4);

        // Verify all removed
        let stats = cache.stats();
        assert_eq!(stats.total_entries, 0);
    }
}
