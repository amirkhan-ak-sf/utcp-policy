//! Structured audit lines.
//!
//! The policy doesn't pick a sink — it emits one line per request via
//! `pdk::logger` so the operator's existing log pipeline (Anypoint
//! Monitoring, stdout, sidecar) carries it. We log at info for happy
//! paths and warn for validation failures so log-level filters can
//! tease the two apart.

use serde::Serialize;
use serde_json::Value;

use crate::validate::Violation;

#[derive(Debug, Serialize)]
pub struct AuditLine<'a> {
    pub policy: &'static str,
    pub event: &'static str,
    pub method: &'a str,
    pub path: &'a str,
    pub tool: Option<&'a str>,
    pub principal: Option<&'a str>,
    pub validation_status: &'static str,
    pub status: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<&'a Violation>,
}

pub fn render_violations_body(tool: &str, violations: &[Violation]) -> Vec<u8> {
    let arr: Vec<Value> = violations
        .iter()
        .map(|v| {
            serde_json::json!({
                "path": v.path,
                "message": v.message,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "error": "utcp.input_invalid",
        "tool": tool,
        "violations": arr,
    });
    serde_json::to_vec(&payload).unwrap_or_else(|_| br#"{"error":"utcp.input_invalid"}"#.to_vec())
}

pub fn render_error_body(code: &str) -> Vec<u8> {
    let payload = serde_json::json!({ "error": code });
    serde_json::to_vec(&payload)
        .unwrap_or_else(|_| format!(r#"{{"error":"{code}"}}"#).into_bytes())
}
