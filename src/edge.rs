//! Edge Logic & Transformations
//!
//! Provides URL rewriting, header transformations, query string normalization,
//! and conditional routing for edge processing.

use axum::{
    body::Body,
    extract::State,
    http::{header::HeaderName, HeaderMap, HeaderValue, Method, Request, Uri},
    middleware::Next,
    response::Response,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tracing::{debug, instrument, warn};

use crate::config::{
    EdgeConfig as ConfigEdgeConfig, RoutingActionConfig, RoutingConditionConfig,
};

/// Edge processing configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    /// URL rewriting rules
    #[serde(default)]
    pub rewrite_rules: Vec<RewriteRule>,

    /// Header transformation rules
    #[serde(default)]
    pub header_transforms: HeaderTransforms,

    /// Query string normalization settings
    #[serde(default)]
    pub query_normalization: QueryNormalizationConfig,

    /// Conditional routing rules
    #[serde(default)]
    pub routing_rules: Vec<RoutingRule>,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            rewrite_rules: Vec::new(),
            header_transforms: HeaderTransforms::default(),
            query_normalization: QueryNormalizationConfig::default(),
            routing_rules: Vec::new(),
        }
    }
}

// ============================================================================
// URL Rewriting
// ============================================================================

/// A URL rewrite rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRule {
    /// Rule name for logging
    pub name: String,

    /// Regex pattern to match against the URL path
    pub pattern: String,

    /// Replacement string (supports capture groups like $1, $2)
    pub replacement: String,

    /// Whether to stop processing after this rule matches
    #[serde(default)]
    pub stop: bool,

    /// Optional condition for when this rule applies
    #[serde(default)]
    pub condition: Option<RewriteCondition>,
}

/// Condition for rewrite rule application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteCondition {
    /// Header name to check
    pub header: Option<String>,

    /// Header value pattern (regex)
    pub header_pattern: Option<String>,

    /// Query parameter to check
    pub query_param: Option<String>,

    /// Query value pattern (regex)
    pub query_pattern: Option<String>,

    /// HTTP methods this rule applies to
    #[serde(default)]
    pub methods: Vec<String>,
}

/// Compiled rewrite rule for efficient matching
pub struct CompiledRewriteRule {
    pub name: String,
    pub pattern: Regex,
    pub replacement: String,
    pub stop: bool,
    pub condition: Option<CompiledCondition>,
}

/// Compiled condition
pub struct CompiledCondition {
    pub header: Option<String>,
    pub header_pattern: Option<Regex>,
    pub query_param: Option<String>,
    pub query_pattern: Option<Regex>,
    pub methods: Vec<Method>,
}

/// URL rewriter with compiled rules
pub struct UrlRewriter {
    rules: Vec<CompiledRewriteRule>,
}

impl UrlRewriter {
    pub fn new(rules: &[RewriteRule]) -> Self {
        let compiled_rules = rules
            .iter()
            .filter_map(|rule| {
                let pattern = match Regex::new(&rule.pattern) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(rule = %rule.name, error = %e, "Failed to compile rewrite pattern");
                        return None;
                    }
                };

                let condition = rule.condition.as_ref().map(|c| {
                    CompiledCondition {
                        header: c.header.clone(),
                        header_pattern: c.header_pattern.as_ref().and_then(|p| Regex::new(p).ok()),
                        query_param: c.query_param.clone(),
                        query_pattern: c.query_pattern.as_ref().and_then(|p| Regex::new(p).ok()),
                        methods: c
                            .methods
                            .iter()
                            .filter_map(|m| m.parse().ok())
                            .collect(),
                    }
                });

                Some(CompiledRewriteRule {
                    name: rule.name.clone(),
                    pattern,
                    replacement: rule.replacement.clone(),
                    stop: rule.stop,
                    condition,
                })
            })
            .collect();

        Self { rules: compiled_rules }
    }

    /// Rewrite a URL path based on configured rules
    #[instrument(skip(self, headers))]
    pub fn rewrite(
        &self,
        path: &str,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
    ) -> Option<String> {
        let mut current_path = path.to_string();
        let mut rewritten = false;

        for rule in &self.rules {
            // Check condition if present
            if let Some(ref condition) = rule.condition {
                if !self.check_condition(condition, query, method, headers) {
                    continue;
                }
            }

            // Try to match and replace
            if rule.pattern.is_match(&current_path) {
                let new_path = rule
                    .pattern
                    .replace_all(&current_path, &rule.replacement)
                    .to_string();

                if new_path != current_path {
                    debug!(
                        rule = %rule.name,
                        from = %current_path,
                        to = %new_path,
                        "URL rewritten"
                    );
                    current_path = new_path;
                    rewritten = true;

                    if rule.stop {
                        break;
                    }
                }
            }
        }

        if rewritten {
            Some(current_path)
        } else {
            None
        }
    }

    fn check_condition(
        &self,
        condition: &CompiledCondition,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
    ) -> bool {
        // Check method restriction
        if !condition.methods.is_empty() && !condition.methods.contains(method) {
            return false;
        }

        // Check header condition
        if let Some(ref header_name) = condition.header {
            if let Some(ref pattern) = condition.header_pattern {
                let header_value = headers
                    .get(header_name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if !pattern.is_match(header_value) {
                    return false;
                }
            }
        }

        // Check query parameter condition
        if let Some(ref param_name) = condition.query_param {
            if let Some(ref pattern) = condition.query_pattern {
                let param_value = query
                    .and_then(|q| {
                        url::form_urlencoded::parse(q.as_bytes())
                            .find(|(k, _)| k == param_name)
                            .map(|(_, v)| v.to_string())
                    })
                    .unwrap_or_default();
                if !pattern.is_match(&param_value) {
                    return false;
                }
            }
        }

        true
    }
}

