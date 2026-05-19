# Policy Requirements — UTCP Manual Validator (P1)

**Policy ID (proposed):** `utcp-manual-validator`
**Target runtime:** MuleSoft Omni Gateway (Flex Gateway) via PDK **1.8**
**Priority:** P1
**Document status:** Draft v0.1 — 2026-05-19

---

## 1. Background and Motivation

### 1.1 What UTCP is
The Universal Tool Calling Protocol (UTCP) is a JSON-based discovery-and-call contract that lets AI agents discover tools and invoke them over the tool's *native* transport (HTTP, gRPC, WebSocket, SSE, CLI, text, MCP-interop) instead of going through an MCP-style wrapper server.

A UTCP "Manual" is a JSON document with this top-level shape (UTCP v1.x):

```json
{
  "manual_version": "1.0.0",
  "utcp_version": "1.0.1",
  "info":      { "title": "...", "version": "...", "description": "..." },
  "variables": { "base_url": "...", "timeout": 30 },
  "tools": [
    {
      "name": "get_user",
      "description": "Look up a user by id",
      "inputs":  { "type": "object", "properties": { ... }, "required": [ ... ] },
      "outputs": { "type": "object", "properties": { ... } },
      "tool_call_template": {
        "call_template_type": "http",
        "url": "https://api.example.com/users/{user_id}",
        "http_method": "GET",
        "content_type": "application/json",
        "headers":      { "X-Static": "v" },
        "header_fields": ["request_id"],
        "body_field":    "body",
        "auth": {
          "auth_type": "api_key",
          "api_key":   "${API_KEY}",
          "var_name":  "Authorization",
          "location":  "header"
        }
      }
    }
  ]
}
```

Key facts that drive this policy's design:

- Manuals are conventionally served at `GET /utcp`, but UTCP clients also accept an OpenAPI document at any URL and auto-convert it via `OpenApiConverter` (detection rule: response lacks `utcp_version` and `tools` fields → treat as OpenAPI).
- `tool_call_template` types include `http`, `sse`, `streamable_http`, `cli`, `text`, `mcp` (plugin-based; new types may appear).
- HTTP parameter mapping is hierarchical: path placeholders → `body_field` → `header_fields` → remaining args become query string.
- Auth schemes: `api_key` (header/query/cookie), `basic`, `oauth2` (client credentials with token caching by `client_id`). Env-var interpolation `${VAR}` is permitted in auth fields.
- HTTPS is required except for `localhost` / `127.0.0.1`. Default timeouts: 10s discovery, 30s execution.

### 1.2 Why a gateway should sit here
UTCP's premise — "agents call native endpoints directly" — eliminates the wrapper hop but also eliminates the natural choke point for auth, quota, schema enforcement, and audit. A gateway *is* that choke point. The Manual is effectively a machine-readable contract of what an agent is allowed to call, and Omni Gateway already terminates the actual tool traffic, so it can:

1. **Host or proxy** the Manual itself (and optionally synthesize one from an existing OpenAPI spec).
2. **Validate** that incoming agent requests correspond to a tool declared in the Manual and conform to its `inputs` schema.
3. **Enforce** auth, quota, and policy on the *real* tool endpoints.
4. **Audit-log** every direct tool invocation with the originating agent identity and the matched tool name.

---

## 2. Goals and Non-Goals

