//! Compiled in-memory state for the served Manual.
//!
//! Builds a `Manual` from the validated `PolicyConfig.tools`, compiles
//! a path-template router, and pre-renders the JSON bytes the
//! discovery endpoint will hand out. Each tool's
//! `tool_call_template.url` is composed from `publicBaseUrl + path` so
//! agents have a public address to invoke.

use anyhow::anyhow;
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
            tools.push(build_tool(entry, &cfg.public_base_url));
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

        let router = ToolRouter::build(&cfg.tools)
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

fn build_tool(entry: &ToolEntry, public_base_url: &str) -> Tool {
    let url = compose_url(public_base_url, &entry.path_template);
    Tool {
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
    }
}

/// Compose `publicBaseUrl + tool_path`. Empty `publicBaseUrl` yields
/// the path alone (a relative URL); agents resolve it against
/// whatever address they discovered the Manual on.
fn compose_url(public_base_url: &str, tool_path: &str) -> String {
    if public_base_url.is_empty() {
        return tool_path.to_string();
    }
    let host = public_base_url.trim_end_matches('/');
    if tool_path.starts_with('/') {
        format!("{host}{tool_path}")
    } else {
        format!("{host}/{tool_path}")
    }
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

    fn cfg_with_tools(public_base_url: &str, tools: Vec<ToolEntry>) -> PolicyConfig {
        PolicyConfig {
            discovery_path: "/utcp".into(),
            api_instance_proxy_path: String::new(),
            public_base_url: public_base_url.into(),
            egress_service: pdk::hl::Service::default(),
            outbound_timeout: std::time::Duration::from_secs(30),
            utcp_version: "1.0.1".into(),
            manual_title: "SAP Bridge".into(),
            manual_info_version: "1.0.0".into(),
            manual_description: "".into(),
            tools,
            enforcement_mode: crate::config::EnforcementMode::Strict,
            validate_inputs: true,
            max_body_bytes: 1_048_576,
            require_principal: false,
            principal_header: "x-anypoint-client-id".into(),
            cache_control_header: "public, max-age=60".into(),
            tool_header_name: "x-utcp-tool".into(),
            tool_name_prefix: "".into(),
        }
    }

    fn entry(name: &str, method: &str, path: &str) -> ToolEntry {
        ToolEntry {
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
        let cfg = cfg_with_tools(
            "https://gw.example.com/erp",
            vec![
                entry("createSalesOrder", "POST", "/api/order"),
                entry("checkInventory", "POST", "/mmbe"),
            ],
        );
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
        assert!(urls.contains(&"https://gw.example.com/erp/api/order".to_string()));
        assert!(urls.contains(&"https://gw.example.com/erp/mmbe".to_string()));
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
    fn empty_public_base_url_yields_relative_paths() {
        let cfg = cfg_with_tools("", vec![entry("checkInventory", "POST", "/mmbe")]);
        let state = ManualState::build(&cfg).unwrap();
        let url = match &state.manual.tools[0].tool_call_template {
            CallTemplate::Http(h) => h.url.clone(),
        };
        assert_eq!(url, "/mmbe");
    }

    #[test]
    fn compose_url_collapses_trailing_slash() {
        assert_eq!(compose_url("https://h", "/path"), "https://h/path");
        assert_eq!(compose_url("https://h/", "/path"), "https://h/path");
        assert_eq!(compose_url("https://h/api", "/path"), "https://h/api/path");
    }
}