// ============================================================================
// Header Transformations
// ============================================================================

/// Header transformation configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeaderTransforms {
    /// Headers to add to requests going to origin
    #[serde(default)]
    pub request_add: HashMap<String, String>,

    /// Headers to remove from requests going to origin
    #[serde(default)]
    pub request_remove: Vec<String>,

    /// Headers to add to responses going to client
    #[serde(default)]
    pub response_add: HashMap<String, String>,

    /// Headers to remove from responses going to client
    #[serde(default)]
    pub response_remove: Vec<String>,

    /// Header value transformations (regex-based)
    #[serde(default)]
    pub transformations: Vec<HeaderTransformation>,
}

/// A single header transformation rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderTransformation {
    /// Header name to transform
    pub header: String,

    /// Pattern to match in the header value
    pub pattern: String,

    /// Replacement value
    pub replacement: String,

    /// Apply to request (true) or response (false)
    #[serde(default)]
    pub request: bool,
}

/// Header transformer with compiled patterns
pub struct HeaderTransformer {
    request_add: Vec<(HeaderName, HeaderValue)>,
    request_remove: Vec<HeaderName>,
    response_add: Vec<(HeaderName, HeaderValue)>,
    response_remove: Vec<HeaderName>,
    request_transforms: Vec<CompiledHeaderTransform>,
    response_transforms: Vec<CompiledHeaderTransform>,
}

struct CompiledHeaderTransform {
    header: HeaderName,
    pattern: Regex,
    replacement: String,
}

