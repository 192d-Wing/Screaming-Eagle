use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::OnceLock;
use thiserror::Error;

use crate::error_pages::{ErrorPages, default_error_page};

/// Global error pages instance (set during initialization)
static ERROR_PAGES: OnceLock<ErrorPages> = OnceLock::new();

/// Initialize the global error pages instance
pub fn init_error_pages(pages: ErrorPages) {
    let _ = ERROR_PAGES.set(pages);
}

/// Get the global error pages instance
pub fn get_error_pages() -> Option<&'static ErrorPages> {
    ERROR_PAGES.get()
}

#[derive(Error, Debug)]
pub enum CdnError {
    #[error("Origin server error: {0}")]
    OriginError(String),

    #[error("Origin server unreachable: {0}")]
    OriginUnreachable(String),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Internal server error: {0}")]
    Internal(String),
}

impl CdnError {
    /// Get the HTTP status code for this error
    pub fn status_code(&self) -> StatusCode {
        match self {
            CdnError::OriginError(_) => StatusCode::BAD_GATEWAY,
            CdnError::OriginUnreachable(_) => StatusCode::SERVICE_UNAVAILABLE,
            CdnError::CacheError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            CdnError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            CdnError::NotFound(_) => StatusCode::NOT_FOUND,
            CdnError::ConfigError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            CdnError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Get the error message
    pub fn message(&self) -> &str {
        match self {
            CdnError::OriginError(msg) => msg,
            CdnError::OriginUnreachable(msg) => msg,
            CdnError::CacheError(msg) => msg,
            CdnError::InvalidRequest(msg) => msg,
            CdnError::NotFound(msg) => msg,
            CdnError::ConfigError(msg) => msg,
            CdnError::Internal(msg) => msg,
        }
    }
}

impl IntoResponse for CdnError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let message = self.message().to_string();

        // Check if we have custom error pages enabled
        if let Some(error_pages) = get_error_pages()
            && error_pages.is_enabled() {
                // Try to render custom error page
                if let Some(html) = error_pages.render_page(status, &message) {
                    return (
                        status,
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        html,
                    )
                        .into_response();
                }

                // Fall back to default styled error page
                let html = default_error_page(status, &message);
                return (
                    status,
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    html,
                )
                    .into_response();
            }

        // Default JSON error response
        let body = Json(json!({
            "error": message,
            "status": status.as_u16()
        }));

        (status, body).into_response()
    }
}

impl From<reqwest::Error> for CdnError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_connect() {
            CdnError::OriginUnreachable(err.to_string())
        } else if err.is_timeout() {
            CdnError::OriginUnreachable(format!("Origin timeout: {}", err))
        } else {
            CdnError::OriginError(err.to_string())
        }
    }
}

pub type CdnResult<T> = Result<T, CdnError>;
