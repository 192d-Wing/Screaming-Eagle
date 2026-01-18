use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

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

impl IntoResponse for CdnError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            CdnError::OriginError(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            CdnError::OriginUnreachable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
            CdnError::CacheError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            CdnError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            CdnError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            CdnError::ConfigError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            CdnError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

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