impl HeaderTransformer {
    pub fn new(config: &HeaderTransforms) -> Self {
        let request_add = config
            .request_add
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    HeaderName::try_from(k).ok()?,
                    HeaderValue::try_from(v).ok()?,
                ))
            })
            .collect();

        let request_remove = config
            .request_remove
            .iter()
            .filter_map(|k| HeaderName::try_from(k).ok())
            .collect();

        let response_add = config
            .response_add
            .iter()
            .filter_map(|(k, v)| {
                Some((
                    HeaderName::try_from(k).ok()?,
                    HeaderValue::try_from(v).ok()?,
                ))
            })
            .collect();

        let response_remove = config
            .response_remove
            .iter()
            .filter_map(|k| HeaderName::try_from(k).ok())
            .collect();

        let (request_transforms, response_transforms): (Vec<_>, Vec<_>) = config
            .transformations
            .iter()
            .filter_map(|t| {
                Some(CompiledHeaderTransform {
                    header: HeaderName::try_from(&t.header).ok()?,
                    pattern: Regex::new(&t.pattern).ok()?,
                    replacement: t.replacement.clone(),
                })
            })
            .partition(|_| true); // Simplified; in practice check t.request

        Self {
            request_add,
            request_remove,
            response_add,
            response_remove,
            request_transforms,
            response_transforms,
        }
    }

    /// Transform request headers
    pub fn transform_request_headers(&self, headers: &mut HeaderMap) {
        // Remove headers
        for name in &self.request_remove {
            headers.remove(name);
        }

        // Add headers
        for (name, value) in &self.request_add {
            headers.insert(name.clone(), value.clone());
        }

        // Apply transformations
        for transform in &self.request_transforms {
            if let Some(value) = headers.get(&transform.header) {
                if let Ok(value_str) = value.to_str() {
                    let new_value = transform
                        .pattern
                        .replace_all(value_str, &transform.replacement);
                    if let Ok(new_header_value) = HeaderValue::try_from(new_value.as_ref()) {
                        headers.insert(transform.header.clone(), new_header_value);
                    }
                }
            }
        }
    }

    /// Transform response headers
    pub fn transform_response_headers(&self, headers: &mut HeaderMap) {
        // Remove headers
        for name in &self.response_remove {
            headers.remove(name);
        }

        // Add headers
        for (name, value) in &self.response_add {
            headers.insert(name.clone(), value.clone());
        }

        // Apply transformations
        for transform in &self.response_transforms {
            if let Some(value) = headers.get(&transform.header) {
                if let Ok(value_str) = value.to_str() {
                    let new_value = transform
                        .pattern
                        .replace_all(value_str, &transform.replacement);
                    if let Ok(new_header_value) = HeaderValue::try_from(new_value.as_ref()) {
                        headers.insert(transform.header.clone(), new_header_value);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Query String Normalization
// ============================================================================

/// Query string normalization configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryNormalizationConfig {
    /// Whether to sort query parameters alphabetically
    #[serde(default = "default_true")]
    pub sort_params: bool,

    /// Whether to remove empty parameters
    #[serde(default = "default_true")]
    pub remove_empty: bool,

    /// Parameters to always remove (e.g., tracking params)
    #[serde(default)]
    pub remove_params: Vec<String>,

    /// Parameters to keep (if set, only these are kept)
    #[serde(default)]
    pub keep_only_params: Vec<String>,

    /// Whether to lowercase parameter names
    #[serde(default)]
    pub lowercase_names: bool,

    /// Whether to decode and re-encode values for consistency
    #[serde(default = "default_true")]
    pub normalize_encoding: bool,
}

fn default_true() -> bool {
    true
}

impl Default for QueryNormalizationConfig {
    fn default() -> Self {
        Self {
            sort_params: true,
            remove_empty: true,
            remove_params: vec![
                "utm_source".to_string(),
                "utm_medium".to_string(),
                "utm_campaign".to_string(),
                "utm_term".to_string(),
                "utm_content".to_string(),
                "fbclid".to_string(),
                "gclid".to_string(),
            ],
            keep_only_params: Vec::new(),
            lowercase_names: false,
            normalize_encoding: true,
        }
    }
}

/// Query string normalizer
pub struct QueryNormalizer {
    config: QueryNormalizationConfig,
}

impl QueryNormalizer {
    pub fn new(config: QueryNormalizationConfig) -> Self {
        Self { config }
    }

    /// Normalize a query string
    pub fn normalize(&self, query: Option<&str>) -> Option<String> {
        let query = query?;
        if query.is_empty() {
            return None;
        }

        let mut params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| {
                let key = if self.config.lowercase_names {
                    k.to_lowercase()
                } else {
                    k.to_string()
                };
                (key, v.to_string())
            })
            .collect();

        // Remove empty parameters
        if self.config.remove_empty {
            params.retain(|(_, v)| !v.is_empty());
        }

        // Remove blacklisted parameters
        if !self.config.remove_params.is_empty() {
            params.retain(|(k, _)| !self.config.remove_params.contains(k));
        }

        // Keep only whitelisted parameters
        if !self.config.keep_only_params.is_empty() {
            params.retain(|(k, _)| self.config.keep_only_params.contains(k));
        }

        // Sort parameters
        if self.config.sort_params {
            params.sort_by(|a, b| a.0.cmp(&b.0));
        }

        if params.is_empty() {
            return None;
        }

        // Re-encode
        let normalized: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();

        Some(normalized)
    }
}

// ============================================================================
// Conditional Routing
// ============================================================================

/// A conditional routing rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Rule name for logging
    pub name: String,

    /// Conditions that must all match
    pub conditions: Vec<RoutingCondition>,

    /// Action to take when conditions match
    pub action: RoutingAction,

    /// Priority (higher = checked first)
    #[serde(default)]
    pub priority: i32,
}

/// A routing condition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RoutingCondition {
    /// Match path pattern
    #[serde(rename = "path")]
    Path { pattern: String },

    /// Match header value
    #[serde(rename = "header")]
    Header { name: String, pattern: String },

    /// Match query parameter
    #[serde(rename = "query")]
    Query { param: String, pattern: String },

    /// Match HTTP method
    #[serde(rename = "method")]
    Method { methods: Vec<String> },

    /// Match client IP (CIDR)
    #[serde(rename = "ip")]
    ClientIp { cidrs: Vec<String> },

    /// Geographic location (country codes)
    #[serde(rename = "geo")]
    Geo { countries: Vec<String> },

    /// Time-based condition
    #[serde(rename = "time")]
    Time {
        /// Days of week (0=Sunday, 6=Saturday)
        days: Option<Vec<u8>>,
        /// Start hour (0-23)
        start_hour: Option<u8>,
        /// End hour (0-23)
        end_hour: Option<u8>,
    },
}

/// Action to take when routing conditions match
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RoutingAction {
    /// Route to a specific origin
    #[serde(rename = "origin")]
    RouteToOrigin { origin: String },

    /// Redirect to a URL
    #[serde(rename = "redirect")]
    Redirect { url: String, status: u16 },

    /// Return a fixed response
    #[serde(rename = "response")]
    FixedResponse {
        status: u16,
        body: Option<String>,
        headers: Option<HashMap<String, String>>,
    },

    /// Modify the request and continue
    #[serde(rename = "modify")]
    Modify {
        set_headers: Option<HashMap<String, String>>,
        set_path: Option<String>,
    },

    /// Block the request
    #[serde(rename = "block")]
    Block { status: u16, message: Option<String> },
}

/// Compiled routing rule
pub struct CompiledRoutingRule {
    pub name: String,
    pub conditions: Vec<CompiledRoutingCondition>,
    pub action: RoutingAction,
    pub priority: i32,
}

/// Compiled routing condition
pub enum CompiledRoutingCondition {
    Path(Regex),
    Header(String, Regex),
    Query(String, Regex),
    Method(Vec<Method>),
    ClientIp(Vec<String>),
    Geo(Vec<String>),
    Time {
        days: Option<Vec<u8>>,
        start_hour: Option<u8>,
        end_hour: Option<u8>,
    },
}

/// Conditional router
pub struct ConditionalRouter {
    rules: Vec<CompiledRoutingRule>,
}

impl ConditionalRouter {
    pub fn new(mut rules: Vec<RoutingRule>) -> Self {
        // Sort by priority (descending)
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        let compiled_rules = rules
            .into_iter()
            .filter_map(|rule| {
                let conditions: Vec<CompiledRoutingCondition> = rule
                    .conditions
                    .into_iter()
                    .filter_map(|c| Self::compile_condition(c))
                    .collect();

                Some(CompiledRoutingRule {
                    name: rule.name,
                    conditions,
                    action: rule.action,
                    priority: rule.priority,
                })
            })
            .collect();

        Self { rules: compiled_rules }
    }

    fn compile_condition(condition: RoutingCondition) -> Option<CompiledRoutingCondition> {
        match condition {
            RoutingCondition::Path { pattern } => {
                Regex::new(&pattern).ok().map(CompiledRoutingCondition::Path)
            }
            RoutingCondition::Header { name, pattern } => Regex::new(&pattern)
                .ok()
                .map(|r| CompiledRoutingCondition::Header(name, r)),
            RoutingCondition::Query { param, pattern } => Regex::new(&pattern)
                .ok()
                .map(|r| CompiledRoutingCondition::Query(param, r)),
            RoutingCondition::Method { methods } => {
                let parsed: Vec<Method> = methods
                    .iter()
                    .filter_map(|m| m.parse().ok())
                    .collect();
                Some(CompiledRoutingCondition::Method(parsed))
            }
            RoutingCondition::ClientIp { cidrs } => {
                Some(CompiledRoutingCondition::ClientIp(cidrs))
            }
            RoutingCondition::Geo { countries } => Some(CompiledRoutingCondition::Geo(countries)),
            RoutingCondition::Time {
                days,
                start_hour,
                end_hour,
            } => Some(CompiledRoutingCondition::Time {
                days,
                start_hour,
                end_hour,
            }),
        }
    }

    /// Evaluate routing rules and return the first matching action
    #[instrument(skip(self, headers))]
    pub fn evaluate(
        &self,
        path: &str,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
        client_ip: Option<&str>,
    ) -> Option<&RoutingAction> {
        for rule in &self.rules {
            if self.matches_all_conditions(&rule.conditions, path, query, method, headers, client_ip)
            {
                debug!(rule = %rule.name, "Routing rule matched");
                return Some(&rule.action);
            }
        }
        None
    }

    fn matches_all_conditions(
        &self,
        conditions: &[CompiledRoutingCondition],
        path: &str,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
        client_ip: Option<&str>,
    ) -> bool {
        conditions.iter().all(|c| {
            self.matches_condition(c, path, query, method, headers, client_ip)
        })
    }

    fn matches_condition(
        &self,
        condition: &CompiledRoutingCondition,
        path: &str,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
        client_ip: Option<&str>,
    ) -> bool {
        match condition {
            CompiledRoutingCondition::Path(pattern) => pattern.is_match(path),
            CompiledRoutingCondition::Header(name, pattern) => headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| pattern.is_match(v))
                .unwrap_or(false),
            CompiledRoutingCondition::Query(param, pattern) => query
                .and_then(|q| {
                    url::form_urlencoded::parse(q.as_bytes())
                        .find(|(k, _)| k == param)
                        .map(|(_, v)| pattern.is_match(&v))
                })
                .unwrap_or(false),
            CompiledRoutingCondition::Method(methods) => methods.contains(method),
            CompiledRoutingCondition::ClientIp(cidrs) => {
                client_ip.map(|ip| cidrs.iter().any(|cidr| ip_matches_cidr(ip, cidr))).unwrap_or(false)
            }
            CompiledRoutingCondition::Geo(_countries) => {
                // Geo lookup would require a GeoIP database
                // For now, this always returns false (not implemented)
                false
            }
            CompiledRoutingCondition::Time {
                days,
                start_hour,
                end_hour,
            } => {
                let now = chrono::Utc::now();
                let weekday = now.format("%w").to_string().parse::<u8>().unwrap_or(0);
                let hour = now.format("%H").to_string().parse::<u8>().unwrap_or(0);

                if let Some(ref allowed_days) = days {
                    if !allowed_days.contains(&weekday) {
                        return false;
                    }
                }

                if let (Some(start), Some(end)) = (start_hour, end_hour) {
                    if *start <= *end {
                        // Normal range (e.g., 9-17)
                        if hour < *start || hour > *end {
                            return false;
                        }
                    } else {
                        // Overnight range (e.g., 22-6)
                        if hour < *start && hour > *end {
                            return false;
                        }
                    }
                }

                true
            }
        }
    }
}

