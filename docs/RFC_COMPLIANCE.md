# RFC Compliance Document

## Screaming Eagle CDN

This document outlines the RFC standards applicable to the Screaming Eagle CDN and our compliance status with each requirement.

---

## Applicable RFCs

### Core HTTP Standards
| RFC | Title | Relevance |
|-----|-------|-----------|
| RFC 9110 | HTTP Semantics | Core HTTP behavior, methods, headers, status codes |
| RFC 9111 | HTTP Caching | Cache storage, validation, expiration |
| RFC 9112 | HTTP/1.1 | Message syntax and routing |

### Caching Extensions
| RFC | Title | Relevance |
|-----|-------|-----------|
| RFC 5861 | HTTP Cache-Control Extensions for Stale Content | stale-while-revalidate, stale-if-error |

### Related Standards
| RFC | Title | Relevance |
|-----|-------|-----------|
| RFC 7239 | Forwarded HTTP Extension | Client IP forwarding |
| RFC 8446 | TLS 1.3 | HTTPS support |
| RFC 6648 | Deprecating X- Prefix | Custom header naming |

---

## RFC 9110 - HTTP Semantics

### Section 8.4 - Content Codings (Compression)

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Support gzip content-coding | COMPLIANT | `tower-http` CompressionLayer; reqwest client decompression |
| Support deflate content-coding | PARTIAL | Not explicitly enabled |
| Support br (Brotli) content-coding | COMPLIANT | Enabled in both server and client |
| Accept-Encoding header handling | COMPLIANT | Handled by tower-http middleware |
| Content-Encoding header on responses | COMPLIANT | Set by compression layer |

### Section 8.8 - Validators

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| ETag generation | COMPLIANT | Auto-generated using xxHash3 if not from origin |
| ETag forwarding from origin | COMPLIANT | Preserved in cache entries |
| Last-Modified forwarding | COMPLIANT | Extracted and stored from origin |
| Strong vs Weak ETags | PARTIAL | All ETags treated equally; no weak ETag handling |

**Gap:** Weak ETags (prefixed with `W/`) should be handled differently for range requests.

### Section 9 - Methods

| Method | Status | Notes |
|--------|--------|-------|
| GET | COMPLIANT | Primary method for CDN content |
| HEAD | COMPLIANT | Returns headers without body, implemented via Method extractor |
| POST | PARTIAL | Only for admin API, not proxied |
| PUT | NOT IMPLEMENTED | Not required for CDN |
| DELETE | NOT IMPLEMENTED | Not required for CDN |
| OPTIONS | PARTIAL | CORS preflight handled by middleware |

### Section 10 - Message Context

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Host header handling | COMPLIANT | Configurable per-origin host_header |
| Date header | COMPLIANT | Added via `build_response()` using chrono UTC formatting |
| Via header | COMPLIANT | Added `Via: 1.1 screaming-eagle` to all responses |

### Section 12 - Content Negotiation

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Accept header forwarding | COMPLIANT | Forwarded to origin |
| Accept-Encoding handling | COMPLIANT | Handled by compression layer |
| Accept-Language forwarding | COMPLIANT | Forwarded to origin |
| Vary header handling | COMPLIANT | Vary header values included in cache key via `generate_cache_key_with_vary()` |

### Section 13 - Conditional Requests

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| If-None-Match handling | COMPLIANT | Forwarded to origin |
| If-Modified-Since handling | COMPLIANT | Forwarded to origin |
| 304 Not Modified responses | COMPLIANT | Recognized and handled |
| If-Match handling | NOT IMPLEMENTED | Not forwarded |
| If-Unmodified-Since handling | NOT IMPLEMENTED | Not forwarded |

### Section 14 - Range Requests

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Range header parsing | NOT IMPLEMENTED | All requests return full content |
| Accept-Ranges header | NOT IMPLEMENTED | Should indicate bytes or none |
| Content-Range header | NOT IMPLEMENTED | Required for 206 responses |
| 206 Partial Content status | NOT IMPLEMENTED | Not supported |
| 416 Range Not Satisfiable | NOT IMPLEMENTED | Not supported |

**Gap:** Range requests are essential for large file delivery and video streaming.

### Section 15 - Status Codes

| Status Code | Status | Usage |
|-------------|--------|-------|
| 200 OK | COMPLIANT | Successful responses |
| 206 Partial Content | NOT IMPLEMENTED | Range requests |
| 304 Not Modified | COMPLIANT | Conditional request validation |
| 400 Bad Request | COMPLIANT | Invalid requests |
| 404 Not Found | COMPLIANT | Unknown origin/path |
| 429 Too Many Requests | COMPLIANT | Rate limiting |
| 500 Internal Server Error | COMPLIANT | Server errors |
| 502 Bad Gateway | COMPLIANT | Origin errors |
| 503 Service Unavailable | COMPLIANT | Origin unreachable |

---

## RFC 9111 - HTTP Caching

### Section 3 - Storing Responses in Caches

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Store responses with explicit freshness | COMPLIANT | max-age, s-maxage respected |
| Respect no-store directive | COMPLIANT | Content not cached |
| Respect private directive | COMPLIANT | Content not cached in shared cache |
| Store responses with status 200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 501 | PARTIAL | Only 200 range cached |

**Gap:** Should cache certain error responses (404, 410) if explicitly cacheable.

### Section 4 - Constructing Responses from Caches

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Serve fresh responses | COMPLIANT | TTL-based freshness |
| Validate stale responses | COMPLIANT | Conditional requests sent |
| Age header calculation | COMPLIANT | Added to cached responses via `cache_age_secs` calculation |
| Warning header for stale content | DEPRECATED | No longer required in RFC 9111 |

