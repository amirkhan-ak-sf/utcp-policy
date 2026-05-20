//! UTCP Manual Validator policy entrypoint.
//!
//! Inbound request flow (`on_request`):
//!
//!   1. If the path (after stripping `apiInstanceProxyPath`) is
//!      `<discoveryPath>` and method is `GET`, short-circuit with the
//!      pre-rendered Manual JSON.
//!   2. If `requirePrincipal=true` and the configured principal header
//!      is absent, short-circuit with 401 `utcp.unauthenticated`.
//!   3. Otherwise resolve `(method, path)` against the compiled router.
//!         * No match -> 404 `utcp.tool_not_declared`.
//!         * Match -> validate the body against the tool's input schema
//!           (when `validateInputs=true`), then issue an outbound HTTP
//!           request to the matched upstream's PDK Service. The
//!           upstream response is returned to the caller via
//!           `Flow::Break`.
//!
//! Header forwarding: every inbound header except a small allow-deny
//! list of hop-by-hop / proxy-internal headers is copied to the
//! outbound request, including `Authorization` and any custom auth
//! headers the agent supplies. The policy itself never holds upstream
//! credentials.

mod audit;
mod config;
mod generated;
pub mod manual;
mod manual_state;
pub mod validate;

use std::rc::Rc;
use std::time::Duration;

use anyhow::anyhow;
use pdk::cache::CacheBuilder;
use pdk::hl::*;
use pdk::logger;

use crate::config::{PolicyConfig, ToolEntry};
use crate::generated::config::Config;
use crate::manual_state::ManualState;

/// Hop-by-hop / proxy-internal headers we never forward upstream.
/// Excludes `Authorization` (forwarded) and the `:pseudo-headers`
/// (handled separately by the HttpClient).
const SKIP_HEADERS: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "content-length",
    "upgrade",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
];

#[entrypoint]
pub async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    _cache_builder: CacheBuilder,
) -> anyhow::Result<()> {
    let raw: Config = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow!("invalid policy configuration: {e}"))?;

    let cfg = PolicyConfig::from_config(&raw)
        .map_err(|e| anyhow!("policy configuration rejected: {e}"))?;

    let state = ManualState::build(&cfg).map_err(|e| {
        anyhow!("utcp-manual-validator: failed to build Manual at policy load: {e}")
    })?;

    // Hold the registered Service handles for outbound dispatch. Index
    // matches `cfg.upstream_urls` and `ToolEntry.upstream_index`.
    let services: Vec<pdk::hl::Service> = raw
        .upstreams
        .iter()
        .map(|u| u.host.clone())
        .collect();

    logger::info!(
        "utcp-manual-validator: loaded {} upstream(s) / {} tool(s); discoveryPath='{}' apiInstanceProxyPath='{}' enforcement={:?} validateInputs={}",
        services.len(),
        state.manual.tools.len(),
        cfg.discovery_path,
        cfg.api_instance_proxy_path,
        cfg.enforcement_mode,
        cfg.validate_inputs,
    );

    let cfg = Rc::new(cfg);
    let state = Rc::new(state);
    let services = Rc::new(services);

    let request_cfg = cfg.clone();
    let request_state = state.clone();
    let request_services = services.clone();

    let filter = on_request(move |request: RequestHeadersState, client: HttpClient| {
        let cfg = request_cfg.clone();
        let state = request_state.clone();
        let services = request_services.clone();
        async move { request_filter(request, client, cfg, state, services).await }
    });

    launcher.launch(filter).await?;
    Ok(())
}

