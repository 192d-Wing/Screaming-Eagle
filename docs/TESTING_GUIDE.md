# Advanced Caching Features - Testing Guide

This guide provides comprehensive instructions for testing the newly implemented advanced caching features: Cache Tags and L1/L2 Hierarchy.

## Prerequisites

- Screaming Eagle CDN running locally or on a server
- `curl` or similar HTTP client
- `jq` for JSON parsing (optional but recommended)

## Phase 1: Cache Tags Testing

### 1.1 Basic Tag Storage and Retrieval

**Store a response with cache tags:**

```bash
# Start the CDN (if not already running)
cargo run --release

# Make a request that will be cached
# The origin server should return a Cache-Tag header
curl -i http://localhost:8080/product/123

# If you're using a test origin that supports Cache-Tag headers:
# Cache-Tag: product-123, category-shoes, brand-nike
```

### 1.2 Tag-Based Invalidation

**Purge all entries with a specific tag:**

```bash
# Purge all entries tagged with "category-shoes"
curl -X POST http://localhost:8080/_admin/purge \
  -H "Content-Type: application/json" \
  -d '{"tag": "category-shoes"}'

# Expected response:
# {
#   "success": true,
#   "message": "Purged N cache entries",
#   "purged_count": N
# }
```

**Verify the purge:**

```bash
# Request the same resource again - should be a cache MISS
curl -i http://localhost:8080/product/123

# Check the X-Cache-Status header - should show "MISS"
```

### 1.3 Multiple Tags Per Entry

**Test multiple tags on a single entry:**

```bash
# Store entry with multiple tags
# Origin should return: Cache-Tag: tag1, tag2, tag3

# Purge by one tag should invalidate all entries with that tag
curl -X POST http://localhost:8080/_admin/purge \
  -H "Content-Type: application/json" \
  -d '{"tag": "tag1"}'
```

### 1.4 Cache Statistics with Tags

**View tag statistics:**

```bash
# Get overall cache stats
curl http://localhost:8080/_admin/stats | jq '.'

# Look for:
# {
#   ...
#   "total_tags": N,
#   "tagged_entries": M,
#   ...
# }
```

## Phase 2: L1/L2 Hierarchy Testing

### 2.1 Verify Hierarchy is Enabled

**Check configuration:**

```toml
# In your config.toml, ensure:
[cache.hierarchy]
enabled = true
l1_size_percent = 20
l2_size_percent = 80
promotion_threshold = 3
```

**Get hierarchy statistics:**

```bash
curl http://localhost:8080/_admin/hierarchy-stats | jq '.'

# Expected response:
# {
#   "enabled": true,
#   "l1_entries": N,
#   "l2_entries": M,
#   "l1_size_bytes": X,
#   "l2_size_bytes": Y,
#   "l1_hits": A,
#   "l2_hits": B,
#   "promotions": P,
#   "demotions": D,
#   "l1_hit_ratio": R1,
#   "l2_hit_ratio": R2
# }
```

### 2.2 Test L2 to L1 Promotion

**Access an entry multiple times to trigger promotion:**

```bash
# First request - entry goes to L2 (cold tier)
curl -i http://localhost:8080/test-page

# Access it 2 more times to reach promotion threshold (3)
curl -i http://localhost:8080/test-page
curl -i http://localhost:8080/test-page

# Check hierarchy stats to see promotion
curl http://localhost:8080/_admin/hierarchy-stats | jq '.promotions'

# Should show at least 1 promotion
```

### 2.3 Test L1/L2 Hit Ratios

**Generate traffic to see tier distribution:**

```bash
# Access different URLs with varying frequency
for i in {1..5}; do
  # Hot pages (access multiple times)
  for j in {1..5}; do
    curl -s http://localhost:8080/hot-page-$i > /dev/null
  done

  # Cold pages (access once)
  curl -s http://localhost:8080/cold-page-$i > /dev/null
done

# Check hierarchy stats
curl http://localhost:8080/_admin/hierarchy-stats | jq '{
  l1_entries,
  l2_entries,
  l1_hit_ratio,
  l2_hit_ratio,
  promotions
}'
```

### 2.4 Test Hot vs Cold Entry Distribution

```bash
# Create traffic pattern
# Hot entries should end up in L1
# Cold entries should stay in L2

# Check overall cache stats
curl http://localhost:8080/_admin/stats | jq '{
  total_entries,
  hot_entries,
  avg_entry_size_bytes
}'

# Compare with hierarchy stats
curl http://localhost:8080/_admin/hierarchy-stats | jq '{
  l1_entries,
  l2_entries
}'

# L1 should contain mostly hot entries
```

## Integrated Testing: Tags + Hierarchy

### 3.1 Tag Invalidation Across Both Tiers

**Create entries in both L1 and L2 with same tag:**

```bash
# Create some hot entries (L1)
for i in {1..5}; do
  curl http://localhost:8080/product/$i
  curl http://localhost:8080/product/$i
  curl http://localhost:8080/product/$i
done

# Create some cold entries (L2)
for i in {6..10}; do
  curl http://localhost:8080/product/$i
done

# All should have tag "product" from origin

# Purge by tag
curl -X POST http://localhost:8080/_admin/purge \
  -H "Content-Type: application/json" \
  -d '{"tag": "product"}'

# Verify all removed from both tiers
curl http://localhost:8080/_admin/hierarchy-stats | jq '{
  l1_entries,
  l2_entries
}'
```

### 3.2 Verify Tag Tracking Works in Both Tiers

