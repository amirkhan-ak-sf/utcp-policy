//! Targeted JSON Schema subset validator.
//!
//! We deliberately do not pull in the full `jsonschema` crate: at the time
//! of writing, its `wasm32-wasip1` story (regex backend, format
//! validators) is unverified and the policy only needs the keyword
//! subset that `OpenApiConverter` actually emits. See
//! [[utcp-policy-roadmap]] for the longer-term plan to swap in a full
//! 2020-12 validator once a WASM-clean crate is selected.
//!
//! Supported keywords:
//!   * type: string|integer|number|boolean|object|array|null (single
//!     type or array-of-types)
//!   * required: list of property names (object-only)
//!   * properties: per-property schemas (object-only)
//!   * additionalProperties: bool or schema (object-only)
//!   * items: schema (array-only)
//!   * minLength / maxLength: string-only
//!   * minimum / maximum / exclusiveMinimum / exclusiveMaximum:
//!     integer/number-only
//!   * minItems / maxItems: array-only
//!   * enum: any
//!   * pattern: string regex (regex crate; anchored or not, per spec
//!     — we run `Regex::find` so unanchored patterns match per JSON
//!     Schema semantics)
//!
//! Anything else is silently ignored (treated as "no constraint"),
//! which matches the spec's "unknown keyword" rule.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

pub fn validate_inputs(schema: &Value, value: &Value) -> Vec<Violation> {
    let mut out = Vec::new();
    validate_at(schema, value, "", &mut out);
    out
}