### 2.1 Goals
- **G1.** Serve a UTCP Manual at a configurable discovery path (default `/utcp`) for any API instance the policy is attached to.
- **G2.** Auto-generate the Manual from an OpenAPI spec the API instance already publishes (mirroring UTCP's `OpenApiConverter` behavior).
- **G3.** Allow operators to override or extend the auto-generated Manual with a static Manual document or per-tool overrides.
- **G4.** On every request to a *tool* endpoint, validate the inbound request against the Manual: the URL/method must correspond to a declared tool, and the request body/query/headers must satisfy the tool's `inputs` schema.
- **G5.** Emit a structured audit-log line for every tool invocation: agent identity, tool name, status, latency, request id, and any validation failures.
- **G6.** Reject non-conformant requests with a deterministic UTCP-aware error response and a stable error code set.

### 2.2 Non-Goals
- **NG1.** This policy does **not** provide auth itself; it integrates with existing Omni Gateway auth policies (JWT, OAuth2, mTLS, API Key) and reads the resulting principal.
- **NG2.** This policy does **not** rate-limit; it integrates with existing rate-limit/quota policies. (It surfaces tool name as a label/header so those policies can scope quotas per-tool.)
- **NG3.** No support, in v1, for non-HTTP transports (`cli`, `mcp`, `text`, `sse`, `streamable_http`) — but the design must not preclude adding them.
- **NG4.** No client-side execution of UTCP tool calls (the agent is the client; the gateway is the server side of the tool).

---

## 3. Glossary

| Term | Meaning |
|---|---|
| **Manual** | A UTCP Manual JSON document declaring tools and their call templates. |
| **Tool** | One named, callable entry inside a Manual. |
| **Tool endpoint** | The actual HTTP URL/method backing a tool's `tool_call_template`. |
| **Discovery endpoint** | The path the policy serves the Manual on (default `/utcp`). |
| **Agent** | The UTCP client (typically an LLM-driven application) that fetches the Manual and calls tools. |
| **PDK** | MuleSoft Policy Development Kit; Rust + proxy-wasm SDK for Omni/Flex Gateway. |

---

## 4. Functional Requirements

### 4.1 Manual hosting (FR-MAN)

- **FR-MAN-1.** The policy MUST respond to `GET <discovery_path>` (configurable; default `/utcp`) with `Content-Type: application/json` and a valid Manual body. The handler MUST short-circuit the upstream call (no traffic to origin).
- **FR-MAN-2.** When `manual_source: openapi` is configured, the policy MUST fetch the configured OpenAPI URL on policy startup *and* on a configurable refresh interval, convert it to a UTCP Manual, and cache the result.
- **FR-MAN-3.** When `manual_source: static` is configured, the policy MUST load the Manual from a config-embedded JSON document.
- **FR-MAN-4.** When `manual_source: hybrid` is configured, the policy MUST start from the OpenAPI conversion and apply a configured patch document (per-tool overrides for `description`, `auth`, `tags`, `average_response_size`, custom `headers`, etc.).
- **FR-MAN-5.** The served Manual MUST include `manual_version`, `utcp_version`, `info`, and `tools[]`. `utcp_version` MUST be a value the policy actually targets (default `1.0.1`; configurable).
- **FR-MAN-6.** The policy MUST support `HEAD` and `OPTIONS` on the discovery path (return 200 + correct CORS headers if CORS is enabled in config).
- **FR-MAN-7.** The Manual response MUST be served with `Cache-Control` controllable by config (default `public, max-age=60`).
- **FR-MAN-8.** Auth secret values (`api_key`, `client_secret`) in the Manual MUST be emitted as `${ENV_VAR_NAME}` placeholders, never as literal secrets, regardless of how they were configured.

### 4.2 OpenAPI → UTCP conversion (FR-CONV)

- **FR-CONV-1.** Conversion MUST accept OpenAPI 3.0 and 3.1 documents in JSON or YAML.
- **FR-CONV-2.** Each `paths.<path>.<method>` MUST become one tool. Tool `name` defaults to `operationId`; if absent, use `<METHOD>_<path-slug>`.
- **FR-CONV-3.** Path parameters MUST become `{param}` placeholders in the tool's `url`. Query/header/cookie parameters MUST be mapped per the UTCP HTTP spec (header → `header_fields`, body → `body_field`, the rest → query string).
- **FR-CONV-4.** OpenAPI `requestBody` schemas MUST be inlined into `inputs` under a `body` property (or whatever name `body_field` is set to). `$ref` resolution MUST be supported within the same document.
- **FR-CONV-5.** OpenAPI `securitySchemes` MUST translate to UTCP `auth` blocks: `apiKey` → `api_key`, `http bearer` → `api_key` with `Bearer ${TOKEN}` shape, `oauth2 clientCredentials` → `oauth2`.
- **FR-CONV-6.** When the OpenAPI fetch fails or returns invalid JSON/YAML on startup, the policy MUST log a structured error and serve a `503` on the discovery path until the next successful refresh, *without* impacting tool-endpoint enforcement (which falls back to the last good Manual if available).

### 4.3 Inbound request validation (FR-VAL)

- **FR-VAL-1.** For every request that is not the discovery path, the policy MUST resolve the request to at most one tool by matching `(method, path-template)` against the cached Manual. Path-template matching MUST treat `{param}` as a single non-slash segment.
- **FR-VAL-2.** If no tool matches and `enforcement_mode: strict` is set, the request MUST be rejected with HTTP `404` and error code `utcp.tool_not_declared`.
- **FR-VAL-3.** If no tool matches and `enforcement_mode: permissive` is set, the request MUST pass through unmodified, but a `utcp.unmatched=true` audit field MUST be recorded.
- **FR-VAL-4.** When a tool matches and `validate_inputs: true` is set, the policy MUST validate request payload against `tool.inputs` JSON Schema (Draft 2020-12). Validation MUST cover:
  - Path params → coerced from URL segments,
  - Query params → coerced from query string per declared types,
  - Header params declared in `header_fields`,
  - Body → parsed per `Content-Type` (initial scope: `application/json`; reject other content types when body validation is required unless `accept_unvalidated_content_types` lists them).
- **FR-VAL-5.** Validation failures MUST yield HTTP `400` with body:
  ```json
  {
    "error": "utcp.input_invalid",
    "tool":  "<tool name>",
    "violations": [ { "path": "/body/email", "message": "must match format \"email\"" } ]
  }
  ```
- **FR-VAL-6.** The policy MUST set request header `x-utcp-tool: <tool_name>` on the upstream request after a successful match, so downstream policies (rate limit, audit) can scope by tool.
- **FR-VAL-7.** The policy MUST be safe under partial-body availability: if validation requires the body and the body is chunked, it MUST buffer up to a configurable `max_body_bytes` (default 1 MiB) and reject with `413` when exceeded.

### 4.4 Auth and identity (FR-AUTH)

- **FR-AUTH-1.** The policy MUST NOT itself authenticate the agent. It MUST read the principal that an upstream auth policy has placed in a configurable header (default `x-anypoint-client-id`) or in the request's authority context.
- **FR-AUTH-2.** When the principal is missing and `require_principal: true` is set, the policy MUST reject with HTTP `401` and `utcp.unauthenticated`.
- **FR-AUTH-3.** Auth scheme rendering in the served Manual is purely declarative — the policy never holds upstream secrets. Operators wire actual credentials at the agent side via env vars referenced by `${VAR_NAME}`.

### 4.5 Audit logging (FR-LOG)

- **FR-LOG-1.** Every tool invocation MUST emit one structured log line at end-of-response containing: `timestamp`, `request_id`, `principal`, `tool`, `method`, `path`, `status`, `latency_ms`, `validation_status` (`ok` | `failed` | `skipped`), `bytes_in`, `bytes_out`.
- **FR-LOG-2.** Validation failures MUST emit an additional `violations[]` field (capped at a configurable count).
- **FR-LOG-3.** Logs MUST be routed via PDK's logging facility so they flow into the operator's existing log pipeline (Anypoint Monitoring, stdout, or sidecar collector — the policy does not pick a sink).

### 4.6 Manual refresh and caching (FR-CACHE)

- **FR-CACHE-1.** The policy MUST keep an in-memory cache of: `(parsed_manual, compiled_path_router, compiled_input_schemas)`.
- **FR-CACHE-2.** On a configurable interval (default 300s) and on demand via a configurable admin path (default `POST <discovery_path>/refresh`, gated to a configurable admin token), the policy MUST refetch and recompile the OpenAPI source.
- **FR-CACHE-3.** Refresh failures MUST NOT evict the last good cache. They MUST be observable via a counter metric.

---

## 5. Non-Functional Requirements

- **NFR-1. Latency.** Validation overhead on a 1 KB JSON body for a tool with ~10 input properties MUST be < 2 ms p99 on a representative dev host.
- **NFR-2. Memory.** Per-API-instance footprint MUST stay under 32 MiB for Manuals up to 500 tools.
- **NFR-3. Determinism.** Two identical OpenAPI inputs MUST produce byte-identical Manuals (stable tool ordering, stable JSON key order). This matters for downstream agents that hash the Manual.
- **NFR-4. Compatibility.** Targets PDK **1.8** and the Omni Gateway version line that ships with it. The policy MUST run in both Connected Mode (configured via API Manager) and Local Mode (configured via YAML).
- **NFR-5. Security.**
  - The policy MUST never reflect secret env-var values in the served Manual.
  - The OpenAPI fetcher MUST refuse non-HTTPS URLs unless `allow_insecure_openapi_url: true` is explicitly set, and MUST disallow `file://` and link-local addresses.
  - The admin refresh path MUST require a constant-time string comparison on the admin token.
- **NFR-6. Observability.** The policy MUST expose counters: `utcp.manual.refresh.success`, `utcp.manual.refresh.failure`, `utcp.requests.matched`, `utcp.requests.unmatched`, `utcp.requests.invalid_input`, `utcp.discovery.served`.

---

## 6. Configuration Schema

The policy schema is defined in `definition/gcl.yaml` (PDK convention) and rendered as a struct under `src/generated/config.rs`. Proposed fields:

```yaml
# definition/gcl.yaml (excerpt)
schema:
  type: object
  required: [manual_source]
  properties:
    discovery_path:
      type: string
      default: "/utcp"
    utcp_version:
      type: string
      default: "1.0.1"
    manual_source:
      type: string
      enum: [openapi, static, hybrid]
    openapi:
      type: object
      properties:
        url:                 { type: string, format: uri }
        refresh_interval_s:  { type: integer, default: 300, minimum: 30 }
        allow_insecure_url:  { type: boolean, default: false }
        bearer_token_env:    { type: string }   # optional auth for fetching the OpenAPI
    static_manual:
      type: object  # full UTCP manual document, used when manual_source = static or as patch when hybrid
    overrides:
      type: array
      items:
        type: object
        properties:
          tool_name_pattern: { type: string } # regex
          set:
            type: object
            properties:
              description:           { type: string }
              tags:                  { type: array, items: { type: string } }
              average_response_size: { type: integer }
              auth:                  { type: object }
              headers:               { type: object }
    enforcement_mode:
      type: string
      enum: [strict, permissive]
      default: strict
    validate_inputs:        { type: boolean, default: true }
    max_body_bytes:         { type: integer, default: 1048576 }
    accept_unvalidated_content_types:
      type: array
      items: { type: string }
      default: []
    require_principal:      { type: boolean, default: false }
    principal_header:       { type: string,  default: "x-anypoint-client-id" }
    cache_control_header:   { type: string,  default: "public, max-age=60" }
    cors:
      type: object
      properties:
        enabled:         { type: boolean, default: false }
        allowed_origins: { type: array, items: { type: string } }
    admin:
      type: object
      properties:
        refresh_path:  { type: string, default: "/utcp/refresh" }
        token_env:     { type: string }   # name of env var holding the shared secret
    audit:
      type: object
      properties:
        max_violations_logged: { type: integer, default: 25 }
        include_request_body:  { type: boolean, default: false }
```

---

## 7. Implementation Approach on Omni Gateway / PDK 1.8

### 7.1 Why this fits PDK
PDK policies compile to WebAssembly via the proxy-wasm spec and run as Envoy filters. PDK 1.8 layers a reactor/executor abstraction so the developer writes linear request/response handlers instead of raw proxy-wasm callbacks. That model is a clean fit for this policy because it has three orthogonal concerns:

1. A **discovery handler** that serves a JSON body and short-circuits.
2. A **validation handler** that inspects the request line, headers, and (optionally) buffers the body.
3. A **background refresher** that periodically fetches and recompiles the Manual.

PDK exposes outbound HTTP calls (used for the OpenAPI fetch), structured logging, and config hydration from the gcl-defined schema, so all three concerns can live in one policy crate.

### 7.2 Project layout (PDK 1.8 convention)

```
utcp-policy/
├── Cargo.toml
├── Makefile
├── .project.yaml
├── definition/
│   └── gcl.yaml                 # policy metadata + config schema (Section 6)
├── src/
│   ├── lib.rs                   # #[entrypoint], filter wiring
│   ├── generated/
│   │   ├── mod.rs               # generated by `make build-asset-files`
│   │   └── config.rs            # generated Configuration struct
│   ├── manual/
│   │   ├── mod.rs
│   │   ├── model.rs             # UTCP Manual / Tool / CallTemplate types (serde)
│   │   ├── openapi.rs           # OpenApiConverter port (3.0 + 3.1)
│   │   ├── render.rs            # serialize Manual deterministically
│   │   └── overrides.rs         # apply hybrid overrides
│   ├── validate/
│   │   ├── mod.rs
│   │   ├── router.rs            # path-template router (radix tree on segments)
│   │   └── schema.rs            # JSON Schema 2020-12 compile + validate
│   ├── audit.rs                 # structured logging
│   └── http_client.rs           # outbound fetch wrapper
├── playground/                  # local-mode YAML + docker-compose for manual testing
└── tests/
    ├── conformance/             # golden-file tests: openapi.yaml → expected manual.json
    ├── validation/              # request-validation cases
    └── integration/             # end-to-end through Flex playground
```

### 7.3 `Cargo.toml` outline

```toml
[package]
name    = "utcp-manual-validator"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
pdk           = "1.8"            # MuleSoft PDK SDK
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
serde_yaml    = "0.9"
jsonschema    = { version = "0.18", default-features = false }  # 2020-12 support
regex         = "1"
url           = "2"
once_cell     = "1"
anyhow        = "1"
thiserror     = "1"

[build-dependencies]
pdk-build     = "1.8"
```

> Verify exact crate names against the PDK 1.8 release. The project scaffold from `anypoint-cli-v4 pdk policy-project create` is the authoritative starting point — we should generate it once and align imports with what's actually shipped before locking dependencies.

### 7.4 `src/lib.rs` skeleton (illustrative)

```rust
use anyhow::Result;
use pdk::hl::*;
use pdk::logger;

mod audit;
mod http_client;
mod manual;
mod validate;
mod generated;

use generated::config::Config;
use manual::{ManualState};
use validate::Decision;

#[entrypoint]
async fn configure(launcher: Launcher, Configuration(bytes): Configuration) -> Result<()> {
    let cfg: Config = serde_json::from_slice(&bytes)?;
    let state = ManualState::initialize(&cfg).await?;     // load openapi/static + compile
    state.spawn_refresher(cfg.openapi.refresh_interval_s); // background tick

    let filter = move |request: RequestHeadersState, response: ResponseHeadersState| {
        let cfg = cfg.clone();
        let state = state.clone();
        async move {
            on_request(request, response, &cfg, &state).await
        }
    };
    launcher.launch(on_request_filter(filter)).await?;
    Ok(())
}

async fn on_request(
    req: RequestHeadersState,
    resp: ResponseHeadersState,
    cfg: &Config,
    state: &ManualState,
) -> Flow<()> {
    let path   = req.path();
    let method = req.method();

    // 1) Discovery path → short-circuit with the manual.
    if method == "GET" && path == cfg.discovery_path {
        let body = state.serialized_manual();
        return resp.send(200)
            .header("content-type", "application/json")
            .header("cache-control", &cfg.cache_control_header)
            .body(body)
            .into_break();
    }

    // 2) Admin refresh.
    if method == "POST" && path == cfg.admin.refresh_path {
        return audit::handle_admin_refresh(req, cfg, state).await;
    }

    // 3) Match against the manual.
    match state.router().resolve(method, path) {
        Some(tool) => {
            req.headers_mut().set("x-utcp-tool", tool.name());
            if cfg.validate_inputs {
                let body = req.read_body_capped(cfg.max_body_bytes).await?;
                if let Err(violations) = tool.validate(&req, &body) {
                    audit::record_invalid(req, tool, &violations);
                    return resp.send(400)
                        .header("content-type", "application/json")
                        .body(audit::render_violations(tool, &violations))
                        .into_break();
                }
            }
            audit::record_matched(req, tool);
            Flow::Continue(())
        }
        None if cfg.enforcement_mode.is_strict() => {
            audit::record_unmatched(req);
            resp.send(404)
                .header("content-type", "application/json")
                .body(br#"{"error":"utcp.tool_not_declared"}"#.to_vec())
                .into_break()
        }
        None => {
            audit::record_unmatched(req);
            Flow::Continue(())
        }
    }
}
```

> The exact PDK 1.8 trait names (`RequestHeadersState`, `Launcher`, `Flow`, `Configuration`, `on_request_filter`) are illustrative and must be confirmed against `pdk` 1.8's published API surface. The generated scaffold from `make setup` is canonical; treat the snippet above as a structural placeholder, not copy-paste-ready code.

### 7.5 OpenAPI conversion strategy

We do **not** call Python's `OpenApiConverter` directly. We re-implement its behavior in Rust against UTCP's documented mapping rules so the policy stays a single self-contained WASM module:

| OpenAPI element | UTCP target |
|---|---|
| `paths.{p}.{method}.operationId` | `tool.name` (fallback: `<METHOD>_<slug>`) |
| `summary` / `description` | `tool.description` |
| `parameters[in=path]`             | `{name}` placeholders in `url` |
| `parameters[in=query]`            | merged into `inputs.properties`; default mapping (becomes query string) |
| `parameters[in=header]`           | added to `header_fields`; merged into `inputs.properties` |
| `parameters[in=cookie]`           | merged into `inputs.properties`; auth-only or rejected per config |
| `requestBody.content.<ct>.schema` | `inputs.properties[body_field]`; `body_field` = `"body"` by default |
| `responses.<2xx>.content.<ct>.schema` | `outputs` |
| `securitySchemes.apiKey`          | `auth: { auth_type: api_key, var_name, location, api_key: "${ENV}" }` |
| `securitySchemes.http bearer`     | `auth: { auth_type: api_key, var_name: "Authorization", api_key: "Bearer ${ENV}", location: "header" }` |
| `securitySchemes.oauth2 clientCredentials` | `auth: { auth_type: oauth2, client_id: "${C}", client_secret: "${S}", token_url, scope }` |
| `tags`                            | preserved on tools |

`$ref` resolution is local-only in v1 (no remote `$ref`). Bundling externals is the operator's responsibility.

### 7.6 Path-template router

A radix tree over slash-segmented paths, where a literal segment beats a `{param}` segment, and conflicts (two tools claiming the same template) are reported at compile time via a startup error in the policy log. Stored alongside each leaf: tool index + compiled JSON-Schema validator.

### 7.7 Validation pipeline

1. Resolve `(method, path) → tool`.
2. Extract path params from URL segments.
3. Parse query string and header_fields into typed values per `inputs`.
4. If body is required, buffer up to `max_body_bytes` and `serde_json::from_slice` it; reject `413` on overrun.
5. Build a synthetic `value` object `{ <param>: ..., body: <body> }` and run a single `jsonschema` compile/validate.
6. On failure, render up to `max_violations_logged` and return 400.

JSON Schema compilation happens **once at Manual load time**, not per request.

### 7.8 Outbound HTTP for OpenAPI fetch

PDK exposes an outbound HTTP client for calling external services. The fetcher:
- Refuses non-HTTPS unless `allow_insecure_url: true`.
- Resolves DNS through Envoy (no raw sockets from WASM).
- Honors `bearer_token_env` if set.
- Times out at 10 s (matching UTCP's discovery default).
- On failure, increments `utcp.manual.refresh.failure` and keeps the prior cache.

### 7.9 Connected Mode vs. Local Mode

- **Local Mode**: ship the policy WASM + `definition/gcl.yaml`; operators reference the policy in their Local Mode YAML and pass config inline. No additional artifacts.
- **Connected Mode**: publish to Exchange via `make publish` / `make release`. API Manager renders the gcl schema into the policy configuration UI, so all of Section 6 must validate against `gcl.yaml` cleanly.

---

## 8. Error and Response Shapes

| Scenario | HTTP | Body |
|---|---|---|
| Tool not declared (strict) | 404 | `{"error":"utcp.tool_not_declared"}` |
| Input invalid | 400 | `{"error":"utcp.input_invalid","tool":"...","violations":[...]}` |
| Body too large | 413 | `{"error":"utcp.body_too_large","limit":1048576}` |
| Unauthenticated (when `require_principal`) | 401 | `{"error":"utcp.unauthenticated"}` |
| Manual unavailable on discovery | 503 | `{"error":"utcp.manual_unavailable"}` |
| Admin refresh unauthorized | 403 | `{"error":"utcp.admin_forbidden"}` |

All error responses set `content-type: application/json`.

---

## 9. Testing Strategy

- **Unit (Rust):**
  - `manual::openapi` round-trip: a fixture OpenAPI YAML/JSON → expected Manual JSON (golden files; assert byte-equality for determinism).
  - `validate::router`: literal-vs-template precedence, conflicts at compile time, trailing slashes, percent-encoded segments.
  - `validate::schema`: JSON Schema 2020-12 conformance against a published test suite.
- **Integration (PDK playground):**
  - Spin up the policy in `playground/` against a mock backend.
  - Tests cover: discovery served, hybrid overrides applied, strict reject of unmatched, permissive pass-through of unmatched, body too large, OpenAPI refresh succeeds and survives a transient failure.
- **Conformance:**
  - Round-trip a real-world OpenAPI (e.g., a checked-in fixture from the GitHub OpenAPI document, slimmed) through Rust converter and through `python-utcp`'s `OpenApiConverter` and assert tool-set equivalence (names, urls, methods).

---

## 10. Observability

| Signal | Type | Description |
|---|---|---|
| `utcp.manual.refresh.success`         | counter | Successful OpenAPI fetch + recompile |
| `utcp.manual.refresh.failure`         | counter | Failed refresh; labeled by reason (`fetch`,`parse`,`compile`) |
| `utcp.manual.tools`                   | gauge   | Number of tools currently served |
| `utcp.discovery.served`               | counter | Discovery responses sent |
| `utcp.requests.matched`               | counter | Tool requests matched, labeled by `tool` |
| `utcp.requests.unmatched`             | counter | Tool requests not matched |
| `utcp.requests.invalid_input`         | counter | 400s, labeled by `tool` |
| `utcp.requests.body_too_large`        | counter | 413s |
| `utcp.validate.duration_ms`           | histogram | Validation latency |

---

## 11. Risks and Open Questions

1. **PDK 1.8 API surface.** Several PDK doc subpages were not directly retrievable while drafting this. The handler signatures in §7.4 must be reconciled with what `anypoint-cli-v4 pdk policy-project create` scaffolds before code is written. **Action:** scaffold a hello-world policy on PDK 1.8 and pin signatures.
2. **JSON Schema 2020-12 in WASM.** The `jsonschema` crate's WASM compatibility under `wasm32-wasip1` (PDK 1.8's target per prerequisites) needs verification, especially around regex (`fancy-regex` vs. `regex`) and format validators. **Action:** spike-build with the policy scaffold; if blocked, fall back to a constrained subset (Draft 7 with `valico` or `boon`).
3. **Body buffering cost.** Some validation requires the full body, which forces buffering and inflates latency on large payloads. **Mitigation:** `max_body_bytes` cap and a per-tool `validate_body: false` opt-out for large/binary endpoints.
4. **Path-template ambiguity with OpenAPI.** Two operations with overlapping templates (`/users/{id}` and `/users/me`) need deterministic resolution. **Decision:** literal beats template; same-shape conflicts fail at startup.
5. **gRPC / WebSocket coverage.** UTCP allows non-HTTP transports. Flex Gateway's policy story for those is more limited. **Decision:** v1 = HTTP only; document this clearly in policy metadata so Connected Mode users see it in API Manager.
6. **Manual hashing for agents.** Agents may cache by hash. If we re-emit `${ENV}` placeholders deterministically and order tools by `name`, hashes will be stable across restarts; we should also expose `ETag` on the discovery response.
7. **Secret leakage via overrides.** The `overrides.set.auth` config can technically embed literal secrets. **Mitigation:** schema validator on `gcl.yaml` rejects `auth.api_key` values that don't match `^\$\{[A-Z0-9_]+\}$`.

---

## 12. Acceptance Criteria

The policy is ready for P1 sign-off when **all** of the following are demonstrable in the Flex Gateway playground:

1. With `manual_source: openapi` pointing at a fixture OpenAPI document, `GET /utcp` returns a structurally valid UTCP Manual that successfully ingests into a `python-utcp` `UtcpClient` with no errors.
2. With `enforcement_mode: strict`, requests to `/users/{id}` succeed when the path parameter and JSON body match `inputs`, and requests with a missing required field return 400 with a populated `violations` array.
3. With `enforcement_mode: strict`, a request to a non-declared path returns 404 `utcp.tool_not_declared`.
4. The OpenAPI source can be rotated at runtime via the admin refresh endpoint, and the served Manual reflects the change within one second without dropping in-flight requests.
5. Audit log lines for matched and invalid requests are present in stdout with the schema in §4.5.
6. The same policy artifact installs and runs in both Connected Mode (Exchange-published) and Local Mode (YAML-configured) without code changes.

---

## 13. References

- UTCP overview and Manual concept — utcp.io
- UTCP specification repository — `universal-tool-calling-protocol/utcp-specification`
- `python-utcp` README and `OpenApiConverter` docstring
- UTCP HTTP protocol spec (`docs/protocols/http.md`): URL/path/header/body/query mapping; auth shapes; OpenAPI auto-conversion trigger
- MuleSoft PDK overview, prerequisites, and architecture (`docs.mulesoft.com/pdk/latest/`)
- proxy-wasm specification (the substrate PDK abstracts over)
