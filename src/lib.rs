//! UTCP Manual Validator policy entrypoint.
//!
//! Inbound request flow (`on_request`):
//!
//!   1. If the path is `<discoveryPath>` and method is `GET`, short-circuit
//!      with the pre-rendered Manual JSON.
//!   2. If `requirePrincipal=true` and the configured principal header
//!      is absent, short-circuit with 401 `utcp.unauthenticated`.
//!   3. Otherwise resolve `(method, path)` against the compiled router.
//!         * No match + strict mode  -> 404 `utcp.tool_not_declared`.
//!         * No match + permissive   -> pass through, log unmatched.
//!         * Match -> set `<toolHeaderName>: <tool>`, optionally
//!                    validate the body against the tool's input schema,
//!                    return 400 with violations on failure, otherwise
//!                    Continue.

mod audit;
mod config;
mod generated;
pub mod manual;
mod manual_state;
pub mod validate;

use std::rc::Rc;

use anyhow::anyhow;
use pdk::cache::CacheBuilder;
use pdk::hl::*;
use pdk::logger;

use crate::config::{EnforcementMode, PolicyConfig};
use crate::generated::config::Config;
use crate::manual_state::ManualState;

#[entrypoint]
pub async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    _cache_builder: CacheBuilder,
) -> anyhow::Result<()> {
    let raw: Config = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow!("invalid policy configuration: {e}"))?;

    let cfg = PolicyConfig::from_raw((&raw).into())
        .map_err(|e| anyhow!("policy configuration rejected: {e}"))?;

    let state = ManualState::for_source(&cfg).map_err(|e| {
        anyhow!(
            "utcp-manual-validator: failed to build Manual at policy load: {e}"
        )
    })?;

    logger::info!(
        "utcp-manual-validator: loaded {} tool(s); discoveryPath='{}' enforcement={:?} validateInputs={}",
        state.manual.tools.len(),
        cfg.discovery_path,
        cfg.enforcement_mode,
        cfg.validate_inputs,
    );

    let cfg = Rc::new(cfg);
    let state = Rc::new(state);

    let request_cfg = cfg.clone();
    let request_state = state.clone();

    let filter = on_request(move |request, _client: HttpClient| {
        let cfg = request_cfg.clone();
        let state = request_state.clone();
        async move { request_filter(request, cfg, state).await }
    });

    launcher.launch(filter).await?;
    Ok(())
}

