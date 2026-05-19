//! In-memory UTCP Manual representation.
//!
//! Mirrors UTCP v1.x. All optional fields are `Option<_>` so the
//! deterministic serializer can omit them when unset rather than emit
//! `null`. JSON Schema fragments (`inputs`, `outputs`) are kept as
//! `serde_json::Value` to avoid re-modeling JSON Schema 2020-12 in this
//! policy.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manual {
    pub manual_version: String,
    pub utcp_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_response_size: Option<u64>,
    pub tool_call_template: CallTemplate,
}

/// Currently we only emit HTTP call templates — non-HTTP transports are
/// out of scope for v1 (see ROADMAP). Modeled as a tagged enum so adding
/// `cli`, `sse`, etc. later is purely additive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "call_template_type", rename_all = "snake_case")]
pub enum CallTemplate {
    Http(HttpCallTemplate),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpCallTemplate {
    pub url: String,
    pub http_method: String,
    #[serde(default = "default_content_type", skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub headers: serde_json::Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<Value>,
}

fn default_content_type() -> String {
    "application/json".to_string()
}

impl Manual {
    /// Parse a UTCP Manual from its canonical JSON shape. Used by
    /// `manualSource=static` and `manualSource=hybrid`.
    pub fn from_value(v: Value) -> anyhow::Result<Self> {
        let manual: Manual = serde_json::from_value(v)?;
        Ok(manual)
    }
}
