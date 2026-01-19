//! HTTP Range Request Support (RFC 9110 Section 14)
//!
//! This module implements byte-range requests for partial content delivery,
//! essential for video streaming and resumable downloads.

use bytes::Bytes;

/// Represents a parsed byte range from a Range header
#[derive(Debug, Clone, PartialEq)]
pub struct ByteRange {
    /// Start position (inclusive)
    pub start: u64,
    /// End position (inclusive)
    pub end: u64,
}

impl ByteRange {
    /// Create a new byte range
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// Get the length of this range
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Check if this range is satisfiable for a given content length
    pub fn is_satisfiable(&self, content_length: u64) -> bool {
        self.start < content_length && self.end < content_length && self.start <= self.end
    }

    /// Format as Content-Range header value
    pub fn content_range_header(&self, total_length: u64) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, total_length)
    }
}

/// Result of parsing a Range header
#[derive(Debug, Clone)]
pub enum RangeParseResult {
    /// Valid single range
    Single(ByteRange),
    /// Valid multiple ranges (not currently supported, will return full content)
    Multiple(Vec<ByteRange>),
    /// Invalid range syntax
    Invalid,
    /// No range header present
    None,
}

/// Parse a Range header value
///
/// Supports formats:
/// - `bytes=0-499` (first 500 bytes)
/// - `bytes=500-999` (second 500 bytes)
/// - `bytes=-500` (last 500 bytes)
/// - `bytes=500-` (from byte 500 to end)
/// - `bytes=0-0,-1` (first and last byte - multiple ranges)
pub fn parse_range_header(header: &str, content_length: u64) -> RangeParseResult {
    // Must start with "bytes="
    let range_spec = match header.strip_prefix("bytes=") {
        Some(spec) => spec.trim(),
        None => return RangeParseResult::Invalid,
    };

    if range_spec.is_empty() {
        return RangeParseResult::Invalid;
    }

    // Split by comma for multiple ranges
    let range_parts: Vec<&str> = range_spec.split(',').map(|s| s.trim()).collect();

    if range_parts.is_empty() {
        return RangeParseResult::Invalid;
    }

    let mut ranges = Vec::new();

    for part in range_parts {
        if let Some(range) = parse_single_range(part, content_length) {
            ranges.push(range);
        } else {
            // Invalid range in the set
            return RangeParseResult::Invalid;
        }
    }

    if ranges.is_empty() {
        return RangeParseResult::Invalid;
    }

    if ranges.len() == 1 {
        RangeParseResult::Single(ranges.remove(0))
    } else {
        RangeParseResult::Multiple(ranges)
    }
}

/// Parse a single range specification
fn parse_single_range(spec: &str, content_length: u64) -> Option<ByteRange> {
    if content_length == 0 {
        return None;
    }

    let parts: Vec<&str> = spec.splitn(2, '-').collect();
    if parts.len() != 2 {
        return None;
    }

    let start_str = parts[0].trim();
    let end_str = parts[1].trim();

    // Suffix range: -500 means last 500 bytes
    if start_str.is_empty() {
        let suffix_length: u64 = end_str.parse().ok()?;
        if suffix_length == 0 {
            return None;
        }
        let start = content_length.saturating_sub(suffix_length);
        let end = content_length - 1;
        return Some(ByteRange::new(start, end));
    }

    let start: u64 = start_str.parse().ok()?;

    // Open-ended range: 500- means from byte 500 to end
    if end_str.is_empty() {
        if start >= content_length {
            return None;
        }
        return Some(ByteRange::new(start, content_length - 1));
    }

    // Explicit range: 0-499
    let end: u64 = end_str.parse().ok()?;

    // Validate range
    if start > end {
        return None;
    }

    // Clamp end to content length
    let end = std::cmp::min(end, content_length - 1);

    if start >= content_length {
        return None;
    }

    Some(ByteRange::new(start, end))
}

/// Extract a byte range from content
pub fn extract_range(content: &Bytes, range: &ByteRange) -> Bytes {
    let start = range.start as usize;
    let end = (range.end + 1) as usize;
    content.slice(start..std::cmp::min(end, content.len()))
}

