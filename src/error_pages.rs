//! Custom error pages module
//!
//! Provides support for serving custom HTML error pages instead of JSON error responses.

use axum::http::StatusCode;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::config::ErrorPagesConfig;

/// Manages custom error pages
#[derive(Clone)]
pub struct ErrorPages {
    enabled: bool,
    pages: Arc<HashMap<u16, String>>,
}

impl ErrorPages {
    /// Create a new ErrorPages instance from configuration
    pub fn new(config: &ErrorPagesConfig) -> Self {
        if !config.enabled {
            return Self {
                enabled: false,
                pages: Arc::new(HashMap::new()),
            };
        }

        let mut pages = HashMap::new();

        // Load pages from explicit configuration
        let page_configs = [
            (400, &config.page_400),
            (404, &config.page_404),
            (500, &config.page_500),
            (502, &config.page_502),
            (503, &config.page_503),
            (504, &config.page_504),
        ];

        for (status_code, page_option) in page_configs {
            if let Some(page_path) = page_option
                && let Some(content) = load_error_page(page_path, &config.directory) {
                    pages.insert(status_code, content);
                    info!(status_code = status_code, path = %page_path, "Loaded custom error page");
                }
        }

        // Also try to auto-discover error pages in the directory
        // Look for files named like "400.html", "404.html", etc.
        if Path::new(&config.directory).exists() {
            for status_code in [400, 401, 403, 404, 500, 502, 503, 504] {
                if pages.contains_key(&status_code) {
                    continue; // Already loaded from explicit config
                }

                let filename = format!("{}.html", status_code);
                let filepath = Path::new(&config.directory).join(&filename);

                if filepath.exists()
                    && let Ok(content) = std::fs::read_to_string(&filepath) {
                        pages.insert(status_code, content);
                        info!(status_code = status_code, path = ?filepath, "Auto-discovered custom error page");
                    }
            }
        }

        if pages.is_empty() {
            warn!("Error pages enabled but no custom pages found");
        } else {
            info!(count = pages.len(), "Custom error pages loaded");
        }

        Self {
            enabled: true,
            pages: Arc::new(pages),
        }
    }

    /// Check if custom error pages are enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get custom error page content for a status code
    pub fn get_page(&self, status_code: StatusCode) -> Option<&str> {
        if !self.enabled {
            return None;
        }

        self.pages.get(&status_code.as_u16()).map(|s| s.as_str())
    }

    /// Get custom error page content with variable substitution
    /// Supports placeholders: {{status_code}}, {{status_text}}, {{message}}
    pub fn render_page(&self, status_code: StatusCode, message: &str) -> Option<String> {
        let template = self.get_page(status_code)?;

        let status_text = status_code.canonical_reason().unwrap_or("Error");

        let rendered = template
            .replace("{{status_code}}", &status_code.as_u16().to_string())
            .replace("{{status_text}}", status_text)
            .replace("{{message}}", message);

        Some(rendered)
    }

    /// List all available custom error pages
    pub fn available_pages(&self) -> Vec<u16> {
        self.pages.keys().copied().collect()
    }
}

/// Load an error page from the filesystem
fn load_error_page(page_path: &str, base_dir: &str) -> Option<String> {
    // Try as absolute path first
    let path = Path::new(page_path);
    if path.is_absolute() && path.exists() {
        match std::fs::read_to_string(path) {
            Ok(content) => return Some(content),
            Err(e) => {
                warn!(path = ?path, error = %e, "Failed to load error page");
                return None;
            }
        }
    }

    // Try relative to base directory
    let relative_path = Path::new(base_dir).join(page_path);
    if relative_path.exists() {
        match std::fs::read_to_string(&relative_path) {
            Ok(content) => return Some(content),
            Err(e) => {
                warn!(path = ?relative_path, error = %e, "Failed to load error page");
                return None;
            }
        }
    }

    warn!(path = %page_path, "Error page file not found");
    None
}

/// Generate a default HTML error page (used when no custom page is available)
pub fn default_error_page(status_code: StatusCode, message: &str) -> String {
    let status_text = status_code.canonical_reason().unwrap_or("Error");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{} {}</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            color: #fff;
            min-height: 100vh;
            margin: 0;
            display: flex;
            align-items: center;
            justify-content: center;
        }}
        .container {{
            text-align: center;
            padding: 2rem;
        }}
        .status-code {{
            font-size: 8rem;
            font-weight: bold;
            margin: 0;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            background-clip: text;
        }}
        .status-text {{
            font-size: 1.5rem;
            margin: 1rem 0;
            color: #a0a0a0;
        }}
        .message {{
            font-size: 1rem;
            color: #707070;
            max-width: 500px;
            margin: 1rem auto;
        }}
        .powered-by {{
            margin-top: 3rem;
            font-size: 0.8rem;
            color: #505050;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1 class="status-code">{}</h1>
        <p class="status-text">{}</p>
        <p class="message">{}</p>
        <p class="powered-by">Powered by Screaming Eagle CDN</p>
    </div>
</body>
</html>"#,
        status_code.as_u16(),
        status_text,
        status_code.as_u16(),
        status_text,
        html_escape(message)
    )
}

/// Escape HTML special characters to prevent XSS
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_pages_disabled() {
        let config = ErrorPagesConfig::default();
        let pages = ErrorPages::new(&config);

        assert!(!pages.is_enabled());
        assert!(pages.get_page(StatusCode::NOT_FOUND).is_none());
    }

    #[test]
    fn test_default_error_page() {
        let html = default_error_page(StatusCode::NOT_FOUND, "Page not found");

        assert!(html.contains("404"));
        assert!(html.contains("Not Found"));
        assert!(html.contains("Page not found"));
        assert!(html.contains("Screaming Eagle"));
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("test & verify"), "test &amp; verify");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn test_render_page() {
        let mut config = ErrorPagesConfig::default();
        config.enabled = true;

        // Since we can't easily create files in tests, we'll test the disabled case
        let pages = ErrorPages::new(&config);

        // No pages loaded, should return None
        assert!(pages.render_page(StatusCode::NOT_FOUND, "test").is_none());
    }
}