```bash
# After creating entries in both tiers with tags
curl http://localhost:8080/_admin/stats | jq '{
  total_tags,
  tagged_entries,
  total_entries
}'

# tagged_entries should equal total_entries if all have tags
```

## Performance Testing

### 4.1 L1 vs L2 Hit Performance

```bash
# Measure L1 hit performance (should be fastest)
time for i in {1..1000}; do
  curl -s http://localhost:8080/hot-page > /dev/null
done

# Measure L2 hit performance (slightly slower)
time for i in {1..1000}; do
  curl -s http://localhost:8080/cold-page > /dev/null
done

# Check hierarchy stats to confirm tier distribution
curl http://localhost:8080/_admin/hierarchy-stats | jq '{
  l1_hits,
  l2_hits,
  l1_hit_ratio,
  l2_hit_ratio
}'
```

### 4.2 Tag Invalidation Performance

```bash
# Create many entries with same tag
for i in {1..100}; do
  curl -s http://localhost:8080/item/$i > /dev/null
done

# Time tag-based purge
time curl -X POST http://localhost:8080/_admin/purge \
  -H "Content-Type: application/json" \
  -d '{"tag": "common-tag"}'

# Should complete quickly (< 100ms for 100 entries)
```

## Configuration Testing

### 5.1 Test Different L1/L2 Ratios

**Modify config.toml:**

```toml
[cache.hierarchy]
enabled = true
l1_size_percent = 30  # Increase L1 size
l2_size_percent = 70
promotion_threshold = 3
```

**Restart and verify:**

```bash
# Restart CDN
cargo run --release

# Generate traffic and check distribution
curl http://localhost:8080/_admin/hierarchy-stats
```

### 5.2 Test Different Promotion Thresholds

```toml
[cache.hierarchy]
enabled = true
l1_size_percent = 20
l2_size_percent = 80
promotion_threshold = 5  # Require 5 accesses for promotion
```

**Verify promotion behavior:**

```bash
# Access entry 4 times - should stay in L2
for i in {1..4}; do
  curl http://localhost:8080/test > /dev/null
done

curl http://localhost:8080/_admin/hierarchy-stats | jq '.promotions'
# Should be 0

# Access once more - should promote to L1
curl http://localhost:8080/test > /dev/null

curl http://localhost:8080/_admin/hierarchy-stats | jq '.promotions'
# Should be 1
```

### 5.3 Disable Hierarchy

```toml
[cache.hierarchy]
enabled = false
```

**Verify fallback to single-tier:**

```bash
curl http://localhost:8080/_admin/hierarchy-stats | jq '.enabled'
# Should return false

# Cache should still work normally
curl -i http://localhost:8080/test
# Should get cached responses
```

## Troubleshooting

### Issue: Tags not being stored

**Check:**
1. Ensure origin server returns `Cache-Tag` header
2. Verify `cache.tags.enabled = true` in config
3. Check logs for tag-related warnings

### Issue: No promotions happening

**Check:**
1. Verify `cache.hierarchy.enabled = true`
2. Check promotion_threshold - ensure accessing enough times
3. Review hierarchy stats for L2 hits

### Issue: Entries not being purged by tag

**Check:**
1. Verify tag name matches exactly (case-sensitive)
2. Check that entries have the tag (view cache stats)
3. Look for tag-related errors in logs

## Expected Test Results

### Successful Cache Tags Implementation

- ✅ Entries store with tags from Cache-Tag header
- ✅ Tag-based purge removes all matching entries
- ✅ Multiple tags per entry work correctly
- ✅ Cache stats show accurate tag counts
- ✅ Tags work across L1/L2 tiers

### Successful L1/L2 Hierarchy Implementation

- ✅ Hot entries (access_count >= threshold) go to L1
- ✅ Cold entries start in L2
- ✅ L2 entries promote to L1 after reaching threshold
- ✅ L1 hit ratio is higher than L2 hit ratio for hot content
- ✅ Hierarchy stats show accurate metrics
- ✅ System gracefully falls back when hierarchy disabled

## Unit Tests

Run the comprehensive unit test suite:

```bash
# Run all cache tests
cargo test cache::tests

# Expected output:
# running 10 tests
# test cache::tests::test_cache_tags_basic ... ok
# test cache::tests::test_cache_tags_invalidation ... ok
# test cache::tests::test_l1_l2_hierarchy_disabled ... ok
# test cache::tests::test_l1_l2_hierarchy_enabled ... ok
# test cache::tests::test_l1_l2_promotion ... ok
# test cache::tests::test_cache_stats_with_hierarchy ... ok
# test cache::tests::test_tag_and_hierarchy_integration ... ok
# ... and more

# Run all tests
cargo test

# Expected: All 62+ tests pass
```

## Benchmarking

For performance benchmarking:

```bash
# Build release version
cargo build --release

# Use a tool like wrk or ab for load testing
wrk -t4 -c100 -d30s http://localhost:8080/test-page

# Monitor hierarchy stats during load
watch -n 1 'curl -s http://localhost:8080/_admin/hierarchy-stats | jq "{l1_hits, l2_hits, promotions}"'
```

## Summary

The advanced caching features are production-ready with:
- **Cache Tags**: Full tag-based invalidation support
- **L1/L2 Hierarchy**: Intelligent two-tier caching with automatic promotion/demotion
- **62+ passing tests**: Comprehensive test coverage
- **Configurable**: All features can be tuned or disabled
- **Backwards compatible**: Works with existing cache infrastructure

For Phase 3 (ESI Processing), refer to the plan document for implementation details.
