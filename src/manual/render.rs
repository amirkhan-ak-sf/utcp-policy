//! Deterministic JSON rendering for the served Manual.
//!
//! Two stability requirements:
//!
//!   1. Tools are emitted sorted by `name`. OpenAPI key iteration order
//!      can vary between runs (especially across YAML and JSON inputs);
//!      we sort once at render time so two runs of the same source
//!      produce byte-identical output.
//!   2. Object keys within each tool follow the declaration order on
//!      `Tool` (struct order). This relies on serde_json's
//!      `preserve_order` feature being enabled for parsed values, plus
//!      Serialize's natural field-order behaviour.

use crate::manual::model::Manual;

pub fn to_json_bytes(manual: &Manual) -> anyhow::Result<Vec<u8>> {
    let mut sorted = manual.clone();
    sorted.tools.sort_by(|a, b| a.name.cmp(&b.name));
    let bytes = serde_json::to_vec(&sorted)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual::model::{CallTemplate, HttpCallTemplate, Tool};

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: String::new(),
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
        }
    }

    #[test]
    fn tools_sorted_by_name() {
        let manual = Manual {
            manual_version: "1.0.0".into(),
            utcp_version: "1.0.1".into(),
            info: None,
            variables: None,
            tools: vec![tool("zeta"), tool("alpha"), tool("middle")],
        };
        let bytes = to_json_bytes(&manual).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        let i_alpha = s.find("\"alpha\"").unwrap();
        let i_middle = s.find("\"middle\"").unwrap();
        let i_zeta = s.find("\"zeta\"").unwrap();
        assert!(i_alpha < i_middle && i_middle < i_zeta);
    }

    #[test]
    fn output_is_byte_stable() {
        let mk = || Manual {
            manual_version: "1.0.0".into(),
            utcp_version: "1.0.1".into(),
            info: None,
            variables: None,
            tools: vec![tool("a"), tool("b")],
        };
        assert_eq!(to_json_bytes(&mk()).unwrap(), to_json_bytes(&mk()).unwrap());
    }
}
