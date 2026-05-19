//! OpenAPI 3.x -> UTCP Manual conversion.
//!
//! Mirrors the documented mapping rules of UTCP's `OpenApiConverter`:
//!
//!   * Each `paths.<path>.<method>` becomes one tool. `name` defaults to
//!     `operationId`, falling back to `<METHOD>_<slug-of-path>` when absent.
//!   * Path parameters become `{name}` placeholders in the tool URL.
//!   * Query, header, cookie parameters and `requestBody` properties are
//!     merged into the tool's `inputs` JSON Schema. Header parameters
//!     additionally land in `header_fields`. The body is exposed under a
//!     single property whose name comes from `tool_name_prefix` config —
//!     by default `"body"` — and that name is set as `body_field`.
//!   * `securitySchemes` translate to UTCP `auth` blocks. Secrets are
//!     emitted as `${ENV_VAR}` placeholders, never as literals.
//!   * Local `$ref` resolution only (no remote refs in v1).
//!
//! YAML and JSON are both accepted at the input layer; this module
//! takes a `serde_json::Value` so the caller picks the parser.

use std::collections::{BTreeSet, HashSet};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::manual::model::{CallTemplate, HttpCallTemplate, Manual, Tool};

const SUPPORTED_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "options"];
const DEFAULT_BODY_FIELD: &str = "body";

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("OpenAPI document is not an object")]
    NotAnObject,
    #[error("OpenAPI document has no `paths` object")]
    NoPaths,
    #[error("unable to resolve $ref '{0}'")]
    BadRef(String),
}

pub struct ConvertOptions<'a> {
    pub utcp_version: &'a str,
    pub tool_name_prefix: &'a str,
}

pub fn convert(spec: &Value, opts: &ConvertOptions) -> Result<Manual, ConvertError> {
    let root = spec.as_object().ok_or(ConvertError::NotAnObject)?;
    let paths = root
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(ConvertError::NoPaths)?;

    let info = root.get("info").cloned();
    let base_url = pick_base_url(root);
    let security_schemes = root
        .get("components")
        .and_then(|c| c.get("securitySchemes"))
        .cloned()
        .unwrap_or(Value::Null);

    // Top-level security applies when an operation has no `security`.
    let global_security = root.get("security").cloned();

    let mut tools = Vec::with_capacity(paths.len() * 2);
    let mut name_seen: HashSet<String> = HashSet::new();

    let mut path_keys: Vec<&String> = paths.keys().collect();
    path_keys.sort();

    for path in path_keys {
        let item = match paths.get(path).and_then(Value::as_object) {
            Some(o) => o,
            None => continue,
        };

        let mut method_keys: Vec<&String> = item
            .keys()
            .filter(|k| SUPPORTED_METHODS.contains(&k.to_lowercase().as_str()))
            .collect();
        method_keys.sort();

        // Path-level parameters apply to every operation under this path.
        let path_level_params = item
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for method in method_keys {
            let op = match item.get(method).and_then(Value::as_object) {
                Some(o) => o,
                None => continue,
            };

            let raw_name = op
                .get("operationId")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| synth_name(method, path));

            let mut name = format!("{}{}", opts.tool_name_prefix, raw_name);
            // operationId is *supposed* to be unique but we still defend
            // against duplicates from sloppy specs.
            if !name_seen.insert(name.clone()) {
                let mut suffix = 2usize;
                loop {
                    let candidate = format!("{name}_{suffix}");
                    if name_seen.insert(candidate.clone()) {
                        name = candidate;
                        break;
                    }
                    suffix += 1;
                }
            }

            let description = op
                .get("description")
                .or_else(|| op.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let tags = op
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let mut combined_params = path_level_params.clone();
            if let Some(arr) = op.get("parameters").and_then(Value::as_array) {
                combined_params.extend(arr.iter().cloned());
            }

            let resolved_params = resolve_parameters(spec, &combined_params)?;

            let request_body = op
                .get("requestBody")
                .map(|rb| resolve_ref(spec, rb))
                .transpose()?;

            let body_field = if request_body.is_some() {
                Some(DEFAULT_BODY_FIELD.to_string())
            } else {
                None
            };

            let inputs = build_inputs(&resolved_params, request_body.as_ref(), &body_field);
            let outputs = build_outputs(spec, op);
            let header_fields = collect_header_fields(&resolved_params);

            let url = format!("{}{}", base_url.as_deref().unwrap_or(""), path);

            let auth = pick_auth(
                op.get("security").or(global_security.as_ref()),
                &security_schemes,
            );

            let template = HttpCallTemplate {
                url,
                http_method: method.to_uppercase(),
                content_type: "application/json".into(),
                headers: Map::new(),
                header_fields,
                body_field,
                auth,
            };

            tools.push(Tool {
                name,
                description,
                inputs,
                outputs,
                tags,
                average_response_size: None,
                tool_call_template: CallTemplate::Http(template),
            });
        }
    }

    Ok(Manual {
        manual_version: "1.0.0".into(),
        utcp_version: opts.utcp_version.to_string(),
        info,
        variables: None,
        tools,
    })
}

