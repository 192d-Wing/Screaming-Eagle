//! HTTP/3 QUIC server implementation using quinn + h3.

use bytes::{Buf, Bytes};
use h3_quinn::quinn::{self, crypto::rustls::QuicServerConfig};
use http::{HeaderMap, Method, StatusCode, Uri};
use rustls::ServerConfig as RustlsServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::protocol::Http3Config;

/// Start the HTTP/3 server on UDP.
///
/// This runs alongside the main HTTP/1.1+2 server and handles QUIC connections.
/// The `handler` closure is called for each HTTP/3 request.
pub async fn run_http3_server<F, Fut>(
    bind_addr: SocketAddr,
    rustls_config: RustlsServerConfig,
    _config: Http3Config,
    mut shutdown: watch::Receiver<bool>,
    handler: F,
) -> anyhow::Result<()>
where
    F: Fn(Http3Request) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Http3Response> + Send,
{
    // Configure QUIC with ALPN for h3
    let mut rustls_cfg = rustls_config;
    rustls_cfg.alpn_protocols = vec![b"h3".to_vec()];

    let quic_config = QuicServerConfig::try_from(rustls_cfg)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));

    let endpoint = quinn::Endpoint::server(server_config, bind_addr)?;
    info!(addr = %bind_addr, "HTTP/3 server listening");

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(incoming, handler).await {
                        warn!(error = %e, "HTTP/3 connection error");
                    }
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("HTTP/3 server shutting down");
                    break;
                }
            }
        }
    }

    endpoint.close(0u32.into(), b"server shutdown");
    endpoint.wait_idle().await;
    Ok(())
}

async fn handle_connection<F, Fut>(
    incoming: quinn::Incoming,
    handler: F,
) -> anyhow::Result<()>
where
    F: Fn(Http3Request) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Http3Response> + Send,
{
    let connection = incoming.await?;
    debug!(
        peer = %connection.remote_address(),
        "HTTP/3 connection established"
    );

    let mut h3_conn: h3::server::Connection<h3_quinn::Connection, Bytes> =
        h3::server::Connection::new(h3_quinn::Connection::new(connection)).await?;

    loop {
        match h3_conn.accept().await {
            Ok(Some(resolver)) => {
                // Resolve request on current task to avoid Send issues
                let (req, mut stream) = match resolver.resolve_request().await {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "Failed to resolve HTTP/3 request");
                        continue;
                    }
                };

                // Read request body
                let mut body = Vec::new();
                while let Some(chunk) = stream.recv_data().await? {
                    let mut chunk = chunk;
                    while chunk.has_remaining() {
                        let bytes = chunk.chunk();
                        body.extend_from_slice(bytes);
                        chunk.advance(bytes.len());
                    }
                }

                let h3_req = Http3Request {
                    method: req.method().clone(),
                    uri: req.uri().clone(),
                    headers: req.headers().clone(),
                    body: Bytes::from(body),
                };

                // Handle request
                let response = handler(h3_req).await;

                // Send response
                let mut resp_builder = http::Response::builder().status(response.status);
                for (name, value) in &response.headers {
                    resp_builder = resp_builder.header(name, value);
                }
                let resp = resp_builder.body(()).unwrap();

                if let Err(e) = stream.send_response(resp).await {
                    warn!(error = %e, "Failed to send HTTP/3 response headers");
                    continue;
                }

                if !response.body.is_empty() {
                    if let Err(e) = stream.send_data(response.body).await {
                        warn!(error = %e, "Failed to send HTTP/3 response body");
                    }
                }

                if let Err(e) = stream.finish().await {
                    debug!(error = %e, "Failed to finish HTTP/3 stream");
                }
            }
            Ok(None) => {
                debug!("HTTP/3 connection closed gracefully");
                break;
            }
            Err(e) => {
                error!(error = %e, "HTTP/3 accept error");
                break;
            }
        }
    }

    Ok(())
}

/// An HTTP/3 request passed to handlers.
#[derive(Debug, Clone)]
pub struct Http3Request {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// An HTTP/3 response returned by handlers.
#[derive(Debug, Clone)]
pub struct Http3Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Default for Http3Response {
    fn default() -> Self {
        Self {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }
}

impl Http3Response {
    pub fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            headers: HeaderMap::new(),
            body: Bytes::from_static(b"Not Found"),
        }
    }

    pub fn internal_error() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HeaderMap::new(),
            body: Bytes::from_static(b"Internal Server Error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http3_response_defaults_to_200() {
        let r = Http3Response::default();
        assert_eq!(r.status, StatusCode::OK);
    }

    #[test]
    fn http3_response_not_found() {
        let r = Http3Response::not_found();
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
}
