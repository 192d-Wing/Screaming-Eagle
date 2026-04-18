//! Content processing: compression negotiation, minification, image optimization hooks.

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Request, header},
    middleware::Next,
    response::Response,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

/// Supported compression algorithms, in order of preference when all else is equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Brotli,
    Zstd,
    Gzip,
    Identity,
}

impl Encoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Encoding::Brotli => "br",
            Encoding::Zstd => "zstd",
            Encoding::Gzip => "gzip",
            Encoding::Identity => "identity",
        }
    }

    fn from_token(token: &str) -> Option<Encoding> {
        match token.trim().to_ascii_lowercase().as_str() {
            "br" => Some(Encoding::Brotli),
            "zstd" => Some(Encoding::Zstd),
            "gzip" | "x-gzip" => Some(Encoding::Gzip),
            "identity" => Some(Encoding::Identity),
            _ => None,
        }
    }
}

/// Negotiate the best encoding from an Accept-Encoding header, respecting q-values.
///
/// Returns the best enabled encoding that the client accepts, or `Identity` if
/// none match.
pub fn negotiate_encoding(accept_encoding: Option<&str>, allowed: &[Encoding]) -> Encoding {
    let header = match accept_encoding {
        Some(h) if !h.is_empty() => h,
        _ => return Encoding::Identity,
    };

    let mut best: Option<(Encoding, f32)> = None;
    let mut identity_q = 1.0f32;
    let mut wildcard_q: Option<f32> = None;

    for entry in header.split(',') {
        let mut parts = entry.split(';');
        let token = parts.next().unwrap_or("").trim();
        let mut q = 1.0f32;
        for param in parts {
            let param = param.trim();
            if let Some(rest) = param.strip_prefix("q=") {
                q = rest.trim().parse().unwrap_or(1.0);
            }
        }

        if token == "*" {
            wildcard_q = Some(q);
            continue;
        }

        let encoding = match Encoding::from_token(token) {
            Some(e) => e,
            None => continue,
        };

        if encoding == Encoding::Identity {
            identity_q = q;
            continue;
        }

        if q <= 0.0 || !allowed.contains(&encoding) {
            continue;
        }

        // Prefer higher q, then our preferred order (Brotli > Zstd > Gzip).
        let replace = match best {
            None => true,
            Some((_, best_q)) if q > best_q => true,
            Some((cur, best_q)) if (q - best_q).abs() < f32::EPSILON => {
                encoding_rank(encoding) < encoding_rank(cur)
            }
            _ => false,
        };
        if replace {
            best = Some((encoding, q));
        }
    }

    if let Some((e, _)) = best {
        return e;
    }

    // Fall back to wildcard if client accepted "*".
    if let Some(q) = wildcard_q {
        if q > 0.0 {
            for &e in allowed {
                return e;
            }
        }
    }

    if identity_q > 0.0 {
        Encoding::Identity
    } else {
        // Client explicitly refused identity and offered nothing else.
        Encoding::Identity
    }
}

fn encoding_rank(e: Encoding) -> u8 {
    match e {
        Encoding::Brotli => 0,
        Encoding::Zstd => 1,
        Encoding::Gzip => 2,
        Encoding::Identity => 3,
    }
}

/// Content processing configuration bundle.
#[derive(Debug, Clone)]
pub struct ContentProcessor {
    pub config: ContentConfig,
}

/// Content processing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub compression: CompressionConfig,

    #[serde(default)]
    pub minification: MinificationConfig,

    #[serde(default)]
    pub image: ImageConfig,
}

impl Default for ContentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compression: CompressionConfig::default(),
            minification: MinificationConfig::default(),
            image: ImageConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Compression negotiation and prioritization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Enable Brotli.
    #[serde(default = "default_true")]
    pub brotli: bool,

    /// Enable Zstandard.
    #[serde(default)]
    pub zstd: bool,

    /// Enable gzip.
    #[serde(default = "default_true")]
    pub gzip: bool,

    /// Minimum response size to consider compressing (bytes).
    #[serde(default = "default_min_size")]
    pub min_size: usize,

    /// Content-Type prefixes eligible for compression.
    #[serde(default = "default_compressible_types")]
    pub compressible_types: Vec<String>,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            brotli: true,
            zstd: false,
            gzip: true,
            min_size: default_min_size(),
            compressible_types: default_compressible_types(),
        }
    }
}