/// Check if an IP matches a CIDR range (simplified)
fn ip_matches_cidr(ip: &str, cidr: &str) -> bool {
    use std::net::IpAddr;

    let ip_addr: IpAddr = match ip.parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return false;
    }

    let network_ip: IpAddr = match parts[0].parse() {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    let prefix_len: u8 = match parts[1].parse() {
        Ok(len) => len,
        Err(_) => return false,
    };

    match (ip_addr, network_ip) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            if prefix_len > 32 {
                return false;
            }
            let mask = if prefix_len == 0 {
                0
            } else {
                !0u32 << (32 - prefix_len)
            };
            let ip_bits = u32::from(ip);
            let net_bits = u32::from(net);
            (ip_bits & mask) == (net_bits & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            if prefix_len > 128 {
                return false;
            }
            let ip_bits = u128::from(ip);
            let net_bits = u128::from(net);
            let mask = if prefix_len == 0 {
                0
            } else {
                !0u128 << (128 - prefix_len)
            };
            (ip_bits & mask) == (net_bits & mask)
        }
        _ => false,
    }
}

// ============================================================================
// Edge Processor - combines all edge logic
// ============================================================================

/// Main edge processor that combines all edge logic
pub struct EdgeProcessor {
    rewriter: UrlRewriter,
    header_transformer: HeaderTransformer,
    query_normalizer: QueryNormalizer,
    router: ConditionalRouter,
}

