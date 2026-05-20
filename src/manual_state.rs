//! Compiled in-memory state for the served Manual.
//!
//! Builds a `Manual` from the validated `PolicyConfig.tools` (one entry
//! per upstream tool), compiles a path-template router, and pre-renders
//! the JSON bytes the discovery endpoint will hand out.

use anyhow::{anyhow, Context};
use serde_json::{Map, Value};

use crate::config::{PolicyConfig, ToolEntry};
use crate::manual::{
    model::{CallTemplate, HttpCallTemplate, Manual, Tool},
    render,
};
use crate::validate::ToolRouter;

pub struct ManualState {
    pub manual: Manual,
    pub manual_bytes: Vec<u8>,
    pub router: ToolRouter,
}

impl ManualState {
    pub fn build(cfg: &PolicyConfig) -> anyhow::Result<Self> {
        if cfg.tools.is_empty() {
            return Err(anyhow!("no tools configured"));
        }

        let mut tools = Vec::with_capacity(cfg.tools.len());
        for entry in &cfg.tools {
            tools.push(build_tool(entry)?);
        }

        let info = build_info(
            &cfg.manual_title,
            &cfg.manual_info_version,
            &cfg.manual_description,
        );

        let manual = Manual {
            manual_version: "1.0.0".into(),
            utcp_version: cfg.utcp_version.clone(),
            info,
            variables: None,
            tools,
        };

        Self::finalize(manual)
    }

    fn finalize(manual: Manual) -> anyhow::Result<Self> {
        let router = ToolRouter::build(&manual)
            .map_err(|e| anyhow!("router compilation failed: {e}"))?;
        let manual_bytes = render::to_json_bytes(&manual)
            .map_err(|e| anyhow!("manual rendering failed: {e}"))?;
        Ok(Self {
            manual,
            manual_bytes,
            router,
        })
    }
}

fn build_tool(entry: &ToolEntry) -> anyhow::Result<Tool> {
    let url = join_url(&entry.upstream_url, &entry.path_template)
        .with_context(|| format!("tool '{}': could not compose URL", entry.name))?;
    Ok(Tool {
        name: entry.name.clone(),
        description: entry.description.clone(),
        inputs: entry.inputs.clone(),
        outputs: None,
        tags: Vec::new(),
        average_response_size: None,
        tool_call_template: CallTemplate::Http(HttpCallTemplate {
            url,
            http_method: entry.method.clone(),
            content_type: entry.content_type.clone(),
            headers: serde_json::Map::new(),
            header_fields: Vec::new(),
            body_field: entry.body_field.clone(),
            auth: None,
        }),
    })
}

/// Concatenate upstream + path with exactly one `/` between them.
/// Upstream may already end in a base path (`https://host/api`); tool
/// path always starts with `/`.
fn join_url(upstream: &str, tool_path: &str) -> anyhow::Result<String> {
    let host = upstream.trim_end_matches('/');
    if !tool_path.starts_with('/') {
        return Err(anyhow!("tool path must start with '/', got '{tool_path}'"));
    }
    Ok(format!("{host}{tool_path}"))
}

fn build_info(title: &str, version: &str, description: &str) -> Option<Value> {
    let mut info = Map::new();
    if !title.is_empty() {
        info.insert("title".into(), Value::String(title.into()));
    }
    if !version.is_empty() {
        info.insert("version".into(), Value::String(version.into()));
    }
    if !description.is_empty() {
        info.insert("description".into(), Value::String(description.into()));
    }
    if info.is_empty() {
        None
    } else {
        Some(Value::Object(info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_tools(tools: Vec<ToolEntry>) -> PolicyConfig {
        PolicyConfig {
            discovery_path: "/utcp".into(),
            api_instance_proxy_path: String::new(),
            utcp_version: "1.0.1".into(),
            manual_title: "SAP Bridge".into(),
            manual_info_version: "1.0.0".into(),
            manual_description: "".into(),
            tools,
            upstream_urls: vec!["https://sap.example.com".into()],
            enforcement_mode: crate::config::EnforcementMode::Strict,
            validate_inputs: true,
            max_body_bytes: 1_048_576,
            outbound_timeout_seconds: 30,
            require_principal: false,
            principal_header: "x-anypoint-client-id".into(),
            cache_control_header: "public, max-age=60".into(),
            tool_header_name: "x-utcp-tool".into(),
            tool_name_prefix: "".into(),
        }
    }

    fn entry(name: &str, method: &str, path: &str) -> ToolEntry {
        ToolEntry {
            upstream_index: 0,
            upstream_url: "https://sap.example.com".into(),
            name: name.into(),
            description: format!("{name} description"),
            method: method.into(),
            path_template: path.into(),
            content_type: "application/json".into(),
            body_field: Some("body".into()),
            inputs: Some(serde_json::json!({
                "type":"object","required":["body"],
                "properties":{"body":{"type":"object"}}
            })),
        }
    }

    #[test]
    fn synthesises_manual_from_tools() {
        let cfg = cfg_with_tools(vec![
            entry("createSalesOrder", "POST", "/api/order"),
            entry("checkInventory", "POST", "/mmbe"),
        ]);
        let state = ManualState::build(&cfg).unwrap();
        assert_eq!(state.manual.tools.len(), 2);
        let urls: Vec<_> = state
            .manual
            .tools
            .iter()
            .map(|t| match &t.tool_call_template {
                CallTemplate::Http(h) => h.url.clone(),
            })
            .collect();
        assert!(urls.contains(&"https://sap.example.com/api/order".to_string()));
        assert!(urls.contains(&"https://sap.example.com/mmbe".to_string()));
        assert_eq!(
            state
                .manual
                .info
                .as_ref()
                .and_then(|v| v.get("title"))
                .and_then(|v| v.as_str()),
            Some("SAP Bridge")
        );
    }

    #[test]
    fn join_url_collapses_trailing_slash() {
        assert_eq!(
            join_url("https://h", "/path").unwrap(),
            "https://h/path"
        );
        assert_eq!(
            join_url("https://h/", "/path").unwrap(),
            "https://h/path"
        );
        assert_eq!(
            join_url("https://h/api", "/path").unwrap(),
            "https://h/api/path"
        );
    }
}
