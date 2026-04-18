//! Protocol enhancements: WebSocket proxying, SSE streaming, HTTP/3 config.

use axum::{
    body::Body,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream as ClientWsStream, connect_async,
    tungstenite::{Message as TMessage, client::IntoClientRequest},
};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Protocol configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolConfig {
    #[serde(default)]
    pub websocket: WebSocketConfig,

    #[serde(default)]
    pub sse: SseConfig,

    #[serde(default)]
    pub http3: Http3Config,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            websocket: WebSocketConfig::default(),
            sse: SseConfig::default(),
            http3: Http3Config::default(),
        }
    }
}

/// WebSocket proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Maximum in-flight frames before backpressure applies.
    #[serde(default = "default_ws_buffer")]
    pub buffer: usize,

    /// Maximum message size in bytes.
    #[serde(default = "default_ws_max_message")]
    pub max_message_size: usize,

    /// Upstream URL for proxied WebSockets. `{origin}` and `{path}` are
    /// substituted from the request.
    #[serde(default)]
    pub upstream_template: Option<String>,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            buffer: default_ws_buffer(),
            max_message_size: default_ws_max_message(),
            upstream_template: None,
        }
    }
}

fn default_ws_buffer() -> usize {
    64
}

fn default_ws_max_message() -> usize {
    16 * 1024 * 1024
}

/// Server-Sent Events configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Idle keep-alive interval in seconds for outgoing SSE streams.
    #[serde(default = "default_sse_keepalive")]
    pub keepalive_secs: u64,

    /// Disable response compression for SSE (needed for correct streaming).
    #[serde(default = "default_true")]
    pub disable_compression: bool,

    /// Bypass cache for SSE responses.
    #[serde(default = "default_true")]
    pub bypass_cache: bool,
}

impl Default for SseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keepalive_secs: default_sse_keepalive(),
            disable_compression: true,
            bypass_cache: true,
        }
    }
}

fn default_sse_keepalive() -> u64 {
    15
}

fn default_true() -> bool {
    true
}

/// HTTP/3 QUIC configuration. The transport itself is not yet wired in —
/// this configures what the server advertises.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Http3Config {
    #[serde(default)]
    pub enabled: bool,

    /// Port to bind for HTTP/3 (UDP). Typically matches the HTTPS port.
    #[serde(default = "default_http3_port")]
    pub port: u16,

    /// Advertise Alt-Svc header so clients can upgrade to HTTP/3.
    #[serde(default = "default_true")]
    pub advertise_alt_svc: bool,

    /// Alt-Svc max age in seconds.
    #[serde(default = "default_alt_svc_max_age")]
    pub alt_svc_max_age: u64,
}

impl Default for Http3Config {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_http3_port(),
            advertise_alt_svc: true,
            alt_svc_max_age: default_alt_svc_max_age(),
        }
    }
}

fn default_http3_port() -> u16 {
    443
}

fn default_alt_svc_max_age() -> u64 {
    86400
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Returns true if the request is an SSE request (Accept: text/event-stream).
pub fn is_sse_request(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| {
            a.split(',')
                .any(|t| t.trim().starts_with("text/event-stream"))
        })
        .unwrap_or(false)
}

/// Returns true if the response body is an SSE stream.
pub fn is_sse_response(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.trim_start().starts_with("text/event-stream"))
        .unwrap_or(false)
}

/// Returns true if the request is attempting a WebSocket upgrade.
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let upgrade_ok = headers
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    let connection_ok = headers
        .get(axum::http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);

    upgrade_ok && connection_ok
}

// ---------------------------------------------------------------------------
// Alt-Svc advertising middleware (HTTP/3 hint)
// ---------------------------------------------------------------------------

/// Middleware that sets Alt-Svc to advertise HTTP/3 when configured.
pub async fn alt_svc_middleware(
    State(cfg): State<Arc<Http3Config>>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    if cfg.enabled && cfg.advertise_alt_svc {
        let value = format!("h3=\":{}\"; ma={}", cfg.port, cfg.alt_svc_max_age);
        if let Ok(v) = axum::http::HeaderValue::from_str(&value) {
            response.headers_mut().insert("alt-svc", v);
        }
    }
    response
}

// ---------------------------------------------------------------------------
// SSE pass-through middleware
// ---------------------------------------------------------------------------

/// Middleware that adapts headers on SSE responses:
///   - disables buffering via `X-Accel-Buffering: no`,
///   - disables compression hint added by content processor,
///   - sets `Cache-Control: no-cache` when bypass_cache is configured.
pub async fn sse_middleware(
    State(cfg): State<Arc<SseConfig>>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    if !cfg.enabled {
        return next.run(request).await;
    }

    let client_wants_sse = is_sse_request(request.headers());
    let mut response = next.run(request).await;

    let response_is_sse = is_sse_response(response.headers());
    if !client_wants_sse && !response_is_sse {
        return response;
    }

    let headers = response.headers_mut();
    headers.insert("x-accel-buffering", "no".parse().unwrap());

    if cfg.disable_compression {
        headers.remove("x-compression-hint");
        headers.remove(axum::http::header::CONTENT_ENCODING);
    }

    if cfg.bypass_cache {
        headers.insert(axum::http::header::CACHE_CONTROL, "no-cache".parse().unwrap());
        headers.insert("x-cdn-sse", "pass-through".parse().unwrap());
    }

    response
}