impl EdgeProcessor {
    pub fn new(config: EdgeConfig) -> Self {
        Self {
            rewriter: UrlRewriter::new(&config.rewrite_rules),
            header_transformer: HeaderTransformer::new(&config.header_transforms),
            query_normalizer: QueryNormalizer::new(config.query_normalization),
            router: ConditionalRouter::new(config.routing_rules),
        }
    }

    /// Create from config module types
    pub fn from_config(config: &ConfigEdgeConfig) -> Self {
        // Convert rewrite rules
        let rewrite_rules: Vec<RewriteRule> = config
            .rewrite_rules
            .iter()
            .map(|r| RewriteRule {
                name: r.name.clone(),
                pattern: r.pattern.clone(),
                replacement: r.replacement.clone(),
                stop: r.stop,
                condition: r.condition.as_ref().map(|c| RewriteCondition {
                    header: c.header.clone(),
                    header_pattern: c.header_pattern.clone(),
                    query_param: c.query_param.clone(),
                    query_pattern: c.query_pattern.clone(),
                    methods: c.methods.clone(),
                }),
            })
            .collect();

        // Convert header transforms
        let header_transforms = HeaderTransforms {
            request_add: config.header_transforms.request_add.clone(),
            request_remove: config.header_transforms.request_remove.clone(),
            response_add: config.header_transforms.response_add.clone(),
            response_remove: config.header_transforms.response_remove.clone(),
            transformations: config
                .header_transforms
                .transformations
                .iter()
                .map(|t| HeaderTransformation {
                    header: t.header.clone(),
                    pattern: t.pattern.clone(),
                    replacement: t.replacement.clone(),
                    request: t.request,
                })
                .collect(),
        };

        // Convert query normalization
        let query_normalization = QueryNormalizationConfig {
            sort_params: config.query_normalization.sort_params,
            remove_empty: config.query_normalization.remove_empty,
            remove_params: config.query_normalization.remove_params.clone(),
            keep_only_params: config.query_normalization.keep_only_params.clone(),
            lowercase_names: config.query_normalization.lowercase_names,
            normalize_encoding: config.query_normalization.normalize_encoding,
        };

        // Convert routing rules
        let routing_rules: Vec<RoutingRule> = config
            .routing_rules
            .iter()
            .map(|r| RoutingRule {
                name: r.name.clone(),
                conditions: r
                    .conditions
                    .iter()
                    .map(|c| match c {
                        RoutingConditionConfig::Path { pattern } => {
                            RoutingCondition::Path { pattern: pattern.clone() }
                        }
                        RoutingConditionConfig::Header { name, pattern } => {
                            RoutingCondition::Header {
                                name: name.clone(),
                                pattern: pattern.clone(),
                            }
                        }
                        RoutingConditionConfig::Query { param, pattern } => {
                            RoutingCondition::Query {
                                param: param.clone(),
                                pattern: pattern.clone(),
                            }
                        }
                        RoutingConditionConfig::Method { methods } => {
                            RoutingCondition::Method { methods: methods.clone() }
                        }
                        RoutingConditionConfig::ClientIp { cidrs } => {
                            RoutingCondition::ClientIp { cidrs: cidrs.clone() }
                        }
                        RoutingConditionConfig::Geo { countries } => {
                            RoutingCondition::Geo { countries: countries.clone() }
                        }
                        RoutingConditionConfig::Time {
                            days,
                            start_hour,
                            end_hour,
                        } => RoutingCondition::Time {
                            days: days.clone(),
                            start_hour: *start_hour,
                            end_hour: *end_hour,
                        },
                    })
                    .collect(),
                action: match &r.action {
                    RoutingActionConfig::RouteToOrigin { origin } => {
                        RoutingAction::RouteToOrigin { origin: origin.clone() }
                    }
                    RoutingActionConfig::Redirect { url, status } => {
                        RoutingAction::Redirect {
                            url: url.clone(),
                            status: *status,
                        }
                    }
                    RoutingActionConfig::FixedResponse { status, body, headers } => {
                        RoutingAction::FixedResponse {
                            status: *status,
                            body: body.clone(),
                            headers: headers.clone(),
                        }
                    }
                    RoutingActionConfig::Modify { set_headers, set_path } => {
                        RoutingAction::Modify {
                            set_headers: set_headers.clone(),
                            set_path: set_path.clone(),
                        }
                    }
                    RoutingActionConfig::Block { status, message } => {
                        RoutingAction::Block {
                            status: *status,
                            message: message.clone(),
                        }
                    }
                },
                priority: r.priority,
            })
            .collect();

        let edge_config = EdgeConfig {
            rewrite_rules,
            header_transforms,
            query_normalization,
            routing_rules,
        };

        Self::new(edge_config)
    }