fn validate_at(schema: &Value, value: &Value, path: &str, out: &mut Vec<Violation>) {
    let Some(obj) = schema.as_object() else { return };

    // type
    if let Some(t) = obj.get("type") {
        if !matches_type(t, value) {
            out.push(Violation {
                path: path_or_root(path),
                message: format!("expected type {}", render_type(t)),
            });
            // Once the type is wrong, further checks would just produce
            // noise.
            return;
        }
    }

    // enum
    if let Some(en) = obj.get("enum").and_then(Value::as_array) {
        if !en.iter().any(|v| v == value) {
            out.push(Violation {
                path: path_or_root(path),
                message: "value is not in enum".into(),
            });
        }
    }

    match value {
        Value::Object(map) => {
            // required
            if let Some(req) = obj.get("required").and_then(Value::as_array) {
                for r in req {
                    if let Some(name) = r.as_str() {
                        if !map.contains_key(name) {
                            out.push(Violation {
                                path: format!("{path}/{name}"),
                                message: "required property is missing".into(),
                            });
                        }
                    }
                }
            }
            // properties
            if let Some(props) = obj.get("properties").and_then(Value::as_object) {
                for (k, v) in map {
                    if let Some(child) = props.get(k) {
                        validate_at(child, v, &format!("{path}/{k}"), out);
                    } else if let Some(ap) = obj.get("additionalProperties") {
                        match ap {
                            Value::Bool(false) => {
                                out.push(Violation {
                                    path: format!("{path}/{k}"),
                                    message: "additional property not allowed".into(),
                                });
                            }
                            Value::Object(_) => {
                                validate_at(ap, v, &format!("{path}/{k}"), out);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Value::Array(arr) => {
            if let Some(items) = obj.get("items") {
                for (i, v) in arr.iter().enumerate() {
                    validate_at(items, v, &format!("{path}/{i}"), out);
                }
            }
            if let Some(n) = obj.get("minItems").and_then(Value::as_u64) {
                if (arr.len() as u64) < n {
                    out.push(Violation {
                        path: path_or_root(path),
                        message: format!("array must have at least {n} item(s)"),
                    });
                }
            }
            if let Some(n) = obj.get("maxItems").and_then(Value::as_u64) {
                if (arr.len() as u64) > n {
                    out.push(Violation {
                        path: path_or_root(path),
                        message: format!("array must have at most {n} item(s)"),
                    });
                }
            }
        }
        Value::String(s) => {
            if let Some(n) = obj.get("minLength").and_then(Value::as_u64) {
                if (s.chars().count() as u64) < n {
                    out.push(Violation {
                        path: path_or_root(path),
                        message: format!("string must have at least {n} character(s)"),
                    });
                }
            }
            if let Some(n) = obj.get("maxLength").and_then(Value::as_u64) {
                if (s.chars().count() as u64) > n {
                    out.push(Violation {
                        path: path_or_root(path),
                        message: format!("string must have at most {n} character(s)"),
                    });
                }
            }
            if let Some(p) = obj.get("pattern").and_then(Value::as_str) {
                match regex::Regex::new(p) {
                    Ok(re) => {
                        if !re.is_match(s) {
                            out.push(Violation {
                                path: path_or_root(path),
                                message: format!("string must match pattern '{p}'"),
                            });
                        }
                    }
                    Err(_) => {
                        // Unparseable pattern: treat as no constraint and
                        // surface a meta-violation so operators see why.
                        out.push(Violation {
                            path: path_or_root(path),
                            message: format!("schema pattern '{p}' did not compile; skipped"),
                        });
                    }
                }
            }
        }
        Value::Number(n) => {
            if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
                if let Some(v) = n.as_f64() {
                    if v < min {
                        out.push(Violation {
                            path: path_or_root(path),
                            message: format!("must be >= {min}"),
                        });
                    }
                }
            }
            if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
                if let Some(v) = n.as_f64() {
                    if v > max {
                        out.push(Violation {
                            path: path_or_root(path),
                            message: format!("must be <= {max}"),
                        });
                    }
                }
            }
            if let Some(min) = obj.get("exclusiveMinimum").and_then(Value::as_f64) {
                if let Some(v) = n.as_f64() {
                    if v <= min {
                        out.push(Violation {
                            path: path_or_root(path),
                            message: format!("must be > {min}"),
                        });
                    }
                }
            }
            if let Some(max) = obj.get("exclusiveMaximum").and_then(Value::as_f64) {
                if let Some(v) = n.as_f64() {
                    if v >= max {
                        out.push(Violation {
                            path: path_or_root(path),
                            message: format!("must be < {max}"),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

fn matches_type(t: &Value, v: &Value) -> bool {
    match t {
        Value::String(s) => single_type_matches(s, v),
        Value::Array(arr) => arr.iter().any(|t| t.as_str().map(|s| single_type_matches(s, v)).unwrap_or(false)),
        _ => true,
    }
}

fn single_type_matches(t: &str, v: &Value) -> bool {
    match (t, v) {
        ("string", Value::String(_)) => true,
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false),
        ("number", Value::Number(_)) => true,
        ("boolean", Value::Bool(_)) => true,
        ("object", Value::Object(_)) => true,
        ("array", Value::Array(_)) => true,
        ("null", Value::Null) => true,
        _ => false,
    }
}

fn render_type(t: &Value) -> String {
    match t {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let parts: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            parts.join("|")
        }
        _ => "?".into(),
    }
}

fn path_or_root(p: &str) -> String {
    if p.is_empty() { "/".into() } else { p.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn type_check() {
        let s = json!({"type":"object"});
        assert!(validate_inputs(&s, &json!({})).is_empty());
        assert_eq!(validate_inputs(&s, &json!("x"))[0].message, "expected type object");
    }

    #[test]
    fn missing_required() {
        let s = json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"}}});
        let v = validate_inputs(&s, &json!({}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "/name");
    }

    #[test]
    fn enum_violation() {
        let s = json!({"enum":["a","b","c"]});
        assert!(validate_inputs(&s, &json!("a")).is_empty());
        assert!(!validate_inputs(&s, &json!("z")).is_empty());
    }

    #[test]
    fn pattern_violation() {
        let s = json!({"type":"string","pattern":"^[A-Z]{3}$"});
        assert!(validate_inputs(&s, &json!("ABC")).is_empty());
        assert!(!validate_inputs(&s, &json!("abc")).is_empty());
    }

    #[test]
    fn nested_object_paths() {
        let s = json!({
            "type":"object",
            "properties":{
                "body":{
                    "type":"object",
                    "required":["email"],
                    "properties":{"email":{"type":"string","minLength":3}}
                }
            },
            "required":["body"]
        });
        let v = validate_inputs(&s, &json!({"body":{"email":"a"}}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "/body/email");
        assert!(v[0].message.contains("at least 3"));
    }

    #[test]
    fn additional_properties_false() {
        let s = json!({
            "type":"object",
            "properties":{"a":{"type":"string"}},
            "additionalProperties": false
        });
        let v = validate_inputs(&s, &json!({"a":"x","b":"y"}));
        assert_eq!(v.len(), 1);
        assert!(v[0].path.ends_with("/b"));
    }

    #[test]
    fn array_constraints() {
        let s = json!({"type":"array","items":{"type":"integer"},"minItems":1});
        assert!(!validate_inputs(&s, &json!([])).is_empty());
        assert!(validate_inputs(&s, &json!([1, 2])).is_empty());
        assert!(!validate_inputs(&s, &json!([1, "x"])).is_empty());
    }

    #[test]
    fn integer_vs_number() {
        let s = json!({"type":"integer"});
        assert!(validate_inputs(&s, &json!(3)).is_empty());
        assert!(validate_inputs(&s, &json!(3.0)).is_empty()); // 3.0 is integer-valued
        assert!(!validate_inputs(&s, &json!(3.5)).is_empty());
    }
}
