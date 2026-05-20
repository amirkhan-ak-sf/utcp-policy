//! Validated, strongly-typed policy configuration.
//!
//! Two layers:
//!
//!   codegen `Config` (deserialized from operator JSON, owns
//!   `pdk::hl::Service` values that the gateway can route through)
//!     -> `PolicyConfig` (validated, normalized, host-testable)
//!
//! Validation runs once at policy load via `PolicyConfig::from_config`.
//! Bad config (no upstreams, missing tool path, malformed inputs schema)
//! fails policy load with a clear error instead of every request.

use thiserror::Error;

use crate::generated::config::Config;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("at least one entry is required in 'upstreams'")]
    MissingUpstreams,
    #[error("enforcementMode must be one of: strict, permissive (got '{0}')")]
    BadEnforcementMode(String),
    #[error("upstreams[{upstream}].tools is empty")]
    EmptyTools { upstream: usize },
    #[error("upstreams[{upstream}].tools[{tool}].name is required")]
    MissingToolName { upstream: usize, tool: usize },
    #[error("upstreams[{upstream}].tools[{tool}] ({name}): path is required and must start with '/'")]
    BadToolPath {
        upstream: usize,
        tool: usize,
        name: String,
    },
    #[error("upstreams[{upstream}].tools[{tool}] ({name}): inputs failed to parse as JSON: {reason}")]
    BadToolInputsJson {
        upstream: usize,
        tool: usize,
        name: String,
        reason: String,
    },
    #[error("duplicate tool name '{0}' across upstreams; tool names must be unique")]
    DuplicateToolName(String),
    #[error("maxBodyBytes out of range [1024, 52428800]: {0}")]
    BadMaxBodyBytes(i64),
    #[error("outboundTimeoutSeconds out of range [1, 300]: {0}")]
    BadOutboundTimeout(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementMode {
    Strict,
    Permissive,
}

impl EnforcementMode {
    pub fn is_strict(self) -> bool {
        matches!(self, EnforcementMode::Strict)
    }
}

/// One tool, post-validation. The PDK Service handle for the upstream
/// lives in `PolicyConfig::services[upstream_index]` so the request
/// filter can borrow it for the outbound HttpClient call.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    pub upstream_index: usize,
    pub upstream_url: String,
    pub name: String,
    pub description: String,
    pub method: String,
    pub path_template: String,
    pub content_type: String,
    pub body_field: Option<String>,
    pub inputs: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub discovery_path: String,
    pub api_instance_proxy_path: String,
    pub utcp_version: String,
    pub manual_title: String,
    pub manual_info_version: String,
    pub manual_description: String,
    pub tools: Vec<ToolEntry>,
    pub upstream_urls: Vec<String>,
    pub enforcement_mode: EnforcementMode,
    pub validate_inputs: bool,
    pub max_body_bytes: usize,
    pub outbound_timeout_seconds: u32,
    pub require_principal: bool,
    pub principal_header: String,
    pub cache_control_header: String,
    pub tool_header_name: String,
    pub tool_name_prefix: String,
}