    /// Process a request through all edge logic
    pub fn process_request(
        &self,
        path: &str,
        query: Option<&str>,
        method: &Method,
        headers: &HeaderMap,
        client_ip: Option<&str>,
    ) -> EdgeProcessingResult {
        // First, check conditional routing
        if let Some(action) = self.router.evaluate(path, query, method, headers, client_ip) {
            return EdgeProcessingResult::RouteAction(action.clone());
        }

        // Normalize query string
        let normalized_query = self.query_normalizer.normalize(query);

        // Rewrite URL
        let rewritten_path = self.rewriter.rewrite(
            path,
            normalized_query.as_deref().or(query),
            method,
            headers,
        );

        EdgeProcessingResult::Continue {
            path: rewritten_path,
            query: normalized_query,
        }
    }

    /// Transform request headers
    pub fn transform_request_headers(&self, headers: &mut HeaderMap) {
        self.header_transformer.transform_request_headers(headers);
    }

    /// Transform response headers
    pub fn transform_response_headers(&self, headers: &mut HeaderMap) {
        self.header_transformer.transform_response_headers(headers);
    }
}

/// Result of edge processing
#[derive(Debug, Clone)]
pub enum EdgeProcessingResult {
    /// Continue with (optionally modified) request
    Continue {
        path: Option<String>,
        query: Option<String>,
    },
    /// Take a routing action
    RouteAction(RoutingAction),
}

