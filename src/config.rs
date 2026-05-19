//! Validated, strongly-typed policy configuration.
//!
//! Three layers, mirroring the data-masking-policy / oauth-2-jwt-bearer
//! convention:
//!
//!   codegen `Config` (deserialized from operator JSON)
//!     -> `RawConfig` (host-testable, no PDK types)
//!     -> `PolicyConfig` (validated, normalized)
//!
//! Validation runs once at policy load via `PolicyConfig::from_raw`. Bad
//! config (unknown enforcement mode, out-of-range size, missing OpenAPI
//! URL when `manualSource=openapi`) fails policy load with a clear error
//! instead of every request.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("manualSource must be one of: openapi, static, hybrid (got '{0}')")]
    BadManualSource(String),
    #[error("enforcementMode must be one of: strict, permissive (got '{0}')")]
    BadEnforcementMode(String),
    #[error("manualSource={0} requires openapiUrl to be set")]
    MissingOpenapiUrl(String),
    #[error("openapiUrl must be https:// (set allowInsecureOpenapiUrl=true to permit http:// for local dev)")]
    InsecureOpenapiUrl,
    #[error("openapiUrl is not a valid URL: {0}")]
    BadOpenapiUrl(String),
    #[error("manualSource=static requires staticManual to be a non-empty UTCP Manual JSON object")]
    MissingStaticManual,
    #[error("staticManual failed to parse as JSON: {0}")]
    BadStaticManualJson(String),
    #[error("staticManual must be a JSON object with at least 'tools'")]
    MalformedStaticManual,
    #[error("maxBodyBytes out of range [1024, 52428800]: {0}")]
    BadMaxBodyBytes(i64),
    #[error("refreshIntervalSeconds out of range [30, 86400]: {0}")]
    BadRefreshInterval(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualSource {
    OpenApi,
    Static,
    Hybrid,
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

#[derive(Debug, Clone)]
pub struct RawConfig {
    pub discovery_path: Option<String>,
    pub utcp_version: Option<String>,
    pub manual_source: String,
    pub openapi_url: Option<String>,
    pub allow_insecure_openapi_url: Option<bool>,
    pub refresh_interval_seconds: Option<i64>,
    pub static_manual: Option<String>,
    pub enforcement_mode: Option<String>,
    pub validate_inputs: Option<bool>,
    pub max_body_bytes: Option<i64>,
    pub require_principal: Option<bool>,
    pub principal_header: Option<String>,
    pub cache_control_header: Option<String>,
    pub tool_header_name: Option<String>,
    pub tool_name_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub discovery_path: String,
    pub utcp_version: String,
    pub manual_source: ManualSource,
    pub openapi_url: Option<String>,
    pub refresh_interval_seconds: u32,
    pub static_manual_json: Option<serde_json::Value>,
    pub enforcement_mode: EnforcementMode,
    pub validate_inputs: bool,
    pub max_body_bytes: usize,
    pub require_principal: bool,
    pub principal_header: String,
    pub cache_control_header: String,
    pub tool_header_name: String,
    pub tool_name_prefix: String,
}

impl PolicyConfig {
    pub fn from_raw(raw: RawConfig) -> Result<Self, ConfigError> {
        let manual_source = match raw.manual_source.as_str() {
            "openapi" => ManualSource::OpenApi,
            "static" => ManualSource::Static,
            "hybrid" => ManualSource::Hybrid,
            other => return Err(ConfigError::BadManualSource(other.into())),
        };

        let enforcement_mode = match raw.enforcement_mode.as_deref().unwrap_or("strict") {
            "strict" => EnforcementMode::Strict,
            "permissive" => EnforcementMode::Permissive,
            other => return Err(ConfigError::BadEnforcementMode(other.into())),
        };

        let max_body_bytes = match raw.max_body_bytes.unwrap_or(1_048_576) {
            v if (1024..=52_428_800).contains(&v) => v as usize,
            v => return Err(ConfigError::BadMaxBodyBytes(v)),
        };

        let refresh_interval_seconds = match raw.refresh_interval_seconds.unwrap_or(300) {
            v if (30..=86_400).contains(&v) => v as u32,
            v => return Err(ConfigError::BadRefreshInterval(v)),
        };

        let allow_insecure = raw.allow_insecure_openapi_url.unwrap_or(false);

        let openapi_url = match (manual_source, raw.openapi_url.as_deref()) {
            (ManualSource::OpenApi | ManualSource::Hybrid, Some(s)) if !s.trim().is_empty() => {
                let parsed = url::Url::parse(s.trim())
                    .map_err(|e| ConfigError::BadOpenapiUrl(e.to_string()))?;
                let scheme = parsed.scheme();
                if scheme != "https" && !allow_insecure {
                    let host = parsed.host_str().unwrap_or("");
                    let is_local = host == "localhost" || host == "127.0.0.1";
                    if !(scheme == "http" && is_local) {
                        return Err(ConfigError::InsecureOpenapiUrl);
                    }
                }
                Some(parsed.to_string())
            }
            (ManualSource::OpenApi, _) => return Err(ConfigError::MissingOpenapiUrl("openapi".into())),
            (ManualSource::Hybrid, _) => return Err(ConfigError::MissingOpenapiUrl("hybrid".into())),
            (ManualSource::Static, _) => None,
        };

        let static_manual_json = match (manual_source, raw.static_manual.as_deref()) {
            (ManualSource::Static, Some(s)) | (ManualSource::Hybrid, Some(s))
                if !s.trim().is_empty() =>
            {
                let v: serde_json::Value = serde_json::from_str(s)
                    .map_err(|e| ConfigError::BadStaticManualJson(e.to_string()))?;
                if !v.is_object() {
                    return Err(ConfigError::MalformedStaticManual);
                }
                Some(v)
            }
            (ManualSource::Static, _) => return Err(ConfigError::MissingStaticManual),
            _ => None,
        };

        Ok(Self {
            discovery_path: raw
                .discovery_path
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/utcp".to_string()),
            utcp_version: raw
                .utcp_version
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "1.0.1".to_string()),
            manual_source,
            openapi_url,
            refresh_interval_seconds,
            static_manual_json,
            enforcement_mode,
            validate_inputs: raw.validate_inputs.unwrap_or(true),
            max_body_bytes,
            require_principal: raw.require_principal.unwrap_or(false),
            principal_header: raw
                .principal_header
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "x-anypoint-client-id".to_string()),
            cache_control_header: raw
                .cache_control_header
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "public, max-age=60".to_string()),
            tool_header_name: raw
                .tool_header_name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "x-utcp-tool".to_string()),
            tool_name_prefix: raw.tool_name_prefix.unwrap_or_default(),
        })
    }
}