fn default_min_size() -> usize {
    256
}

fn default_compressible_types() -> Vec<String> {
    vec![
        "text/".to_string(),
        "application/json".to_string(),
        "application/javascript".to_string(),
        "application/xml".to_string(),
        "application/rss+xml".to_string(),
        "application/atom+xml".to_string(),
        "application/xhtml+xml".to_string(),
        "image/svg+xml".to_string(),
        "font/ttf".to_string(),
        "font/otf".to_string(),
    ]
}

impl CompressionConfig {
    pub fn allowed_encodings(&self) -> Vec<Encoding> {
        let mut out = Vec::new();
        if self.brotli {
            out.push(Encoding::Brotli);
        }
        if self.zstd {
            out.push(Encoding::Zstd);
        }
        if self.gzip {
            out.push(Encoding::Gzip);
        }
        out
    }

    pub fn is_compressible_type(&self, content_type: &str) -> bool {
        let ct = content_type.split(';').next().unwrap_or("").trim();
        self.compressible_types
            .iter()
            .any(|prefix| ct.starts_with(prefix))
    }
}

/// On-the-fly minification for text resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinificationConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub html: bool,

    #[serde(default = "default_true")]
    pub css: bool,

    #[serde(default = "default_true")]
    pub js: bool,

    /// Maximum response size to minify (bytes). Larger responses pass through.
    #[serde(default = "default_minify_max")]
    pub max_size: usize,
}

impl Default for MinificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            html: true,
            css: true,
            js: true,
            max_size: default_minify_max(),
        }
    }
}

fn default_minify_max() -> usize {
    2 * 1024 * 1024
}

/// Image transformation configuration. Actual transforms require the `image`
/// crate; this config exposes the knobs and the middleware will record intent
/// via response headers so an image worker (or future embedded pipeline) can
/// pick them up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Query parameter that triggers resizing, e.g. `?w=600`.
    #[serde(default = "default_width_param")]
    pub width_param: String,

    /// Query parameter that triggers format conversion, e.g. `?fmt=webp`.
    #[serde(default = "default_format_param")]
    pub format_param: String,

    /// Query parameter that triggers quality adjustment, e.g. `?q=80`.
    #[serde(default = "default_quality_param")]
    pub quality_param: String,

    /// Allowed output formats.
    #[serde(default = "default_allowed_formats")]
    pub allowed_formats: Vec<String>,

    /// Maximum output width in pixels.
    #[serde(default = "default_max_width")]
    pub max_width: u32,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            width_param: default_width_param(),
            format_param: default_format_param(),
            quality_param: default_quality_param(),
            allowed_formats: default_allowed_formats(),
            max_width: default_max_width(),
        }
    }
}

fn default_width_param() -> String {
    "w".to_string()
}

fn default_format_param() -> String {
    "fmt".to_string()
}

fn default_quality_param() -> String {
    "q".to_string()
}

fn default_allowed_formats() -> Vec<String> {
    vec![
        "webp".to_string(),
        "avif".to_string(),
        "jpeg".to_string(),
        "png".to_string(),
    ]
}

fn default_max_width() -> u32 {
    4096
}

/// A parsed image transform request.
#[derive(Debug, Clone, Default)]
pub struct ImageTransform {
    pub width: Option<u32>,
    pub format: Option<String>,
    pub quality: Option<u8>,
}

impl ImageTransform {
    pub fn is_empty(&self) -> bool {
        self.width.is_none() && self.format.is_none() && self.quality.is_none()
    }
}

impl ContentProcessor {
    pub fn new(config: ContentConfig) -> Self {
        Self { config }
    }