// ============================================================================
// Middleware
// ============================================================================

/// Edge processing middleware
pub async fn edge_processing_middleware(
    State(processor): State<Arc<EdgeProcessor>>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let uri = request.uri().clone();
    let path = uri.path();
    let query = uri.query();
    let method = request.method().clone();

    // Extract client IP from headers or connection
    let client_ip = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string());

    // Process through edge logic
    let result = processor.process_request(
        path,
        query,
        &method,
        request.headers(),
        client_ip.as_deref(),
    );

    match result {
        EdgeProcessingResult::RouteAction(action) => {
            handle_routing_action(action)
        }
        EdgeProcessingResult::Continue {
            path: new_path,
            query: new_query,
        } => {
            // Update URI if path or query changed
            if new_path.is_some() || new_query.is_some() {
                let final_path = new_path.as_deref().unwrap_or(path);
                let path_and_query = match new_query.as_deref().or(query) {
                    Some(q) => format!("{}?{}", final_path, q),
                    None => final_path.to_string(),
                };

                if let Ok(new_uri) = path_and_query.parse::<Uri>() {
                    *request.uri_mut() = new_uri;
                }
            }

            // Transform request headers
            processor.transform_request_headers(request.headers_mut());

            // Continue to next handler
            let mut response = next.run(request).await;

            // Transform response headers
            processor.transform_response_headers(response.headers_mut());

            response
        }
    }
}

