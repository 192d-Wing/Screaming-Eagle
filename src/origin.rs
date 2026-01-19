use bytes::Bytes;
use reqwest::{header, Client, Response};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::config::{ConnectionPoolConfig, OriginConfig};
use crate::error::{CdnError, CdnResult};

#[derive(Debug, Clone)]
pub struct OriginResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Bytes,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub cache_control: Option<String>,
}

pub struct OriginFetcher {
    client: Client,
    origins: HashMap<String, OriginConfig>,
}

impl OriginFetcher {
    pub fn new(origins: HashMap<String, OriginConfig>) -> CdnResult<Self> {
        Self::with_pool_config(origins, ConnectionPoolConfig::default())
    }

    pub fn with_pool_config(
        origins: HashMap<String, OriginConfig>,
        pool_config: ConnectionPoolConfig,
    ) -> CdnResult<Self> {
        let mut builder = Client::builder()
            .gzip(true)
            .brotli(true)
            .pool_max_idle_per_host(pool_config.max_idle_per_host)
            .pool_idle_timeout(Duration::from_secs(pool_config.idle_timeout_secs))
            .connect_timeout(Duration::from_secs(pool_config.connect_timeout_secs))
            .tcp_nodelay(pool_config.tcp_nodelay);

        // Configure TCP keepalive
        if pool_config.tcp_keepalive {
            builder =
                builder.tcp_keepalive(Duration::from_secs(pool_config.tcp_keepalive_interval_secs));
        }

        // Configure HTTP/2
        if pool_config.http2_enabled {
            builder = builder
                .http2_prior_knowledge()
                .http2_initial_stream_window_size(pool_config.http2_initial_stream_window_size)
                .http2_initial_connection_window_size(
                    pool_config.http2_initial_connection_window_size,
                )
                .http2_adaptive_window(true);
        }

        let client = builder
            .build()
            .map_err(|e| CdnError::Internal(format!("Failed to create HTTP client: {}", e)))?;

        info!(
            max_idle = pool_config.max_idle_per_host,
            idle_timeout_secs = pool_config.idle_timeout_secs,
            connect_timeout_secs = pool_config.connect_timeout_secs,
            tcp_nodelay = pool_config.tcp_nodelay,
            tcp_keepalive = pool_config.tcp_keepalive,
            http2 = pool_config.http2_enabled,
            "Initialized HTTP client with connection pool"
        );

        Ok(Self { client, origins })
    }

    pub async fn fetch(
        &self,
        origin_name: &str,
        path: &str,
        query: Option<&str>,
        request_headers: &HashMap<String, String>,
    ) -> CdnResult<OriginResponse> {
        let origin = self
            .origins
            .get(origin_name)
            .ok_or_else(|| CdnError::ConfigError(format!("Unknown origin: {}", origin_name)))?;

        let url = self.build_url(&origin.url, path, query)?;

        info!(origin = %origin_name, url = %url, "Fetching from origin");

        let mut attempt = 0;
        let max_retries = origin.max_retries;

        loop {
            attempt += 1;

            match self.do_fetch(&url, origin, request_headers).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt >= max_retries {
                        error!(
                            origin = %origin_name,
                            attempt = attempt,
                            error = %e,
                            "All origin fetch attempts failed"
                        );
                        return Err(e);
                    }

                    warn!(
                        origin = %origin_name,
                        attempt = attempt,
                        max_retries = max_retries,
                        error = %e,
                        "Origin fetch failed, retrying"
                    );

                    // Exponential backoff
                    let delay = Duration::from_millis(100 * 2u64.pow(attempt - 1));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn do_fetch(
        &self,
        url: &str,
        origin: &OriginConfig,
        request_headers: &HashMap<String, String>,
    ) -> CdnResult<OriginResponse> {
        let mut request = self.client.get(url).timeout(origin.timeout());

        // Set Host header if configured
        if let Some(ref host) = origin.host_header {
            request = request.header(header::HOST, host);
        }

        // Forward configured headers
        for (key, value) in &origin.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        // Forward relevant request headers
        for (key, value) in request_headers {
            let key_lower = key.to_lowercase();
            // Only forward safe headers
            if matches!(
                key_lower.as_str(),
                "accept"
                    | "accept-encoding"
                    | "accept-language"
                    | "if-none-match"
                    | "if-modified-since"
            ) {
                request = request.header(key.as_str(), value.as_str());
            }
        }

        let response = request.send().await?;
        self.parse_response(response).await
    }

    async fn parse_response(&self, response: Response) -> CdnResult<OriginResponse> {
        let status_code = response.status().as_u16();
        let headers = self.extract_headers(&response);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let last_modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body = response.bytes().await?;

        debug!(
            status_code = status_code,
            body_size = body.len(),
            content_type = ?content_type,
            "Received origin response"
        );

        Ok(OriginResponse {
            status_code,
            headers,
            body,
            content_type,
            etag,
            last_modified,
            cache_control,
        })
    }

    fn extract_headers(&self, response: &Response) -> HashMap<String, String> {
        let mut headers = HashMap::new();

        // Headers to forward from origin
        let forward_headers = [
            header::CONTENT_TYPE,
            header::CONTENT_LANGUAGE,
            header::CONTENT_ENCODING,
            header::CACHE_CONTROL,
            header::ETAG,
            header::LAST_MODIFIED,
            header::VARY,
            header::CONTENT_DISPOSITION,
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            header::ACCESS_CONTROL_MAX_AGE,
        ];

        for header_name in forward_headers {
            if let Some(value) = response.headers().get(&header_name) {
                if let Ok(v) = value.to_str() {
                    headers.insert(header_name.to_string(), v.to_string());
                }
            }
        }

        headers
    }

    fn build_url(&self, base: &str, path: &str, query: Option<&str>) -> CdnResult<String> {
        let base = base.trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };

        let url = match query {
            Some(q) if !q.is_empty() => format!("{}{}?{}", base, path, q),
            _ => format!("{}{}", base, path),
        };

        Ok(url)
    }

    pub fn has_origin(&self, name: &str) -> bool {
        self.origins.contains_key(name)
    }

    pub fn origin_names(&self) -> Vec<&str> {
        self.origins.keys().map(|s| s.as_str()).collect()
    }
}

pub async fn conditional_fetch(
    fetcher: &OriginFetcher,
    origin_name: &str,
    path: &str,
    query: Option<&str>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> CdnResult<Option<OriginResponse>> {
    let mut headers = HashMap::new();

    if let Some(etag) = etag {
        headers.insert("If-None-Match".to_string(), etag.to_string());
    }

    if let Some(lm) = last_modified {
        headers.insert("If-Modified-Since".to_string(), lm.to_string());
    }

    let response = fetcher.fetch(origin_name, path, query, &headers).await?;

    if response.status_code == 304 {
        debug!(origin = %origin_name, path = %path, "Content not modified");
        Ok(None)
    } else {
        Ok(Some(response))
    }
}
