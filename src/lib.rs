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
//!         * No match -> 404 `utcp.tool_not_declared` (strict) or
//!           pass-through with audit (permissive).
//!         * Match -> validate the body against the tool's input schema
//!           (when `validateInputs=true`), tag the request with
//!           `<toolHeaderName>: <name>`, then forward the request to
//!           `<egressBaseUrl><tool.path>` via the PDK HttpClient and
//!           short-circuit (`Flow::Break`) with the upstream response.
//!
//! The bridge does not let Flex Gateway forward the request through its
//! own upstream cluster. Instead it issues an outbound call to
//! `egressBaseUrl` (typically the gateway's own hostname) so the bridge
//! can stack against sibling API instances on the same gateway.

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

use crate::config::PolicyConfig;
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

    let cfg = PolicyConfig::from_config(&raw)
        .map_err(|e| anyhow!("policy configuration rejected: {e}"))?;

    let state = ManualState::build(&cfg).map_err(|e| {
        anyhow!("utcp-manual-validator: failed to build Manual at policy load: {e}")
    })?;

    logger::info!(
        "utcp-manual-validator: loaded {} tool(s); discoveryPath='{}' apiInstanceProxyPath='{}' egress='{}' enforcement={:?} validateInputs={}",
        state.manual.tools.len(),
        cfg.discovery_path,
        cfg.api_instance_proxy_path,
        cfg.egress_service.uri().authority(),
        cfg.enforcement_mode,
        cfg.validate_inputs,
    );

    let cfg = Rc::new(cfg);
    let state = Rc::new(state);

    let request_cfg = cfg.clone();
    let request_state = state.clone();

    let filter = on_request(move |request: RequestHeadersState, client: HttpClient| {
        let cfg = request_cfg.clone();
        let state = request_state.clone();
        async move { request_filter(request, cfg, state, client).await }
    });

    launcher.launch(filter).await?;
    Ok(())
}