impl From<&crate::generated::config::Config> for RawConfig {
    fn from(c: &crate::generated::config::Config) -> Self {
        RawConfig {
            discovery_path: c.discovery_path.clone(),
            utcp_version: c.utcp_version.clone(),
            manual_source: c.manual_source.clone(),
            openapi_url: c.openapi_url.clone(),
            allow_insecure_openapi_url: c.allow_insecure_openapi_url,
            refresh_interval_seconds: c.refresh_interval_seconds,
            static_manual: c.static_manual.clone(),
            enforcement_mode: c.enforcement_mode.clone(),
            validate_inputs: c.validate_inputs,
            max_body_bytes: c.max_body_bytes,
            require_principal: c.require_principal,
            principal_header: c.principal_header.clone(),
            cache_control_header: c.cache_control_header.clone(),
            tool_header_name: c.tool_header_name.clone(),
            tool_name_prefix: c.tool_name_prefix.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(source: &str) -> RawConfig {
        RawConfig {
            discovery_path: None,
            utcp_version: None,
            manual_source: source.into(),
            openapi_url: None,
            allow_insecure_openapi_url: None,
            refresh_interval_seconds: None,
            static_manual: None,
            enforcement_mode: None,
            validate_inputs: None,
            max_body_bytes: None,
            require_principal: None,
            principal_header: None,
            cache_control_header: None,
            tool_header_name: None,
            tool_name_prefix: None,
        }
    }

    #[test]
    fn defaults_apply_for_openapi_source() {
        let mut r = raw("openapi");
        r.openapi_url = Some("https://example.com/openapi.json".into());
        let cfg = PolicyConfig::from_raw(r).unwrap();
        assert_eq!(cfg.discovery_path, "/utcp");
        assert_eq!(cfg.utcp_version, "1.0.1");
        assert_eq!(cfg.refresh_interval_seconds, 300);
        assert_eq!(cfg.max_body_bytes, 1_048_576);
        assert!(cfg.validate_inputs);
        assert!(cfg.enforcement_mode.is_strict());
    }

    #[test]
    fn missing_openapi_url_rejected() {
        let err = PolicyConfig::from_raw(raw("openapi")).unwrap_err();
        assert!(matches!(err, ConfigError::MissingOpenapiUrl(_)));
    }

    #[test]
    fn http_openapi_url_rejected_unless_localhost_or_allow_flag() {
        let mut r = raw("openapi");
        r.openapi_url = Some("http://example.com/openapi.json".into());
        let err = PolicyConfig::from_raw(r.clone()).unwrap_err();
        assert!(matches!(err, ConfigError::InsecureOpenapiUrl));

        r.allow_insecure_openapi_url = Some(true);
        assert!(PolicyConfig::from_raw(r).is_ok());
    }

    #[test]
    fn http_localhost_url_allowed_without_flag() {
        let mut r = raw("openapi");
        r.openapi_url = Some("http://localhost:8080/openapi.json".into());
        assert!(PolicyConfig::from_raw(r).is_ok());
    }

    #[test]
    fn static_source_requires_manual() {
        let err = PolicyConfig::from_raw(raw("static")).unwrap_err();
        assert!(matches!(err, ConfigError::MissingStaticManual));
    }

    #[test]
    fn static_source_parses_manual_json() {
        let mut r = raw("static");
        r.static_manual = Some(
            r#"{"utcp_version":"1.0.1","tools":[]}"#.into(),
        );
        let cfg = PolicyConfig::from_raw(r).unwrap();
        assert!(cfg.static_manual_json.is_some());
        assert_eq!(cfg.openapi_url, None);
    }

    #[test]
    fn bad_enforcement_rejected() {
        let mut r = raw("static");
        r.static_manual = Some(r#"{"utcp_version":"1.0.1","tools":[]}"#.into());
        r.enforcement_mode = Some("yolo".into());
        let err = PolicyConfig::from_raw(r).unwrap_err();
        assert!(matches!(err, ConfigError::BadEnforcementMode(_)));
    }

    #[test]
    fn out_of_range_max_body_bytes_rejected() {
        let mut r = raw("openapi");
        r.openapi_url = Some("https://example.com/openapi.json".into());
        r.max_body_bytes = Some(100);
        let err = PolicyConfig::from_raw(r).unwrap_err();
        assert!(matches!(err, ConfigError::BadMaxBodyBytes(100)));
    }
}