// ---------------------------------------------------------------------------
// WebSocket proxy
// ---------------------------------------------------------------------------

/// Axum handler that upgrades a client connection and proxies to an upstream.
///
/// The upstream URL is built by substituting `{origin}` and `{path}` in the
/// configured template. If the template is empty, returns 501.
pub async fn websocket_proxy_handler(
    State(cfg): State<Arc<WebSocketConfig>>,
    ws: WebSocketUpgrade,
    axum::extract::Path((origin, path)): axum::extract::Path<(String, String)>,
) -> Response {
    if !cfg.enabled {
        return (StatusCode::NOT_FOUND, "WebSocket proxy disabled").into_response();
    }

    let template = match cfg.upstream_template.as_ref() {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                "No WebSocket upstream configured",
            )
                .into_response();
        }
    };

    let target = template.replace("{origin}", &origin).replace("{path}", &path);
    let max_message = cfg.max_message_size;
    let buffer = cfg.buffer;

    ws.max_message_size(max_message)
        .max_frame_size(max_message)
        .on_upgrade(move |socket| async move {
            if let Err(e) = proxy_websocket(socket, target, buffer).await {
                warn!(error = %e, "WebSocket proxy ended with error");
            }
        })
}

async fn proxy_websocket(
    client: WebSocket,
    upstream_url: String,
    buffer: usize,
) -> anyhow::Result<()> {
    info!(upstream = %upstream_url, "Opening WebSocket proxy");
    let request = upstream_url.into_client_request()?;
    let (upstream, _response) = connect_async(request).await?;
    relay_websocket(client, upstream, buffer).await
}

async fn relay_websocket(
    client: WebSocket,
    upstream: ClientWsStream<MaybeTlsStream<TcpStream>>,
    _buffer: usize,
) -> anyhow::Result<()> {
    let (mut c_tx, mut c_rx) = client.split();
    let (mut u_tx, mut u_rx) = upstream.split();

    let client_to_upstream = async {
        while let Some(msg) = c_rx.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    debug!(error = %e, "client recv error");
                    break;
                }
            };
            let out = match msg {
                Message::Text(s) => TMessage::Text(s.as_str().to_string().into()),
                Message::Binary(b) => TMessage::Binary(b.to_vec()),
                Message::Ping(b) => TMessage::Ping(b.to_vec()),
                Message::Pong(b) => TMessage::Pong(b.to_vec()),
                Message::Close(frame) => {
                    let f = frame.map(|f| {
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: f.code.into(),
                            reason: f.reason.to_string().into(),
                        }
                    });
                    let _ = u_tx.send(TMessage::Close(f)).await;
                    break;
                }
            };
            if u_tx.send(out).await.is_err() {
                break;
            }
        }
    };

    let upstream_to_client = async {
        while let Some(msg) = u_rx.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    debug!(error = %e, "upstream recv error");
                    break;
                }
            };
            let out = match msg {
                TMessage::Text(s) => Message::Text(s.to_string().into()),
                TMessage::Binary(b) => Message::Binary(b.into()),
                TMessage::Ping(b) => Message::Ping(b.into()),
                TMessage::Pong(b) => Message::Pong(b.into()),
                TMessage::Close(frame) => {
                    let f = frame.map(|f| axum::extract::ws::CloseFrame {
                        code: u16::from(f.code),
                        reason: f.reason.to_string().into(),
                    });
                    let _ = c_tx.send(Message::Close(f)).await;
                    break;
                }
                TMessage::Frame(_) => continue,
            };
            if c_tx.send(out).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {}
        _ = upstream_to_client => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn detects_sse_request() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        assert!(is_sse_request(&h));
    }

    #[test]
    fn detects_sse_response() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        assert!(is_sse_response(&h));
    }

    #[test]
    fn detects_websocket_upgrade() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::UPGRADE,
            HeaderValue::from_static("websocket"),
        );
        h.insert(
            axum::http::header::CONNECTION,
            HeaderValue::from_static("Upgrade"),
        );
        assert!(is_websocket_upgrade(&h));
    }

    #[test]
    fn non_sse_request_has_no_event_stream_accept() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::ACCEPT,
            HeaderValue::from_static("text/html"),
        );
        assert!(!is_sse_request(&h));
    }

    #[test]
    fn default_http3_config_is_disabled() {
        let c = Http3Config::default();
        assert!(!c.enabled);
        assert_eq!(c.port, 443);
    }
}