/// Check if the request should use range response
/// Returns None if full content should be served
pub fn should_serve_range(
    range_header: Option<&str>,
    content_length: u64,
    _has_strong_validator: bool,
) -> Option<RangeParseResult> {
    let header = range_header?;

    // RFC 9110: A server MUST ignore a Range header field received with a
    // request method that is unrecognized or for which range handling is not defined.
    // For our CDN, we only support Range for GET requests.

    // Parse the range
    let result = parse_range_header(header, content_length);

    match &result {
        RangeParseResult::Single(range) => {
            // Check if range is satisfiable
            if range.is_satisfiable(content_length) {
                Some(result)
            } else {
                Some(RangeParseResult::Invalid)
            }
        }
        RangeParseResult::Multiple(_) => {
            // For simplicity, we don't support multipart ranges yet
            // Return None to serve full content
            None
        }
        RangeParseResult::Invalid => Some(RangeParseResult::Invalid),
        RangeParseResult::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_header_single() {
        // Standard range
        match parse_range_header("bytes=0-499", 1000) {
            RangeParseResult::Single(range) => {
                assert_eq!(range.start, 0);
                assert_eq!(range.end, 499);
                assert_eq!(range.length(), 500);
            }
            _ => panic!("Expected single range"),
        }

        // Open-ended range
        match parse_range_header("bytes=500-", 1000) {
            RangeParseResult::Single(range) => {
                assert_eq!(range.start, 500);
                assert_eq!(range.end, 999);
                assert_eq!(range.length(), 500);
            }
            _ => panic!("Expected single range"),
        }

        // Suffix range
        match parse_range_header("bytes=-200", 1000) {
            RangeParseResult::Single(range) => {
                assert_eq!(range.start, 800);
                assert_eq!(range.end, 999);
                assert_eq!(range.length(), 200);
            }
            _ => panic!("Expected single range"),
        }
    }

    #[test]
    fn test_parse_range_header_clamping() {
        // End beyond content length should be clamped
        match parse_range_header("bytes=0-9999", 1000) {
            RangeParseResult::Single(range) => {
                assert_eq!(range.start, 0);
                assert_eq!(range.end, 999);
            }
            _ => panic!("Expected single range"),
        }
    }

    #[test]
    fn test_parse_range_header_invalid() {
        // Invalid prefix
        assert!(matches!(
            parse_range_header("invalid=0-499", 1000),
            RangeParseResult::Invalid
        ));

        // Start > End
        assert!(matches!(
            parse_range_header("bytes=500-100", 1000),
            RangeParseResult::Invalid
        ));

        // Start beyond content
        assert!(matches!(
            parse_range_header("bytes=2000-", 1000),
            RangeParseResult::Invalid
        ));

        // Empty range spec
        assert!(matches!(
            parse_range_header("bytes=", 1000),
            RangeParseResult::Invalid
        ));
    }

    #[test]
    fn test_parse_range_header_multiple() {
        match parse_range_header("bytes=0-100, 200-300", 1000) {
            RangeParseResult::Multiple(ranges) => {
                assert_eq!(ranges.len(), 2);
                assert_eq!(ranges[0].start, 0);
                assert_eq!(ranges[0].end, 100);
                assert_eq!(ranges[1].start, 200);
                assert_eq!(ranges[1].end, 300);
            }
            _ => panic!("Expected multiple ranges"),
        }
    }

    #[test]
    fn test_extract_range() {
        let content = Bytes::from("Hello, World!");
        let range = ByteRange::new(0, 4);
        let extracted = extract_range(&content, &range);
        assert_eq!(extracted.as_ref(), b"Hello");

        let range = ByteRange::new(7, 11);
        let extracted = extract_range(&content, &range);
        assert_eq!(extracted.as_ref(), b"World");
    }

    #[test]
    fn test_content_range_header() {
        let range = ByteRange::new(0, 499);
        assert_eq!(range.content_range_header(1000), "bytes 0-499/1000");

        let range = ByteRange::new(500, 999);
        assert_eq!(range.content_range_header(1000), "bytes 500-999/1000");
    }

    #[test]
    fn test_suffix_range_larger_than_content() {
        // Request last 2000 bytes of 1000 byte content
        match parse_range_header("bytes=-2000", 1000) {
            RangeParseResult::Single(range) => {
                assert_eq!(range.start, 0);
                assert_eq!(range.end, 999);
            }
            _ => panic!("Expected single range"),
        }
    }
}
