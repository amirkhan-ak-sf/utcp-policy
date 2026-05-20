//! `(method, path) -> tool` resolution.
//!
//! The router walks each tool's `tool_call_template.url`, strips the
//! scheme/authority/base, and reduces it to a slash-segmented template
//! whose segments are either literal text or a `{param}` placeholder.
//!
//! Routing rules:
//!
//!   1. Method match is case-insensitive.
//!   2. Segment count must match exactly.
//!   3. A literal segment beats a `{param}` segment when both can match
//!      (so `/users/me` is preferred over `/users/{id}`).
//!   4. Two tools that compile to the same `(method, template)` shape
//!      are reported at startup via `RouterError::Conflict`. Operators
//!      need to fix the upstream OpenAPI rather than have the policy
//!      pick arbitrarily at runtime.

use std::collections::HashMap;

use thiserror::Error;

use crate::config::ToolEntry;

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("routing conflict: tools '{0}' and '{1}' share the same ({2} {3}) template")]
    Conflict(String, String, String, String),
}

/// One compiled URL template.
#[derive(Debug, Clone)]
struct Route {
    tool_index: usize,
    method: String,
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param(String),
}

#[derive(Debug, Clone)]
pub struct ResolvedRoute<'a> {
    pub tool_index: usize,
    pub tool_name: &'a str,
    pub path_params: HashMap<String, String>,
    pub query: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ToolRouter {
    routes: Vec<Route>,
    /// Cached tool names so resolve() can return them as `&str`.
    tool_names: Vec<String>,
}

impl ToolRouter {
    /// Build a router from validated tool entries. The matching template
    /// comes from each entry's `path_template` (the policy-config `path`
    /// field), not from the rendered Manual URL — the latter has scheme
    /// and host attached for agent display and may also include the
    /// API-instance proxy prefix.
    pub fn build(tools: &[ToolEntry]) -> Result<Self, RouterError> {
        let mut routes = Vec::with_capacity(tools.len());
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

        for (i, tool) in tools.iter().enumerate() {
            let method = tool.method.to_ascii_uppercase();
            let segments = compile_template(&tool.path_template);
            routes.push(Route {
                tool_index: i,
                method,
                segments,
            });
        }

        // Detect conflicts. Two routes with the same shape (same
        // method, same per-segment kind) clash.
        for i in 0..routes.len() {
            for j in (i + 1)..routes.len() {
                if routes[i].method == routes[j].method
                    && segments_overlap(&routes[i].segments, &routes[j].segments)
                {
                    return Err(RouterError::Conflict(
                        tool_names[routes[i].tool_index].clone(),
                        tool_names[routes[j].tool_index].clone(),
                        routes[i].method.clone(),
                        path_template_string(&routes[i].segments),
                    ));
                }
            }
        }

        Ok(Self { routes, tool_names })
    }

    pub fn resolve(&self, method: &str, request_path: &str) -> Option<ResolvedRoute<'_>> {
        let (path, query) = split_path_query(request_path);
        let path_segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let want_method = method.to_ascii_uppercase();

        // Score: 0 = literal beats param. Pick the lowest score.
        let mut best: Option<(usize, &Route, HashMap<String, String>)> = None;

        for route in &self.routes {
            if route.method != want_method {
                continue;
            }
            if route.segments.len() != path_segments.len() {
                continue;
            }
            let mut params = HashMap::new();
            let mut score = 0usize;
            let mut ok = true;
            for (seg, requested) in route.segments.iter().zip(path_segments.iter()) {
                match seg {
                    Segment::Literal(lit) => {
                        if lit != requested {
                            ok = false;
                            break;
                        }
                    }
                    Segment::Param(name) => {
                        params.insert(name.clone(), urldecode(requested));
                        score += 1; // param is "less specific" than literal
                    }
                }
            }
            if !ok {
                continue;
            }
            match &best {
                None => best = Some((score, route, params)),
                Some((bs, _, _)) if score < *bs => best = Some((score, route, params)),
                _ => {}
            }
        }

        best.map(|(_, route, params)| ResolvedRoute {
            tool_index: route.tool_index,
            tool_name: &self.tool_names[route.tool_index],
            path_params: params,
            query: parse_query(query),
        })
    }
}