async fn request_filter(
    request: RequestHeadersState,
    client: HttpClient,
    cfg: Rc<PolicyConfig>,
    state: Rc<ManualState>,
    services: Rc<Vec<pdk::hl::Service>>,
) -> Flow<()> {
    let method = request.method();
    let raw_path = request.path();

    // 1) Discovery short-circuit. Compare against the apiInstance-stripped
    //    path so the Manual is reachable at <proxy>/<discoveryPath>.
    let bare_path = raw_path.split_once('?').map(|(p, _)| p).unwrap_or(&raw_path);
    let local_path = strip_proxy_prefix(bare_path, &cfg.api_instance_proxy_path);

    if method.eq_ignore_ascii_case("GET") && local_path == cfg.discovery_path {
        logger::debug!(
            "utcp-manual-validator: serving manual on {}",
            cfg.discovery_path
        );
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

    // 3) Tool routing. Build a match path with query string preserved
    //    so the router can populate `query` for synthetic validation.
    let local_path_with_query = match raw_path.split_once('?') {
        Some((_, q)) => format!("{local_path}?{q}"),
        None => local_path.to_string(),
    };
    let principal = request.handler().header(&cfg.principal_header);

    let resolved = state.router.resolve(&method, &local_path_with_query);
    let Some(resolved) = resolved else {
        logger::warn!(
            "utcp-manual-validator: rejecting unmatched {} {} (local '{}')",
            method,
            bare_path,
            local_path
        );
        return Flow::Break(
            Response::new(404)
                .with_headers(vec![("content-type".into(), "application/json".into())])
                .with_body(audit::render_error_body("utcp.tool_not_declared")),
        );
    };

    let tool_name = resolved.tool_name.to_string();
    let tool_index = resolved.tool_index;
    let path_params = resolved.path_params.clone();

    request.handler().remove_header(&cfg.tool_header_name);
    request.handler().set_header(&cfg.tool_header_name, &tool_name);

    // 4) Capture inbound headers up-front; `into_body_state` drops
    //    access to the headers handler.
    let inbound_headers: Vec<(String, String)> = request.handler().headers();
    let has_body = request.contains_body();
    let tool_entry: ToolEntry = cfg.tools[tool_index].clone();

    let body_bytes: Vec<u8> = if has_body {
        let body_state = request.into_body_state().await;
        let bytes = body_state.handler().body();
        if bytes.len() > cfg.max_body_bytes {
            logger::warn!(
                "utcp-manual-validator: body too large for tool='{}' ({} > {})",
                tool_name,
                bytes.len(),
                cfg.max_body_bytes
            );
            return Flow::Break(
                Response::new(413)
                    .with_headers(vec![("content-type".into(), "application/json".into())])
                    .with_body(audit::render_error_body("utcp.body_too_large")),
            );
        }
        bytes
    } else {
        Vec::new()
    };

    // 5) Schema validation against the synthetic
    //    `{path_params..., query..., body: ...}` value.
    if cfg.validate_inputs {
        let body_value = if body_bytes.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice::<serde_json::Value>(&body_bytes)
                    .unwrap_or(serde_json::Value::Null),
            )
        };
        let synthetic = build_synthetic_value(&resolved, body_value);
        if let Some(schema) = &tool_entry.inputs {
            let violations = validate::validate_inputs(schema, &synthetic);
            if !violations.is_empty() {
                logger::warn!(
                    "utcp-manual-validator: rejecting tool='{}' with {} violation(s) principal={:?}",
                    tool_name,
                    violations.len(),
                    principal
                );
                return Flow::Break(
                    Response::new(400)
                        .with_headers(vec![(
                            "content-type".into(),
                            "application/json".into(),
                        )])
                        .with_body(audit::render_violations_body(&tool_name, &violations)),
                );
            }
        }
    }

    // 6) Compose outbound URL: resolve {path} placeholders, then
    //    re-attach the inbound query string.
    let outbound_path = resolve_path_template(&tool_entry.path_template, &path_params);
    let outbound_path = match raw_path.split_once('?') {
        Some((_, q)) if !q.is_empty() => format!("{outbound_path}?{q}"),
        _ => outbound_path,
    };

    // 7) Build forwarded headers from the inbound request (captured
    //    pre-body-state above).
    let mut fwd_headers: Vec<(String, String)> = Vec::with_capacity(inbound_headers.len() + 2);
    for (k, v) in &inbound_headers {
        let lower = k.to_ascii_lowercase();
        if lower.starts_with(':') {
            // proxy-wasm pseudo-headers; HttpClient writes its own.
            continue;
        }
        if SKIP_HEADERS.iter().any(|h| *h == lower) {
            continue;
        }
        fwd_headers.push((k.clone(), v.clone()));
    }
    // Override / set content-type for the outbound call.
    fwd_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
    fwd_headers.push(("content-type".into(), tool_entry.content_type.clone()));
    fwd_headers.push((cfg.tool_header_name.clone(), tool_name.clone()));

    let header_refs: Vec<(&str, &str)> = fwd_headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let body_slice: &[u8] = &body_bytes;

    let service = match services.get(tool_entry.upstream_index) {
        Some(s) => s,
        None => {
            logger::error!(
                "utcp-manual-validator: tool '{}' references upstream {} but no Service registered",
                tool_name,
                tool_entry.upstream_index
            );
            return Flow::Break(
                Response::new(500)
                    .with_headers(vec![("content-type".into(), "application/json".into())])
                    .with_body(audit::render_error_body("utcp.upstream_misconfigured")),
            );
        }
    };

    logger::info!(
        "utcp-manual-validator: dispatching tool='{}' -> {} {}{} principal={:?}",
        tool_name,
        tool_entry.method,
        tool_entry.upstream_url,
        outbound_path,
        principal
    );

    // 8) Issue outbound HTTP. We use `send(method)` so any HTTP method
    //    string the operator configured (GET/POST/PUT/PATCH/DELETE/...)
    //    flows through unchanged.
    let response_result = client
        .request(service)
        .path(&outbound_path)
        .headers(header_refs)
        .body(body_slice)
        .timeout(Duration::from_secs(cfg.outbound_timeout_seconds as u64))
        .send(&tool_entry.method)
        .await;

    match response_result {
        Ok(resp) => {
            let status = resp.status_code();
            let mut out_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .filter(|(k, _)| {
                    let lower = k.to_ascii_lowercase();
                    !lower.starts_with(':')
                        && !SKIP_HEADERS.iter().any(|h| *h == lower)
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            // Preserve the matched tool tag on the way back.
            out_headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&cfg.tool_header_name));
            out_headers.push((cfg.tool_header_name.clone(), tool_name.clone()));
            let body = resp.body().to_vec();
            Flow::Break(Response::new(status as u32).with_headers(out_headers).with_body(body))
        }
        Err(e) => {
            logger::warn!(
                "utcp-manual-validator: upstream call failed for tool='{}': {e}",
                tool_name
            );
            Flow::Break(
                Response::new(504)
                    .with_headers(vec![("content-type".into(), "application/json".into())])
                    .with_body(audit::render_error_body("utcp.upstream_timeout")),
            )
        }
    }
}