### Section 5 - Field Definitions

#### Cache-Control Directives

| Directive | Status | Implementation |
|-----------|--------|----------------|
| max-age | COMPLIANT | Parsed and used for TTL |
| s-maxage | COMPLIANT | Takes precedence for shared cache |
| no-cache | COMPLIANT | Forces revalidation |
| no-store | COMPLIANT | Prevents caching |
| private | COMPLIANT | Prevents shared cache storage |
| public | COMPLIANT | Parsed but implicit for shared cache |
| must-revalidate | PARTIAL | Parsed but not enforced after expiry |
| proxy-revalidate | NOT IMPLEMENTED | Specific to proxy caches |
| no-transform | NOT IMPLEMENTED | Should prevent modifications |
| only-if-cached | NOT IMPLEMENTED | Client directive |
| max-stale | NOT IMPLEMENTED | Client directive |
| min-fresh | NOT IMPLEMENTED | Client directive |

**Gap:** must-revalidate should prevent serving stale content without validation.

### Section 5.2.2.6 - Stale Responses

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Serve stale on origin failure | PARTIAL | Only with stale-while-revalidate |
| stale-if-error handling | NOT IMPLEMENTED | RFC 5861 extension |

---

## RFC 5861 - Cache-Control Extensions for Stale Content

| Directive | Status | Implementation |
|-----------|--------|----------------|
| stale-while-revalidate | COMPLIANT | Background revalidation during stale window |
| stale-if-error | NOT IMPLEMENTED | Should serve stale on 5xx errors |

**Gap:** stale-if-error is valuable for resilience during origin failures.

---

## RFC 7239 - Forwarded HTTP Extension

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| Forwarded header support | NOT IMPLEMENTED | Uses X-Forwarded-For instead |
| X-Forwarded-For parsing | COMPLIANT | First IP extracted |
| X-Real-IP parsing | COMPLIANT | Fallback if X-Forwarded-For missing |

**Note:** X-Forwarded-For is legacy but widely supported. RFC 7239 Forwarded header is preferred.

---

## RFC 8446 - TLS 1.3

| Requirement | Status | Implementation |
|-------------|--------|----------------|
| TLS 1.3 support | COMPLIANT | Via rustls |
| Certificate configuration | COMPLIANT | cert_path, key_path in config |
| Graceful TLS termination | COMPLIANT | axum-server with graceful shutdown |

---

## Compliance Summary

### Fully Compliant
- Cache-Control parsing (core directives)
- ETag generation and forwarding
- Conditional requests (If-None-Match, If-Modified-Since)
- Compression (gzip, brotli)
- stale-while-revalidate
- Rate limiting (429 responses)
- TLS 1.3 support
- CORS handling
- **HEAD method** (RFC 9110 Section 9)
- **Age header** (RFC 9111 Section 4)
- **Date header** (RFC 9110 Section 10)
- **Via header** (RFC 9110 Section 10)
- **Vary-based cache keying** (RFC 9111)

### Partially Compliant
- must-revalidate (parsed but not strictly enforced)
- Status code caching (only 200 range)

### Not Implemented
- Range requests (RFC 9110 Section 14)
- stale-if-error
- Forwarded header (RFC 7239)
- no-transform directive

---

## Priority Remediation Plan

### High Priority (Essential for CDN)

1. **Range Requests (RFC 9110 Section 14)**
   - Required for video streaming, large file downloads
   - Implement Range header parsing
   - Add Accept-Ranges: bytes header
   - Return 206 Partial Content with Content-Range

2. ~~**Age Header (RFC 9111)**~~ ✅ IMPLEMENTED
   - ~~Add Age header indicating time in cache~~
   - ~~Calculate from entry creation time~~

3. ~~**HEAD Method Support**~~ ✅ IMPLEMENTED
   - ~~Return headers without body~~
   - ~~Essential for cache validation tools~~

### Medium Priority (Improved Compliance)

4. ~~**Vary Header Cache Keying**~~ ✅ IMPLEMENTED
   - ~~Include Vary header values in cache key~~
   - ~~Prevent serving wrong content variants~~

5. ~~**Date Header**~~ ✅ IMPLEMENTED
   - ~~Add Date header to all responses~~
   - ~~Use response generation time~~

6. ~~**Via Header**~~ ✅ IMPLEMENTED
   - ~~Add Via: 1.1 screaming-eagle~~
   - ~~Identify proxy in request chain~~

7. **stale-if-error**
   - Serve stale content on 5xx origin errors
   - Improve availability during outages

### Low Priority (Full Compliance)

8. **must-revalidate Enforcement**
   - Never serve stale without validation when set

9. **Cacheable Error Responses**
   - Cache 404, 410 if explicitly cacheable

10. **Forwarded Header (RFC 7239)**
    - Parse and generate standard Forwarded header

---

## Implementation Checklist

- [ ] Add Range request support
- [x] Implement HEAD method
- [x] Add Age header to cached responses
- [x] Add Date header to all responses
- [x] Add Via header
- [x] Implement Vary-based cache keying
- [x] Add Accept-Ranges header (currently returns `none`)
- [ ] Add stale-if-error support
- [ ] Enforce must-revalidate strictly
- [ ] Parse Forwarded header

---

## Version History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2026-01-18 | Initial compliance assessment |
| 1.1 | 2026-01-18 | Implemented HEAD method, Age/Date/Via headers, Vary-based cache keying |