fn compile_template(url: &str) -> Vec<Segment> {
    // Strip scheme/authority and any leading slash.
    let path = strip_to_path(url);
    if path.is_empty() {
        return Vec::new();
    }
    path.trim_start_matches('/')
        .split('/')
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') && s.len() > 2 {
                Segment::Param(s[1..s.len() - 1].to_string())
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

fn strip_to_path(url: &str) -> &str {
    if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        rest.find('/').map(|i| &rest[i..]).unwrap_or("")
    } else {
        url
    }
}

fn segments_overlap(a: &[Segment], b: &[Segment]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
        (Segment::Literal(l), Segment::Literal(r)) => l == r,
        (Segment::Literal(_), Segment::Param(_)) | (Segment::Param(_), Segment::Literal(_)) => {
            // Literal/param overlap is *resolved* by scoring at runtime,
            // so it's not a conflict.
            false
        }
        (Segment::Param(_), Segment::Param(_)) => true,
    })
}

fn path_template_string(segments: &[Segment]) -> String {
    let mut out = String::new();
    for s in segments {
        out.push('/');
        match s {
            Segment::Literal(l) => out.push_str(l),
            Segment::Param(p) => {
                out.push('{');
                out.push_str(p);
                out.push('}');
            }
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

fn split_path_query(path: &str) -> (&str, &str) {
    match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    }
}

fn parse_query(query: &str) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if query.is_empty() {
        return out;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), urldecode(v)),
            None => (pair.to_string(), String::new()),
        };
        out.entry(urldecode(&k)).or_default().push(v);
    }
    out
}

fn urldecode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, method: &str, path: &str) -> ToolEntry {
        ToolEntry {
            name: name.into(),
            description: String::new(),
            method: method.into(),
            path_template: path.into(),
            content_type: "application/json".into(),
            body_field: None,
            inputs: None,
        }
    }

    #[test]
    fn matches_simple_get() {
        let r = ToolRouter::build(&[entry("getThing", "GET", "/things/{id}")]).unwrap();
        let m = r.resolve("GET", "/things/42").unwrap();
        assert_eq!(m.tool_name, "getThing");
        assert_eq!(m.path_params.get("id").map(String::as_str), Some("42"));
    }

    #[test]
    fn literal_beats_param() {
        let r = ToolRouter::build(&[
            entry("getById", "GET", "/users/{id}"),
            entry("getMe", "GET", "/users/me"),
        ])
        .unwrap();
        let m = r.resolve("GET", "/users/me").unwrap();
        assert_eq!(m.tool_name, "getMe");
        let m = r.resolve("GET", "/users/123").unwrap();
        assert_eq!(m.tool_name, "getById");
    }

    #[test]
    fn method_mismatch_returns_none() {
        let r = ToolRouter::build(&[entry("getThing", "GET", "/things/{id}")]).unwrap();
        assert!(r.resolve("POST", "/things/42").is_none());
    }

    #[test]
    fn segment_count_mismatch_returns_none() {
        let r = ToolRouter::build(&[entry("getThing", "GET", "/things/{id}")]).unwrap();
        assert!(r.resolve("GET", "/things").is_none());
        assert!(r.resolve("GET", "/things/42/extra").is_none());
    }

    #[test]
    fn detects_param_param_conflict() {
        let err = ToolRouter::build(&[
            entry("a", "GET", "/things/{id}"),
            entry("b", "GET", "/things/{slug}"),
        ])
        .unwrap_err();
        assert!(matches!(err, RouterError::Conflict(..)));
    }

    #[test]
    fn parses_query_string() {
        let r = ToolRouter::build(&[entry("list", "GET", "/things")]).unwrap();
        let m = r.resolve("GET", "/things?limit=10&q=hello%20world").unwrap();
        assert_eq!(m.query.get("limit").map(|v| v[0].as_str()), Some("10"));
        assert_eq!(m.query.get("q").map(|v| v[0].as_str()), Some("hello world"));
    }

    #[test]
    fn case_insensitive_method() {
        let r = ToolRouter::build(&[entry("list", "get", "/things")]).unwrap();
        assert!(r.resolve("GET", "/things").is_some());
        assert!(r.resolve("get", "/things").is_some());
    }
}