    /// Parse an image-transform request from the query string.
    pub fn parse_image_transform(&self, query: Option<&str>) -> ImageTransform {
        let mut out = ImageTransform::default();
        if !self.config.image.enabled {
            return out;
        }
        let query = match query {
            Some(q) => q,
            None => return out,
        };
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            if k == self.config.image.width_param.as_str() {
                if let Ok(w) = v.parse::<u32>() {
                    out.width = Some(w.min(self.config.image.max_width));
                }
            } else if k == self.config.image.format_param.as_str() {
                let fmt = v.to_ascii_lowercase();
                if self.config.image.allowed_formats.iter().any(|f| f == &fmt) {
                    out.format = Some(fmt);
                }
            } else if k == self.config.image.quality_param.as_str() {
                if let Ok(q) = v.parse::<u8>() {
                    out.quality = Some(q.min(100));
                }
            }
        }
        out
    }

    /// Minify a text body in place when minification is enabled and the type
    /// is eligible.
    pub fn maybe_minify(&self, content_type: &str, body: &mut Bytes) {
        if !self.config.minification.enabled {
            return;
        }
        if body.len() > self.config.minification.max_size {
            return;
        }
        let ct = content_type.split(';').next().unwrap_or("").trim();
        let text = match std::str::from_utf8(body) {
            Ok(s) => s,
            Err(_) => return,
        };
        let minified: Option<String> = if self.config.minification.html
            && (ct == "text/html" || ct == "application/xhtml+xml")
        {
            Some(minify_html(text))
        } else if self.config.minification.css && ct == "text/css" {
            Some(minify_css(text))
        } else if self.config.minification.js
            && (ct == "application/javascript" || ct == "text/javascript")
        {
            Some(minify_js(text))
        } else {
            None
        };
        if let Some(s) = minified {
            *body = Bytes::from(s);
        }
    }
}