/// `servers[0].url` if present, else empty (URL becomes path-relative).
fn pick_base_url(root: &Map<String, Value>) -> Option<String> {
    let arr = root.get("servers").and_then(Value::as_array)?;
    let first = arr.first()?;
    let url = first.get("url").and_then(Value::as_str)?.to_string();
    if url.is_empty() {
        None
    } else {
        // strip trailing slash so concat with leading-slash paths is clean
        Some(url.trim_end_matches('/').to_string())
    }
}

/// Resolve a parameter array, expanding any `$ref` entries against
/// `components.parameters` in the same document.
fn resolve_parameters(spec: &Value, raw: &[Value]) -> Result<Vec<Value>, ConvertError> {
    raw.iter().map(|v| resolve_ref(spec, v)).collect()
}

/// Resolve a single (possibly-`$ref`) value. Local refs only.
fn resolve_ref(spec: &Value, v: &Value) -> Result<Value, ConvertError> {
    let Some(obj) = v.as_object() else { return Ok(v.clone()) };
    let Some(ref_str) = obj.get("$ref").and_then(Value::as_str) else {
        return Ok(v.clone());
    };

    let pointer = ref_str
        .strip_prefix("#")
        .ok_or_else(|| ConvertError::BadRef(ref_str.into()))?;
    let resolved = spec
        .pointer(pointer)
        .ok_or_else(|| ConvertError::BadRef(ref_str.into()))?;
    Ok(resolved.clone())
}

fn synth_name(method: &str, path: &str) -> String {
    let mut slug = String::with_capacity(path.len());
    let mut prev_underscore = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_underscore = false;
        } else if !prev_underscore {
            slug.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = slug.trim_matches('_');
    format!("{}_{}", method.to_uppercase(), trimmed)
}