impl PolicyConfig {
    pub fn from_config(c: &Config) -> Result<Self, ConfigError> {
        if c.upstreams.is_empty() {
            return Err(ConfigError::MissingUpstreams);
        }

        let enforcement_mode = match c.enforcement_mode.as_deref().unwrap_or("strict") {
            "strict" => EnforcementMode::Strict,
            "permissive" => EnforcementMode::Permissive,
            other => return Err(ConfigError::BadEnforcementMode(other.into())),
        };

        let max_body_bytes = match c.max_body_bytes.unwrap_or(1_048_576) {
            v if (1024..=52_428_800).contains(&v) => v as usize,
            v => return Err(ConfigError::BadMaxBodyBytes(v)),
        };

        let outbound_timeout_seconds = match c.outbound_timeout_seconds.unwrap_or(30) {
            v if (1..=300).contains(&v) => v as u32,
            v => return Err(ConfigError::BadOutboundTimeout(v)),
        };

        let mut tools: Vec<ToolEntry> = Vec::new();
        let mut upstream_urls: Vec<String> = Vec::with_capacity(c.upstreams.len());

        for (u_idx, upstream) in c.upstreams.iter().enumerate() {
            let upstream_url = upstream.host.uri().to_string();
            upstream_urls.push(upstream_url.clone());

            let tool_list = upstream.tools.as_deref().unwrap_or(&[]);
            if tool_list.is_empty() {
                return Err(ConfigError::EmptyTools { upstream: u_idx });
            }

            for (t_idx, raw) in tool_list.iter().enumerate() {
                let name = raw.name.trim().to_string();
                if name.is_empty() {
                    return Err(ConfigError::MissingToolName {
                        upstream: u_idx,
                        tool: t_idx,
                    });
                }
                let path = raw.path.trim();
                if path.is_empty() || !path.starts_with('/') {
                    return Err(ConfigError::BadToolPath {
                        upstream: u_idx,
                        tool: t_idx,
                        name,
                    });
                }
                let inputs = match raw.inputs.as_deref().map(str::trim) {
                    Some(s) if !s.is_empty() => Some(
                        serde_json::from_str::<serde_json::Value>(s).map_err(|e| {
                            ConfigError::BadToolInputsJson {
                                upstream: u_idx,
                                tool: t_idx,
                                name: name.clone(),
                                reason: e.to_string(),
                            }
                        })?,
                    ),
                    _ => None,
                };
                let body_field_raw = raw
                    .body_field
                    .clone()
                    .unwrap_or_else(|| "body".to_string());
                let body_field = if body_field_raw.is_empty() {
                    None
                } else {
                    Some(body_field_raw)
                };
                tools.push(ToolEntry {
                    upstream_index: u_idx,
                    upstream_url: upstream_url.clone(),
                    name,
                    description: raw.description.clone().unwrap_or_default(),
                    method: raw
                        .method
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "POST".to_string())
                        .to_ascii_uppercase(),
                    path_template: path.to_string(),
                    content_type: raw
                        .content_type
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "application/json".to_string()),
                    body_field,
                    inputs,
                });
            }
        }

        // Tool names must be globally unique — they're how callers
        // identify routes, and the audit/header tagging assumes a 1:1
        // mapping.
        for i in 0..tools.len() {
            for j in (i + 1)..tools.len() {
                if tools[i].name == tools[j].name {
                    return Err(ConfigError::DuplicateToolName(tools[i].name.clone()));
                }
            }
        }

        let tool_name_prefix = c.tool_name_prefix.clone().unwrap_or_default();
        if !tool_name_prefix.is_empty() {
            for t in &mut tools {
                t.name = format!("{tool_name_prefix}{}", t.name);
            }
        }

        Ok(Self {
            discovery_path: c
                .discovery_path
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/utcp".to_string()),
            api_instance_proxy_path: normalize_proxy_path(
                c.api_instance_proxy_path.as_deref().unwrap_or(""),
            ),
            utcp_version: c
                .utcp_version
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "1.0.1".to_string()),
            manual_title: c.manual_title.clone().unwrap_or_default(),
            manual_info_version: c.manual_info_version.clone().unwrap_or_default(),
            manual_description: c.manual_description.clone().unwrap_or_default(),
            tools,
            upstream_urls,
            enforcement_mode,
            validate_inputs: c.validate_inputs.unwrap_or(true),
            max_body_bytes,
            outbound_timeout_seconds,
            require_principal: c.require_principal.unwrap_or(false),
            principal_header: c
                .principal_header
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "x-anypoint-client-id".to_string()),
            cache_control_header: c
                .cache_control_header
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "public, max-age=60".to_string()),
            tool_header_name: c
                .tool_header_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "x-utcp-tool".to_string()),
            tool_name_prefix,
        })
    }
}

/// Strip trailing '/' so prefix comparison works cleanly. Empty stays
/// empty (i.e. "API instance is at root").
fn normalize_proxy_path(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let with_lead = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let trimmed_trailing = with_lead.trim_end_matches('/');
    if trimmed_trailing.is_empty() {
        String::new()
    } else {
        trimmed_trailing.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Config` is mostly Option<_>; build a minimal valid one.
    /// Unfortunately the codegen `Upstreams0Config` carries a
    /// `pdk::hl::Service` we can't construct outside of a real PDK
    /// runtime, so our host-side tests exercise `normalize_proxy_path`
    /// and the smaller pure helpers. Behavioural validation lives in
    /// the integration tests under `tests/`.
    #[test]
    fn normalize_proxy_path_handles_edges() {
        assert_eq!(normalize_proxy_path(""), "");
        assert_eq!(normalize_proxy_path("/"), "");
        assert_eq!(normalize_proxy_path("/foo"), "/foo");
        assert_eq!(normalize_proxy_path("/foo/"), "/foo");
        assert_eq!(normalize_proxy_path("foo"), "/foo");
        assert_eq!(normalize_proxy_path("foo/"), "/foo");
        assert_eq!(normalize_proxy_path("/foo/bar/"), "/foo/bar");
    }

    #[test]
    fn enforcement_is_strict_default() {
        assert!(EnforcementMode::Strict.is_strict());
        assert!(!EnforcementMode::Permissive.is_strict());
    }
}
