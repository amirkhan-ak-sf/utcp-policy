//! Hybrid-mode overrides: take the OpenAPI-derived Manual and merge a
//! static patch document on top. The patch document follows the same
//! shape as a Manual; missing tools are added, present tools have their
//! fields shallow-merged.
//!
//! Currently a deliberately small surface (deferred to ROADMAP). v0
//! merge rules: only top-level `info`, `variables`, and tool-level
//! `description`, `tags`, `auth`, and `headers` are applied.

use serde_json::{Map, Value};

use crate::manual::model::{CallTemplate, Manual};

pub fn apply_overrides(base: &mut Manual, patch: &Value) {
    let Some(obj) = patch.as_object() else { return };

    if let Some(info) = obj.get("info") {
        base.info = Some(info.clone());
    }
    if let Some(vars) = obj.get("variables") {
        base.variables = Some(vars.clone());
    }

    let Some(patch_tools) = obj.get("tools").and_then(Value::as_array) else { return };

    for pt in patch_tools {
        let Some(name) = pt.get("name").and_then(Value::as_str) else { continue };
        let Some(target) = base.tools.iter_mut().find(|t| t.name == name) else { continue };

        if let Some(desc) = pt.get("description").and_then(Value::as_str) {
            target.description = desc.to_string();
        }
        if let Some(tags) = pt.get("tags").and_then(Value::as_array) {
            target.tags = tags
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(avg) = pt.get("average_response_size").and_then(Value::as_u64) {
            target.average_response_size = Some(avg);
        }

        let Some(patch_call) = pt.get("tool_call_template").and_then(Value::as_object) else {
            continue;
        };

        match &mut target.tool_call_template {
            CallTemplate::Http(http) => {
                if let Some(auth) = patch_call.get("auth") {
                    http.auth = Some(auth.clone());
                }
                if let Some(headers) = patch_call.get("headers").and_then(Value::as_object) {
                    let merged = merge_headers(&http.headers, headers);
                    http.headers = merged;
                }
            }
        }
    }
}

fn merge_headers(base: &Map<String, Value>, patch: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base.clone();
    for (k, v) in patch {
        out.insert(k.clone(), v.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual::model::{HttpCallTemplate, Tool};

    fn manual_with(name: &str) -> Manual {
        Manual {
            manual_version: "1.0.0".into(),
            utcp_version: "1.0.1".into(),
            info: None,
            variables: None,
            tools: vec![Tool {
                name: name.into(),
                description: "original".into(),
                inputs: None,
                outputs: None,
                tags: vec![],
                average_response_size: None,
                tool_call_template: CallTemplate::Http(HttpCallTemplate {
                    url: format!("https://api.example.com/{name}"),
                    http_method: "GET".into(),
                    content_type: "application/json".into(),
                    ..Default::default()
                }),
            }],
        }
    }

    #[test]
    fn description_is_overridden() {
        let mut m = manual_with("getUser");
        let patch = serde_json::json!({
            "tools": [
                { "name": "getUser", "description": "patched" }
            ]
        });
        apply_overrides(&mut m, &patch);
        assert_eq!(m.tools[0].description, "patched");
    }

    #[test]
    fn auth_is_set_via_patch() {
        let mut m = manual_with("getUser");
        let patch = serde_json::json!({
            "tools": [
                {
                    "name": "getUser",
                    "tool_call_template": {
                        "auth": {"auth_type":"api_key","api_key":"${TOKEN}","var_name":"Authorization","location":"header"}
                    }
                }
            ]
        });
        apply_overrides(&mut m, &patch);
        match &m.tools[0].tool_call_template {
            CallTemplate::Http(http) => assert!(http.auth.is_some()),
        }
    }

    #[test]
    fn unknown_tool_is_ignored() {
        let mut m = manual_with("getUser");
        let patch = serde_json::json!({"tools":[{"name":"missing","description":"x"}]});
        apply_overrides(&mut m, &patch);
        assert_eq!(m.tools[0].description, "original");
    }
}