async fn request_filter(
    request: RequestHeadersState,
    cfg: Rc<PolicyConfig>,
    state: Rc<ManualState>,
    client: HttpClient,
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
        if cfg.enforcement_mode.is_strict() {
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
        }
        logger::info!(
            "utcp-manual-validator: passing through unmatched {} {} (utcp.unmatched=true)",
            method,
            bare_path
        );
        return Flow::Continue(());
    };

    let tool_name = resolved.tool_name.to_string();
    let tool_index = resolved.tool_index;
    let path_params = resolved.path_params.clone();
    let query = resolved.query.clone();

    let tool_entry = cfg.tools[tool_index].clone();
    let has_body = request.contains_body();

    // Capture forwardable headers BEFORE consuming `request` for body.
    let forward_headers = collect_forward_headers(&request, &cfg.tool_header_name, &tool_name);

    // 4) Read body (when present) and validate. We read regardless of
    //    schema presence so we can forward the body upstream.
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

    if cfg.validate_inputs && tool_entry.inputs.is_some() {
        let body_value = if body_bytes.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice::<serde_json::Value>(&body_bytes)
                    .unwrap_or(serde_json::Value::Null),
            )
        };
        let synthetic = build_synthetic_value(&path_params, &query, body_value);
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

    // 5) Forward to <egressBaseUrl><tool.path>?<query>.
    let outbound_path = build_outbound_path(&tool_entry.path_template, &path_params, &query);
    let header_refs: Vec<(&str, &str)> = forward_headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    logger::info!(
        "utcp-manual-validator: forwarding tool='{}' {} {} -> {}{} principal={:?}",
        tool_name,
        method,
        bare_path,
        cfg.egress_service.uri().authority(),
        outbound_path,
        principal
    );

    let response = client
        .request(&cfg.egress_service)
        .path(&outbound_path)
        .headers(header_refs)
        .timeout(cfg.outbound_timeout)
        .body(body_bytes.as_slice())
        .send(&method)
        .await;

    match response {
        Ok(upstream) => {
            let status = upstream.status_code();
            let resp_headers: Vec<(String, String)> = upstream
                .headers()
                .iter()
                .filter(|(k, _)| !is_hop_by_hop(k) && !is_pseudo_header(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let body = upstream.body().to_vec();
            Flow::Break(
                Response::new(status)
                    .with_headers(resp_headers)
                    .with_body(body),
            )
        }
        Err(e) => {
            logger::warn!(
                "utcp-manual-validator: upstream call failed tool='{}': {}",
                tool_name,
                e
            );
            Flow::Break(
                Response::new(502)
                    .with_headers(vec![("content-type".into(), "application/json".into())])
                    .with_body(audit::render_error_body("utcp.upstream_unavailable")),
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

/// Build the `{ <param>: ..., body: <body> }` shape the inputs JSON
/// Schema expects.
fn build_synthetic_value(
    path_params: &std::collections::HashMap<String, String>,
    query: &std::collections::HashMap<String, Vec<String>>,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    for (k, v) in path_params {
        map.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    for (k, vs) in query {
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

/// Substitute `{name}` segments in `tool.path_template` with the
/// resolved values, then re-attach the query string from the inbound
/// request.
fn build_outbound_path(
    template: &str,
    path_params: &std::collections::HashMap<String, String>,
    query: &std::collections::HashMap<String, Vec<String>>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = template[i..].find('}') {
                let key = &template[i + 1..i + end];
                if let Some(v) = path_params.get(key) {
                    out.push_str(&urlencode_path_segment(v));
                } else {
                    out.push_str(&template[i..i + end + 1]);
                }
                i += end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    if !query.is_empty() {
        let mut first = true;
        for (k, vs) in query {
            for v in vs {
                out.push(if first { '?' } else { '&' });
                first = false;
                out.push_str(&urlencode_query(k));
                out.push('=');
                out.push_str(&urlencode_query(v));
            }
        }
    }
    out
}

fn urlencode_path_segment(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

fn urlencode_query(s: &str) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

/// Collect inbound headers that should travel upstream. Drops
/// hop-by-hop headers (RFC 7230) and HTTP/2 pseudo-headers (`:path`,
/// `:authority`, ...) since those are populated by the request
/// builder. Adds the `<toolHeaderName>: <tool_name>` audit tag.
fn collect_forward_headers(
    request: &RequestHeadersState,
    tool_header_name: &str,
    tool_name: &str,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in request.handler().headers() {
        if is_hop_by_hop(&k) || is_pseudo_header(&k) {
            continue;
        }
        if k.eq_ignore_ascii_case(tool_header_name) {
            continue;
        }
        out.push((k, v));
    }
    out.push((tool_header_name.to_string(), tool_name.to_string()));
    out
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

fn is_pseudo_header(name: &str) -> bool {
    name.starts_with(':')
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
    fn synthetic_value_has_path_and_body() {
        let mut path_params = HashMap::new();
        path_params.insert("id".into(), "42".into());
        let synth = build_synthetic_value(
            &path_params,
            &HashMap::new(),
            Some(serde_json::json!({"k":"v"})),
        );
        assert_eq!(synth.get("id").and_then(|v| v.as_str()), Some("42"));
        assert!(synth.get("body").is_some());
    }

    #[test]
    fn outbound_path_substitutes_params_and_query() {
        let mut path_params = HashMap::new();
        path_params.insert("id".into(), "42".into());
        let mut query = HashMap::new();
        query.insert("q".into(), vec!["hello world".into()]);
        let p = build_outbound_path("/items/{id}", &path_params, &query);
        // exact query order is map-dependent, but must contain the encoded value
        assert!(p.starts_with("/items/42?"));
        assert!(p.contains("q=hello%20world"));
    }

    #[test]
    fn outbound_path_no_params_no_query() {
        let p = build_outbound_path("/api/order", &HashMap::new(), &HashMap::new());
        assert_eq!(p, "/api/order");
    }

    #[test]
    fn hop_by_hop_filter_drops_connection_and_host() {
        assert!(is_hop_by_hop("Connection"));
        assert!(is_hop_by_hop("host"));
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(!is_hop_by_hop("content-type"));
        assert!(is_pseudo_header(":path"));
        assert!(!is_pseudo_header("path"));
    }
}