/// Minimal HTML whitespace minifier: collapses runs of whitespace outside of
/// `<pre>`, `<script>`, `<style>` tags and removes HTML comments (except IE
/// conditional comments).
pub fn minify_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut last_was_space = false;
    let mut in_preserve = false;
    let mut preserve_end: &[u8] = b"";

    while i < bytes.len() {
        // Skip comments <!-- ... --> (keep conditional comments <!--[if ...]>)
        if !in_preserve
            && bytes.get(i..i + 4) == Some(b"<!--")
            && bytes.get(i + 4) != Some(&b'[')
        {
            if let Some(end) = find_subslice(&bytes[i..], b"-->") {
                i += end + 3;
                continue;
            } else {
                break;
            }
        }

        if in_preserve {
            if bytes[i..].starts_with(preserve_end) {
                out.push_str(std::str::from_utf8(&bytes[i..i + preserve_end.len()]).unwrap_or(""));
                i += preserve_end.len();
                in_preserve = false;
                preserve_end = b"";
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Detect entry into preserve blocks.
        if bytes[i] == b'<' {
            for (open, close) in [
                (&b"<pre"[..], &b"</pre>"[..]),
                (&b"<script"[..], &b"</script>"[..]),
                (&b"<style"[..], &b"</style>"[..]),
                (&b"<textarea"[..], &b"</textarea>"[..]),
            ] {
                if bytes[i..].len() >= open.len()
                    && bytes[i..i + open.len()].eq_ignore_ascii_case(open)
                    && matches!(bytes.get(i + open.len()), Some(b' ') | Some(b'>') | Some(b'\t'))
                {
                    out.push_str(std::str::from_utf8(&bytes[i..i + open.len()]).unwrap_or(""));
                    i += open.len();
                    in_preserve = true;
                    preserve_end = close;
                    last_was_space = false;
                    break;
                }
            }
            if in_preserve {
                continue;
            }
        }

        let c = bytes[i];
        if c.is_ascii_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            i += 1;
        } else {
            out.push(c as char);
            last_was_space = false;
            i += 1;
        }
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Minimal CSS minifier: collapses whitespace, strips comments, removes
/// whitespace around structural punctuation.
pub fn minify_css(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut last_significant: Option<u8> = None;
    while i < bytes.len() {
        // Comment
        if bytes.get(i..i + 2) == Some(b"/*") {
            if let Some(end) = find_subslice(&bytes[i + 2..], b"*/") {
                i += end + 4;
                continue;
            } else {
                break;
            }
        }
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let next = bytes.get(j).copied();
            let skip = match (last_significant, next) {
                (Some(a), Some(b)) if is_css_punct(a) || is_css_punct(b) => true,
                (None, _) => true,
                (_, None) => true,
                _ => false,
            };
            if !skip {
                out.push(' ');
            }
            i = j;
            continue;
        }
        out.push(c as char);
        last_significant = Some(c);
        i += 1;
    }
    out
}

fn is_css_punct(b: u8) -> bool {
    matches!(b, b'{' | b'}' | b':' | b';' | b',' | b'>' | b'+' | b'~' | b'(' | b')')
}

/// Extremely conservative JS minifier: collapses runs of whitespace into
/// single spaces, strips line (`//`) and block (`/* */`) comments. Does not
/// rename identifiers or rewrite syntax.
pub fn minify_js(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string: Option<u8> = None;
    let mut last_was_space = false;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(quote) = in_string {
            out.push(c as char);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' || c == b'`' {
            in_string = Some(c);
            out.push(c as char);
            last_was_space = false;
            i += 1;
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"/*") {
            if let Some(end) = find_subslice(&bytes[i + 2..], b"*/") {
                i += end + 4;
                continue;
            } else {
                break;
            }
        }
        if c.is_ascii_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            i += 1;
            continue;
        }
        out.push(c as char);
        last_was_space = false;
        i += 1;
    }
    out
}

/// Middleware that:
///   - negotiates compression encoding and sets `Vary: Accept-Encoding`,
///   - minifies text bodies for eligible content types,
///   - annotates image-transform requests with `X-Image-Transform` so an
///     image worker can act on them.
pub async fn content_processing_middleware(
    State(processor): State<Arc<ContentProcessor>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !processor.config.enabled {
        return next.run(request).await;
    }

    let accept_encoding = request
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let query = request.uri().query().map(|s| s.to_string());
    let image_transform = processor.parse_image_transform(query.as_deref());

    let mut response = next.run(request).await;

    // Annotate image-transform intent on the response so downstream
    // processors can act; no-op when feature is disabled.
    if !image_transform.is_empty() {
        if let Ok(val) = HeaderValue::from_str(&format_image_transform(&image_transform)) {
            response.headers_mut().insert("x-image-transform", val);
        }
    }

    // Minify eligible text bodies when enabled.
    if processor.config.minification.enabled {
        if let Some(content_type) = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            // Only minify if we haven't already compressed upstream.
            let already_encoded = response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if !already_encoded {
                let (parts, body) = response.into_parts();
                let max = processor.config.minification.max_size;
                match to_bytes(body, max).await {
                    Ok(mut bytes) => {
                        processor.maybe_minify(&content_type, &mut bytes);
                        let mut resp = Response::from_parts(parts, Body::from(bytes.clone()));
                        resp.headers_mut()
                            .insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
                        response = resp;
                    }
                    Err(e) => {
                        warn!(error = %e, "minification skipped (body too large or stream error)");
                        response = Response::from_parts(parts, Body::empty());
                    }
                }
            }
        }
    }

    // Always advertise that the response varies by Accept-Encoding when
    // compression is enabled, so caches key correctly.
    if processor.config.compression.enabled {
        set_vary_accept_encoding(response.headers_mut());
        let allowed = processor.config.compression.allowed_encodings();
        let chosen = negotiate_encoding(accept_encoding.as_deref(), &allowed);
        debug!(chosen = %chosen.as_str(), "compression negotiated");
        if chosen != Encoding::Identity {
            // Only set the hint when content-type is compressible.
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if processor
                .config
                .compression
                .is_compressible_type(content_type)
            {
                if let Ok(val) = HeaderValue::from_str(chosen.as_str()) {
                    response.headers_mut().insert("x-compression-hint", val);
                }
            }
        }
    }

    response
}

fn format_image_transform(t: &ImageTransform) -> String {
    let mut parts = Vec::new();
    if let Some(w) = t.width {
        parts.push(format!("w={}", w));
    }
    if let Some(ref f) = t.format {
        parts.push(format!("fmt={}", f));
    }
    if let Some(q) = t.quality {
        parts.push(format!("q={}", q));
    }
    parts.join(";")
}

fn set_vary_accept_encoding(headers: &mut HeaderMap) {
    let existing = headers
        .get(header::VARY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if existing
        .split(',')
        .any(|t| t.trim().eq_ignore_ascii_case("accept-encoding"))
    {
        return;
    }
    let merged = if existing.is_empty() {
        "Accept-Encoding".to_string()
    } else {
        format!("{}, Accept-Encoding", existing)
    };
    if let Ok(val) = HeaderValue::from_str(&merged) {
        headers.insert(header::VARY, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_allowed() -> Vec<Encoding> {
        vec![Encoding::Brotli, Encoding::Zstd, Encoding::Gzip]
    }

    #[test]
    fn negotiation_picks_highest_q() {
        assert_eq!(
            negotiate_encoding(Some("gzip;q=0.5, br;q=1.0"), &all_allowed()),
            Encoding::Brotli
        );
    }

    #[test]
    fn negotiation_respects_preference_on_tie() {
        assert_eq!(
            negotiate_encoding(Some("gzip, br"), &all_allowed()),
            Encoding::Brotli
        );
        assert_eq!(
            negotiate_encoding(Some("gzip, zstd"), &all_allowed()),
            Encoding::Zstd
        );
    }

    #[test]
    fn negotiation_skips_disabled_encodings() {
        assert_eq!(
            negotiate_encoding(Some("br, gzip"), &[Encoding::Gzip]),
            Encoding::Gzip
        );
    }

    #[test]
    fn negotiation_falls_back_to_identity() {
        assert_eq!(
            negotiate_encoding(Some("deflate"), &all_allowed()),
            Encoding::Identity
        );
        assert_eq!(negotiate_encoding(None, &all_allowed()), Encoding::Identity);
    }

    #[test]
    fn negotiation_skips_zero_q() {
        assert_eq!(
            negotiate_encoding(Some("br;q=0, gzip;q=0.5"), &all_allowed()),
            Encoding::Gzip
        );
    }

    #[test]
    fn html_minifier_collapses_whitespace() {
        let out = minify_html("<html>\n  <body>   hello   world  </body>\n</html>");
        assert_eq!(out, "<html> <body> hello world </body> </html>");
    }

    #[test]
    fn html_minifier_preserves_pre() {
        let out = minify_html("<pre>\n  a\n  b\n</pre>");
        assert!(out.contains("a\n  b"));
    }

    #[test]
    fn html_minifier_strips_comments() {
        let out = minify_html("<div>a</div><!-- comment --><div>b</div>");
        assert_eq!(out, "<div>a</div><div>b</div>");
    }

    #[test]
    fn css_minifier_strips_comments_and_spaces() {
        let out = minify_css("/* hi */\nbody { color :  red ;  }");
        assert_eq!(out, "body{color:red;}");
    }

    #[test]
    fn js_minifier_strips_comments() {
        let out = minify_js("var x = 1; // comment\nvar y = /* inline */ 2;");
        assert_eq!(out.trim(), "var x = 1; var y = 2;");
    }

    #[test]
    fn js_minifier_preserves_strings() {
        let out = minify_js("var s = \"// not a comment\";");
        assert!(out.contains("// not a comment"));
    }

    #[test]
    fn compressible_type_matches_prefix() {
        let cfg = CompressionConfig::default();
        assert!(cfg.is_compressible_type("text/html; charset=utf-8"));
        assert!(cfg.is_compressible_type("application/json"));
        assert!(!cfg.is_compressible_type("image/png"));
    }

    #[test]
    fn image_transform_parses_query() {
        let mut cfg = ContentConfig::default();
        cfg.image.enabled = true;
        let proc_ = ContentProcessor::new(cfg);
        let t = proc_.parse_image_transform(Some("w=800&fmt=webp&q=85"));
        assert_eq!(t.width, Some(800));
        assert_eq!(t.format.as_deref(), Some("webp"));
        assert_eq!(t.quality, Some(85));
    }

    #[test]
    fn image_transform_clamps_width() {
        let mut cfg = ContentConfig::default();
        cfg.image.enabled = true;
        cfg.image.max_width = 1024;
        let proc_ = ContentProcessor::new(cfg);
        let t = proc_.parse_image_transform(Some("w=9999"));
        assert_eq!(t.width, Some(1024));
    }

    #[test]
    fn image_transform_rejects_disallowed_format() {
        let mut cfg = ContentConfig::default();
        cfg.image.enabled = true;
        let proc_ = ContentProcessor::new(cfg);
        let t = proc_.parse_image_transform(Some("fmt=tiff"));
        assert_eq!(t.format, None);
    }
}