/// Strip `prefix` from `path` if present; otherwise return path
/// unchanged. Both inputs are normalized so the empty prefix is a
/// no-op.
fn strip_proxy_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        return path;
    }
    if let Some(rest) = path.strip_prefix(prefix) {
        if rest.is_empty() {
            "/"
        } else if rest.starts_with('/') {
            rest
        } else {
            // The prefix matched mid-segment ("/sap-foo" vs "/sap-foobar");
            // don't strip in that case.
            path
        }
    } else {
        path
    }
}

/// Replace `{name}` placeholders in `template` with values from
/// `params`. Unknown placeholders are left as-is so they're visible in
/// upstream logs.
fn resolve_path_template(template: &str, params: &std::collections::HashMap<String, String>) -> String {
    if !template.contains('{') {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find('}') {
            let name = &rest[start + 1..start + end];
            if let Some(v) = params.get(name) {
                out.push_str(v);
            } else {
                out.push_str(&rest[start..start + end + 1]);
            }
            rest = &rest[start + end + 1..];
        } else {
            out.push_str(&rest[start..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Build the `{ <param>: ..., body: <body> }` shape the inputs JSON
/// Schema expects.
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
    use std::collections::HashMap;

    #[test]
    fn strip_proxy_prefix_strips_when_match() {
        assert_eq!(strip_proxy_prefix("/sap/api", "/sap"), "/api");
        assert_eq!(strip_proxy_prefix("/sap", "/sap"), "/");
        assert_eq!(strip_proxy_prefix("/sapfoo/api", "/sap"), "/sapfoo/api");
        assert_eq!(strip_proxy_prefix("/other/api", "/sap"), "/other/api");
        assert_eq!(strip_proxy_prefix("/api", ""), "/api");
    }

    #[test]
    fn resolve_path_template_substitutes() {
        let mut p = HashMap::new();
        p.insert("id".to_string(), "42".to_string());
        assert_eq!(resolve_path_template("/users/{id}", &p), "/users/42");
        assert_eq!(resolve_path_template("/users", &p), "/users");
        assert_eq!(
            resolve_path_template("/users/{missing}/bar", &p),
            "/users/{missing}/bar"
        );
    }

    #[test]
    fn synthetic_value_has_path_and_body() {
        let mut path_params = HashMap::new();
        path_params.insert("id".into(), "42".into());
        let resolved = validate::ResolvedRoute {
            tool_index: 0,
            tool_name: "x",
            path_params,
            query: HashMap::new(),
        };
        let synth = build_synthetic_value(&resolved, Some(serde_json::json!({"k":"v"})));
        assert_eq!(synth.get("id").and_then(|v| v.as_str()), Some("42"));
        assert!(synth.get("body").is_some());
    }
}