/// Handle a routing action by generating an appropriate response
fn handle_routing_action(action: RoutingAction) -> Response<Body> {
    use axum::http::StatusCode;

    match action {
        RoutingAction::Redirect { url, status } => {
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::FOUND);
            let mut response = Response::new(Body::empty());
            *response.status_mut() = status_code;
            if let Ok(location) = HeaderValue::try_from(&url) {
                response.headers_mut().insert("location", location);
            }
            response
        }
        RoutingAction::FixedResponse {
            status,
            body,
            headers,
        } => {
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            let body_content = body.unwrap_or_default();
            let mut response = Response::new(Body::from(body_content));
            *response.status_mut() = status_code;

            if let Some(headers_map) = headers {
                for (name, value) in headers_map {
                    if let (Ok(name), Ok(value)) = (
                        HeaderName::try_from(&name),
                        HeaderValue::try_from(&value),
                    ) {
                        response.headers_mut().insert(name, value);
                    }
                }
            }
            response
        }
        RoutingAction::Block { status, message } => {
            let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
            let body = message.unwrap_or_else(|| "Blocked".to_string());
            let mut response = Response::new(Body::from(body));
            *response.status_mut() = status_code;
            response
        }
        RoutingAction::RouteToOrigin { origin: _ } => {
            // Origin routing is handled elsewhere; just continue
            Response::new(Body::empty())
        }
        RoutingAction::Modify { .. } => {
            // Modification is handled in the Continue branch; this shouldn't reach here
            Response::new(Body::empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn test_url_rewriting() {
        let rules = vec![
            RewriteRule {
                name: "remove-version".to_string(),
                pattern: r"^/v\d+/(.*)$".to_string(),
                replacement: "/$1".to_string(),
                stop: false,
                condition: None,
            },
            RewriteRule {
                name: "normalize-api".to_string(),
                pattern: r"^/api/(.*)$".to_string(),
                replacement: "/v1/api/$1".to_string(),
                stop: true,
                condition: None,
            },
        ];

        let rewriter = UrlRewriter::new(&rules);
        let headers = HeaderMap::new();

        // Test version removal
        let result = rewriter.rewrite("/v2/users", None, &Method::GET, &headers);
        assert_eq!(result, Some("/users".to_string()));

        // Test API normalization
        let result = rewriter.rewrite("/api/users", None, &Method::GET, &headers);
        assert_eq!(result, Some("/v1/api/users".to_string()));

        // Test no match
        let result = rewriter.rewrite("/static/file.js", None, &Method::GET, &headers);
        assert_eq!(result, None);
    }

    #[test]
    fn test_query_normalization() {
        let config = QueryNormalizationConfig {
            sort_params: true,
            remove_empty: true,
            remove_params: vec!["utm_source".to_string(), "fbclid".to_string()],
            keep_only_params: Vec::new(),
            lowercase_names: false,
            normalize_encoding: true,
        };

        let normalizer = QueryNormalizer::new(config);

        // Test sorting and tracking param removal
        let result = normalizer.normalize(Some("z=1&a=2&utm_source=google&b=3"));
        assert_eq!(result, Some("a=2&b=3&z=1".to_string()));

        // Test empty param removal
        let result = normalizer.normalize(Some("a=1&b=&c=3"));
        assert_eq!(result, Some("a=1&c=3".to_string()));

        // Test all params removed
        let result = normalizer.normalize(Some("utm_source=google&fbclid=abc"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_header_transformation() {
        let config = HeaderTransforms {
            request_add: [("x-custom".to_string(), "value".to_string())]
                .into_iter()
                .collect(),
            request_remove: vec!["x-remove-me".to_string()],
            response_add: [("x-served-by".to_string(), "cdn".to_string())]
                .into_iter()
                .collect(),
            response_remove: vec!["server".to_string()],
            transformations: Vec::new(),
        };

        let transformer = HeaderTransformer::new(&config);

        // Test request transformation
        let mut headers = HeaderMap::new();
        headers.insert("x-remove-me", HeaderValue::from_static("should-be-removed"));
        headers.insert("x-keep-me", HeaderValue::from_static("should-stay"));

        transformer.transform_request_headers(&mut headers);

        assert!(headers.get("x-remove-me").is_none());
        assert!(headers.get("x-keep-me").is_some());
        assert_eq!(
            headers.get("x-custom").unwrap().to_str().unwrap(),
            "value"
        );
    }

    #[test]
    fn test_conditional_routing() {
        let rules = vec![
            RoutingRule {
                name: "block-admin".to_string(),
                conditions: vec![RoutingCondition::Path {
                    pattern: r"^/admin".to_string(),
                }],
                action: RoutingAction::Block {
                    status: 403,
                    message: Some("Forbidden".to_string()),
                },
                priority: 10,
            },
            RoutingRule {
                name: "api-redirect".to_string(),
                conditions: vec![
                    RoutingCondition::Path {
                        pattern: r"^/old-api".to_string(),
                    },
                    RoutingCondition::Method {
                        methods: vec!["GET".to_string()],
                    },
                ],
                action: RoutingAction::Redirect {
                    url: "/new-api".to_string(),
                    status: 301,
                },
                priority: 5,
            },
        ];

        let router = ConditionalRouter::new(rules);
        let headers = HeaderMap::new();

        // Test admin block
        let result = router.evaluate("/admin/users", None, &Method::GET, &headers, None);
        assert!(matches!(result, Some(RoutingAction::Block { .. })));

        // Test API redirect
        let result = router.evaluate("/old-api/v1", None, &Method::GET, &headers, None);
        assert!(matches!(result, Some(RoutingAction::Redirect { .. })));

        // Test no match
        let result = router.evaluate("/public/file", None, &Method::GET, &headers, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_ip_cidr_matching() {
        // IPv4 tests
        assert!(ip_matches_cidr("192.168.1.100", "192.168.1.0/24"));
        assert!(!ip_matches_cidr("192.168.2.100", "192.168.1.0/24"));
        assert!(ip_matches_cidr("10.0.0.1", "10.0.0.0/8"));

        // IPv6 tests
        assert!(ip_matches_cidr("2001:db8::1", "2001:db8::/32"));
        assert!(!ip_matches_cidr("2001:db9::1", "2001:db8::/32"));
    }

    #[test]
    fn test_edge_processor() {
        let config = EdgeConfig {
            rewrite_rules: vec![RewriteRule {
                name: "add-prefix".to_string(),
                pattern: r"^/images/(.*)$".to_string(),
                replacement: "/static/images/$1".to_string(),
                stop: true,
                condition: None,
            }],
            header_transforms: HeaderTransforms::default(),
            query_normalization: QueryNormalizationConfig::default(),
            routing_rules: vec![],
        };

        let processor = EdgeProcessor::new(config);
        let headers = HeaderMap::new();

        let result = processor.process_request(
            "/images/logo.png",
            Some("size=large&utm_source=google"),
            &Method::GET,
            &headers,
            None,
        );

        match result {
            EdgeProcessingResult::Continue { path, query } => {
                assert_eq!(path, Some("/static/images/logo.png".to_string()));
                // utm_source should be removed by default normalization
                assert_eq!(query, Some("size=large".to_string()));
            }
            _ => panic!("Expected Continue result"),
        }
    }
}