fn build_inputs(params: &[Value], request_body: Option<&Value>, body_field: &Option<String>) -> Option<Value> {
    let mut props = Map::new();
    let mut required = BTreeSet::new();

    for p in params {
        let Some(o) = p.as_object() else { continue };
        let name = match o.get("name").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let mut schema = o
            .get("schema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "string"}));
        if let Some(desc) = o.get("description").and_then(Value::as_str) {
            if let Some(map) = schema.as_object_mut() {
                map.entry("description".to_string())
                    .or_insert_with(|| Value::String(desc.into()));
            }
        }
        if o.get("required").and_then(Value::as_bool).unwrap_or(false) {
            required.insert(name.clone());
        }
        // Per OpenAPI 3, path parameters are always required.
        if o.get("in").and_then(Value::as_str) == Some("path") {
            required.insert(name.clone());
        }
        props.insert(name, schema);
    }

    if let (Some(field), Some(rb)) = (body_field.as_ref(), request_body) {
        let body_schema = pick_request_body_schema(rb)
            .unwrap_or_else(|| json!({"type": "object", "additionalProperties": true}));
        props.insert(field.clone(), body_schema);
        if rb.get("required").and_then(Value::as_bool).unwrap_or(false) {
            required.insert(field.clone());
        }
    }

    if props.is_empty() && required.is_empty() {
        return None;
    }

    let required_arr: Vec<Value> = required.into_iter().map(Value::String).collect();
    let mut out = Map::new();
    out.insert("type".into(), Value::String("object".into()));
    out.insert("properties".into(), Value::Object(props));
    if !required_arr.is_empty() {
        out.insert("required".into(), Value::Array(required_arr));
    }
    Some(Value::Object(out))
}

/// Lift the JSON request-body schema, preferring `application/json`.
fn pick_request_body_schema(rb: &Value) -> Option<Value> {
    let content = rb.get("content")?.as_object()?;
    if let Some(json_ct) = content.get("application/json") {
        if let Some(s) = json_ct.get("schema") {
            return Some(s.clone());
        }
    }
    // Fall back to any other content type.
    for (_, v) in content {
        if let Some(s) = v.get("schema") {
            return Some(s.clone());
        }
    }
    None
}

fn build_outputs(spec: &Value, op: &Map<String, Value>) -> Option<Value> {
    let responses = op.get("responses")?.as_object()?;
    // Prefer a 2xx status, defaulting to "default" then "200".
    let mut keys: Vec<&String> = responses.keys().collect();
    keys.sort();
    let pick = keys
        .iter()
        .find(|k| k.starts_with('2'))
        .or_else(|| keys.iter().find(|k| k.as_str() == "default"))
        .or_else(|| keys.first())?;
    let response = responses.get(pick.as_str())?;
    let resolved = resolve_ref(spec, response).ok()?;
    let content = resolved.get("content")?.as_object()?;
    if let Some(json_ct) = content.get("application/json") {
        if let Some(s) = json_ct.get("schema") {
            return Some(s.clone());
        }
    }
    None
}

fn collect_header_fields(params: &[Value]) -> Vec<String> {
    let mut headers: Vec<String> = params
        .iter()
        .filter_map(|p| {
            let o = p.as_object()?;
            if o.get("in")?.as_str()? == "header" {
                o.get("name")?.as_str().map(String::from)
            } else {
                None
            }
        })
        .collect();
    headers.sort();
    headers.dedup();
    headers
}

/// Map an OpenAPI security requirement onto a UTCP `auth` block. Picks
/// the first scheme whose definition we can translate.
fn pick_auth(security: Option<&Value>, schemes: &Value) -> Option<Value> {
    let arr = security?.as_array()?;
    let schemes_obj = schemes.as_object()?;
    for req in arr {
        let req_obj = req.as_object()?;
        for (scheme_name, _scopes) in req_obj {
            let scheme = match schemes_obj.get(scheme_name) {
                Some(s) => s,
                None => continue,
            };
            if let Some(auth) = translate_scheme(scheme_name, scheme) {
                return Some(auth);
            }
        }
    }
    None
}

fn translate_scheme(scheme_name: &str, scheme: &Value) -> Option<Value> {
    let obj = scheme.as_object()?;
    let kind = obj.get("type")?.as_str()?;
    let env = scheme_to_env_var(scheme_name);
    match kind {
        "apiKey" => {
            let var_name = obj.get("name")?.as_str()?.to_string();
            let location = obj
                .get("in")
                .and_then(Value::as_str)
                .unwrap_or("header")
                .to_string();
            Some(json!({
                "auth_type": "api_key",
                "api_key": format!("${{{env}}}"),
                "var_name": var_name,
                "location": location
            }))
        }
        "http" => {
            let scheme_kind = obj.get("scheme").and_then(Value::as_str).unwrap_or("");
            match scheme_kind {
                "bearer" => Some(json!({
                    "auth_type": "api_key",
                    "api_key": format!("Bearer ${{{env}}}"),
                    "var_name": "Authorization",
                    "location": "header"
                })),
                "basic" => Some(json!({
                    "auth_type": "basic",
                    "username": format!("${{{env}_USER}}"),
                    "password": format!("${{{env}_PASS}}")
                })),
                _ => None,
            }
        }
        "oauth2" => {
            let flows = obj.get("flows")?.as_object()?;
            let cc = flows.get("clientCredentials")?.as_object()?;
            let token_url = cc.get("tokenUrl")?.as_str()?.to_string();
            let scope = cc
                .get("scopes")
                .and_then(Value::as_object)
                .map(|m| {
                    let mut keys: Vec<&String> = m.keys().collect();
                    keys.sort();
                    keys.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")
                })
                .unwrap_or_default();
            Some(json!({
                "auth_type": "oauth2",
                "client_id": format!("${{{env}_CLIENT_ID}}"),
                "client_secret": format!("${{{env}_CLIENT_SECRET}}"),
                "token_url": token_url,
                "scope": scope
            }))
        }
        _ => None,
    }
}

/// Derive an upper-case env-var name from the scheme key ("apiKey" ->
/// "APIKEY"). Operators are expected to wire the actual secret on the
/// agent side; this just picks a stable placeholder name.
fn scheme_to_env_var(scheme_name: &str) -> String {
    let mut out = String::with_capacity(scheme_name.len());
    for c in scheme_name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ConvertOptions<'static> {
        ConvertOptions {
            utcp_version: "1.0.1",
            tool_name_prefix: "",
        }
    }

    #[test]
    fn rejects_non_object() {
        let v = serde_json::json!([]);
        assert!(matches!(convert(&v, &opts()).unwrap_err(), ConvertError::NotAnObject));
    }

    #[test]
    fn rejects_missing_paths() {
        let v = serde_json::json!({"openapi":"3.0.0","info":{}});
        assert!(matches!(convert(&v, &opts()).unwrap_err(), ConvertError::NoPaths));
    }

    #[test]
    fn converts_simple_get() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "servers": [{"url": "https://api.example.com"}],
            "paths": {
                "/users/{userId}": {
                    "get": {
                        "operationId": "getUser",
                        "summary": "fetch a user",
                        "parameters": [
                            {"name":"userId","in":"path","required":true,"schema":{"type":"string"}},
                            {"name":"verbose","in":"query","schema":{"type":"boolean"}}
                        ],
                        "responses": {"200": {"description":"ok"}}
                    }
                }
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        assert_eq!(manual.tools.len(), 1);
        assert_eq!(manual.tools[0].name, "getUser");
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => {
                assert_eq!(http.url, "https://api.example.com/users/{userId}");
                assert_eq!(http.http_method, "GET");
                assert!(http.body_field.is_none());
            }
        }
        let inputs = manual.tools[0].inputs.as_ref().unwrap();
        let props = inputs.get("properties").unwrap();
        assert!(props.get("userId").is_some());
        assert!(props.get("verbose").is_some());
        let req = inputs.get("required").unwrap().as_array().unwrap();
        assert!(req.iter().any(|v| v == "userId"));
    }

    #[test]
    fn converts_post_with_body() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "paths": {
                "/things": {
                    "post": {
                        "operationId": "createThing",
                        "requestBody": {
                            "required": true,
                            "content": {
                                "application/json": {
                                    "schema": {"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}
                                }
                            }
                        },
                        "responses": {"201": {"description":"ok"}}
                    }
                }
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => {
                assert_eq!(http.body_field.as_deref(), Some("body"));
            }
        }
        let inputs = manual.tools[0].inputs.as_ref().unwrap();
        let req = inputs.get("required").unwrap().as_array().unwrap();
        assert!(req.iter().any(|v| v == "body"));
    }

    #[test]
    fn collects_header_fields() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "paths": {
                "/x": {
                    "get": {
                        "operationId": "getX",
                        "parameters": [
                            {"name":"X-Trace","in":"header","schema":{"type":"string"}}
                        ],
                        "responses": {"200": {"description":"ok"}}
                    }
                }
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => {
                assert_eq!(http.header_fields, vec!["X-Trace"]);
            }
        }
    }

    #[test]
    fn translates_apikey_security() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "components": {
                "securitySchemes": {
                    "apiKeyAuth": {"type":"apiKey","in":"header","name":"X-API-Key"}
                }
            },
            "security": [{"apiKeyAuth": []}],
            "paths": {
                "/x": {"get":{"operationId":"getX","responses":{"200":{"description":"ok"}}}}
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => {
                let auth = http.auth.as_ref().unwrap();
                assert_eq!(auth.get("auth_type").unwrap(), "api_key");
                assert_eq!(auth.get("var_name").unwrap(), "X-API-Key");
                assert_eq!(auth.get("location").unwrap(), "header");
                assert_eq!(auth.get("api_key").unwrap(), "${APIKEYAUTH}");
            }
        }
    }

    #[test]
    fn translates_bearer_security() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "components": {
                "securitySchemes": {
                    "bearer": {"type":"http","scheme":"bearer"}
                }
            },
            "security": [{"bearer": []}],
            "paths": {
                "/x": {"get":{"operationId":"getX","responses":{"200":{"description":"ok"}}}}
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => {
                let auth = http.auth.as_ref().unwrap();
                assert_eq!(auth.get("api_key").unwrap(), "Bearer ${BEARER}");
                assert_eq!(auth.get("var_name").unwrap(), "Authorization");
            }
        }
    }

    #[test]
    fn synthesises_tool_name_when_no_operation_id() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "paths": {
                "/users/{id}/posts": {"get":{"responses":{"200":{"description":"ok"}}}}
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        assert_eq!(manual.tools[0].name, "GET_users_id_posts");
    }

    #[test]
    fn deduplicates_repeated_operation_ids() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "paths": {
                "/a": {"get":{"operationId":"x","responses":{"200":{"description":"ok"}}}},
                "/b": {"get":{"operationId":"x","responses":{"200":{"description":"ok"}}}}
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        let names: Vec<&str> = manual.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"x_2"));
    }

    #[test]
    fn resolves_local_ref_for_parameter() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "components": {
                "parameters": {
                    "TraceId": {"name":"X-Trace","in":"header","schema":{"type":"string"}}
                }
            },
            "paths": {
                "/x": {
                    "get": {
                        "operationId": "getX",
                        "parameters": [{"$ref":"#/components/parameters/TraceId"}],
                        "responses": {"200":{"description":"ok"}}
                    }
                }
            }
        });
        let manual = convert(&v, &opts()).unwrap();
        match &manual.tools[0].tool_call_template {
            CallTemplate::Http(http) => assert_eq!(http.header_fields, vec!["X-Trace"]),
        }
    }

    #[test]
    fn applies_tool_name_prefix() {
        let v = serde_json::json!({
            "openapi": "3.0.0",
            "info": {"title":"x","version":"1"},
            "paths": {"/x":{"get":{"operationId":"getX","responses":{"200":{"description":"ok"}}}}}
        });
        let manual = convert(
            &v,
            &ConvertOptions {
                utcp_version: "1.0.1",
                tool_name_prefix: "crm.",
            },
        )
        .unwrap();
        assert_eq!(manual.tools[0].name, "crm.getX");
    }
}