async fn request_filter(
    request: RequestHeadersState,
    cfg: Rc<PolicyConfig>,
    state: Rc<ManualState>,
) -> Flow<()> {
    let method = request.method();
    let path = request.path();

    // 1) Discovery short-circuit. We compare against the path
    //    exclusive of any query string so `/utcp?foo=bar` still serves.
    let bare_path = path.split_once('?').map(|(p, _)| p).unwrap_or(&path);
    if method.eq_ignore_ascii_case("GET") && bare_path == cfg.discovery_path {
        logger::debug!("utcp-manual-validator: serving manual on {}", cfg.discovery_path);
        return Flow::Break(
            Response::new(200)
                .with_headers(vec![
                    ("content-type".into(), "application/json".into()),
                    ("cache-control".into(), cfg.cache_control_header.clone()),
                    ("x-utcp-version".into(), cfg.utcp_version.clone()),
                ])
                .with_body(state.manual_bytes.clone()),
        );
    }

    // 2) Optional principal enforcement.
    if cfg.require_principal && request.handler().header(&cfg.principal_header).is_none() {
        logger::warn!(
            "utcp-manual-validator: rejecting request to {} {}: missing principal header '{}'",
            method,
            bare_path,
            cfg.principal_header
        );
        return Flow::Break(
            Response::new(401)
                .with_headers(vec![("content-type".into(), "application/json".into())])
                .with_body(audit::render_error_body("utcp.unauthenticated")),
        );
    }

    // 3) Tool routing.
    let resolved = state.router.resolve(&method, &path);
    let principal = request.handler().header(&cfg.principal_header);

    let Some(resolved) = resolved else {
        match cfg.enforcement_mode {
            EnforcementMode::Strict => {
                logger::warn!(
                    "utcp-manual-validator: rejecting unmatched {} {} (strict)",
                    method,
                    bare_path
                );
                return Flow::Break(
                    Response::new(404)
                        .with_headers(vec![("content-type".into(), "application/json".into())])
                        .with_body(audit::render_error_body("utcp.tool_not_declared")),
                );
            }
            EnforcementMode::Permissive => {
                logger::info!(
                    "utcp-manual-validator: pass-through unmatched {} {} principal={:?}",
                    method,
                    bare_path,
                    principal
                );
                return Flow::Continue(());
            }
        }
    };

    let tool_name = resolved.tool_name.to_string();
    let tool_index = resolved.tool_index;

    // Tag the upstream request so downstream policies can scope per-tool.
    request.handler().remove_header(&cfg.tool_header_name);
    request.handler().set_header(&cfg.tool_header_name, &tool_name);

    if !cfg.validate_inputs {
        logger::info!(
            "utcp-manual-validator: matched tool='{}' (validation skipped) principal={:?}",
            tool_name,
            principal
        );
        return Flow::Continue(());
    }

    // 4) Schema validation. We only buffer a body when the matched tool
    //    actually has a body field; otherwise pure path/query/header
    //    validation suffices and we keep the request streaming.
    let tool = &state.manual.tools[tool_index];
    let needs_body = match &tool.tool_call_template {
        crate::manual::CallTemplate::Http(h) => h.body_field.is_some(),
    };

    let body_value = if needs_body && request.contains_body() {
        let body_state = request.into_body_state().await;
        let body = body_state.handler().body();
        if body.len() > cfg.max_body_bytes {
            logger::warn!(
                "utcp-manual-validator: body too large for tool='{}' ({} > {})",
                tool_name,
                body.len(),
                cfg.max_body_bytes
            );
            return Flow::Break(
                Response::new(413)
                    .with_headers(vec![("content-type".into(), "application/json".into())])
                    .with_body(audit::render_error_body("utcp.body_too_large")),
            );
        }
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => Some(v),
            Err(_) => {
                // Non-JSON body: leave as null; schema check will fail
                // type=object naturally if the schema requires it.
                Some(serde_json::Value::Null)
            }
        }
    } else {
        None
    };

    let synthetic = build_synthetic_value(&resolved, body_value);

    let violations = match &tool.inputs {
        Some(schema) => validate::validate_inputs(schema, &synthetic),
        None => Vec::new(),
    };

    if !violations.is_empty() {
        logger::warn!(
            "utcp-manual-validator: rejecting tool='{}' with {} violation(s) principal={:?}",
            tool_name,
            violations.len(),
            principal
        );
        return Flow::Break(
            Response::new(400)
                .with_headers(vec![("content-type".into(), "application/json".into())])
                .with_body(audit::render_violations_body(&tool_name, &violations)),
        );
    }

    logger::info!(
        "utcp-manual-validator: matched tool='{}' principal={:?}",
        tool_name,
        principal
    );
    Flow::Continue(())
}

/// Build the `{ <param>: ..., body: <body> }` shape the inputs JSON
/// Schema expects. Path params come from the router; query parameters
/// are flattened (single-value preserved as scalar, repeated keys as
/// arrays).
fn build_synthetic_value(
    resolved: &validate::ResolvedRoute<'_>,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    for (k, v) in &resolved.path_params {
        map.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    for (k, vs) in &resolved.query {
        let value = if vs.len() == 1 {
            serde_json::Value::String(vs[0].clone())
        } else {
            serde_json::Value::Array(vs.iter().cloned().map(serde_json::Value::String).collect())
        };
        // Don't clobber a path param that happens to share the name.
        map.entry(k.clone()).or_insert(value);
    }
    if let Some(b) = body {
        map.insert("body".into(), b);
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual::model::{CallTemplate, HttpCallTemplate, Manual, Tool};

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: String::new(),
            inputs: Some(serde_json::json!({
                "type":"object",
                "required":["id"],
                "properties":{"id":{"type":"string"}}
            })),
            outputs: None,
            tags: vec![],
            average_response_size: None,
            tool_call_template: CallTemplate::Http(HttpCallTemplate {
                url: "https://api.example.com/things/{id}".into(),
                http_method: "GET".into(),
                content_type: "application/json".into(),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn synthetic_value_includes_path_params() {
        let manual = Manual {
            manual_version: "1.0.0".into(),
            utcp_version: "1.0.1".into(),
            info: None,
            variables: None,
            tools: vec![tool("getThing")],
        };
        let router = validate::ToolRouter::build(&manual).unwrap();
        let resolved = router.resolve("GET", "/things/42").unwrap();
        let synth = build_synthetic_value(&resolved, None);
        assert_eq!(synth.get("id").and_then(|v| v.as_str()), Some("42"));
    }

    #[test]
    fn synthetic_value_includes_body() {
        let manual = Manual {
            manual_version: "1.0.0".into(),
            utcp_version: "1.0.1".into(),
            info: None,
            variables: None,
            tools: vec![tool("getThing")],
        };
        let router = validate::ToolRouter::build(&manual).unwrap();
        let resolved = router.resolve("GET", "/things/42").unwrap();
        let synth = build_synthetic_value(&resolved, Some(serde_json::json!({"x":1})));
        assert!(synth.get("body").is_some());
    }
}
