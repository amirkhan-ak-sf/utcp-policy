# UTCP Manual Validator

A MuleSoft Flex Gateway policy that turns any HTTP API instance into a
UTCP-compliant tool surface. The policy hosts a UTCP **Manual** at a
configurable discovery path and enforces, on every inbound request, that
the call corresponds to a tool the Manual declares — and that the
request's body, query, headers, and path parameters satisfy the tool's
JSON Schema.

> **What is UTCP?** The Universal Tool Calling Protocol is a JSON
> discovery-and-call contract for AI agents. A "Manual" is a JSON
> document listing tools and how to invoke them on their *native*
> transport (HTTP today). Agents fetch the Manual once, then call tools
> directly. Because Flex Gateway already terminates the underlying
> traffic, it is the natural place to publish the contract and enforce
> conformance.

---

## At a glance

| | |
|---|---|
| **Policy ID** | `utcp-manual-validator-policy-dev` (definition) / `utcp-manual-validator-policy-impl-dev` (implementation) |
| **Runtime** | Flex Gateway via PDK 1.8 (wasm32-wasip1) |
| **Injection point** | inbound |
| **Interface scope** | api, resource |
| **Category** | Security |
| **Version** | 0.1.0 (initial release) |

---

## What the policy does

1. **Hosts the Manual.** `GET <discoveryPath>` (default `/utcp`) returns
   a UTCP Manual JSON document. Operators can supply the Manual inline
   (`manualSource: static`), or — once enabled — derive it from an
   OpenAPI spec the API already publishes.
2. **Routes each tool call.** Every non-discovery request is matched
   against `(method, path-template)` from the Manual. Path placeholders
   like `/users/{id}` match a single non-slash segment.
3. **Validates inputs.** When a tool matches, the request body is
   parsed and validated against the tool's `inputs` JSON Schema before
   the request is forwarded upstream.
4. **Tags successful matches.** A successful match stamps
   `x-utcp-tool: <tool-name>` (configurable header name) on the
   upstream request so downstream rate-limit / quota / audit policies
   can scope per tool.
5. **Audit-logs every decision.** Matched, unmatched, and validation
   failures all emit a structured log line via PDK logging.

The policy never holds upstream credentials. Auth values declared in
the Manual are emitted as `${ENV_VAR}` placeholders that the agent
resolves on its side.

---

## Quick start

### 1. Apply the policy to an API instance

In API Manager, open your Flex-deployed API instance and apply the
**UTCP Manual Validator** policy from Exchange. Configure at least:

- `manualSource`: `static` (only mode supported in v0.1)
- `staticManual`: a JSON string containing your UTCP Manual
- `discoveryPath`: the path you want the Manual served on

A minimal working `staticManual` (escaped for the JSON form):

```json
{
  "manual_version": "1.0.0",
  "utcp_version": "1.0.1",
  "info": {
    "title": "SAP Orders UTCP API",
    "version": "1.0.0",
    "description": "Create sales orders in SAP ECC."
  },
  "tools": [
    {
      "name": "createSalesOrder",
      "description": "Create a sales order in SAP ECC.",
      "inputs": {
        "type": "object",
        "required": ["body"],
        "properties": {
          "body": {
            "type": "object",
            "required": [
              "SALES_ORG", "DISTR_CHAN", "DIVISION",
              "PURCH_NO", "MATERIAL", "PLANT",
              "TARGET_QTY", "CUSTOMER"
            ],
            "properties": {
              "SALES_ORG":  { "type": "string" },
              "DISTR_CHAN": { "type": "string" },
              "DIVISION":   { "type": "string" },
              "PURCH_NO":   { "type": "string" },
              "MATERIAL":   { "type": "string" },
              "PLANT":      { "type": "string" },
              "TARGET_QTY": { "type": "string" },
              "CUSTOMER":   { "type": "string" }
            }
          }
        }
      },
      "tool_call_template": {
        "call_template_type": "http",
        "url":  "https://gateway.example.com/sap-orders/api/order",
        "http_method": "POST",
        "content_type": "application/json",
        "body_field": "body"
      }
    }
  ]
}
```

### 2. Discover the Manual

```bash
curl -i https://<gateway-host>/<api-prefix>/utcp
```

Returns the Manual with `Content-Type: application/json` and the
configured `Cache-Control`.

### 3. Call a tool

```bash
curl -i -X POST https://<gateway-host>/<api-prefix>/api/order \
  -H 'Content-Type: application/json' \
  -d '{ "SALES_ORG": "3000", "DISTR_CHAN": "10", "DIVISION": "00",
        "PURCH_NO": "Cake", "MATERIAL": "MULETEST0", "PLANT": "3000",
        "TARGET_QTY": "1", "CUSTOMER": "0000000007" }'
```

The response is the upstream API's native body — UTCP is a *calling*
protocol, not an envelope. The only UTCP-specific signal is the
`x-utcp-tool` header the policy stamps on the upstream request.

> **Heads-up on path matching.** Flex Gateway forwards the API
> instance's proxy prefix as part of the path. Your tool URL and
> `discoveryPath` must include that prefix. For example, if your API
> instance is mounted at `/sap-orders/`, set
> `discoveryPath: /sap-orders/utcp` and tool URLs like
> `https://<host>/sap-orders/api/order`.

---

## Configuration reference

| Field | Type | Default | Notes |
|---|---|---|---|
| `manualSource` | enum | `openapi` | **v0.1 supports `static` only.** `openapi` and `hybrid` are accepted by the schema but rejected at policy load — see Roadmap. |
| `staticManual` | string (JSON) | `""` | Required when `manualSource=static`. Must parse as a JSON object with `utcp_version` and `tools[]`. |
| `discoveryPath` | string | `/utcp` | Where the Manual is served. Must include the API instance's proxy prefix. |
| `utcpVersion` | string | `1.0.1` | Value emitted as `utcp_version`. |
| `enforcementMode` | enum | `strict` | `strict`: 404 on unmatched. `permissive`: pass-through but audit-logged. |
| `validateInputs` | bool | `true` | Validate request body/path/query against the matched tool's `inputs` schema. |
| `maxBodyBytes` | int | 1048576 | Body cap when validation is enabled. Over this returns 413. Min 1 KiB, max 50 MiB. |
| `requirePrincipal` | bool | `false` | When `true`, missing `principalHeader` returns 401. |
| `principalHeader` | string | `x-anypoint-client-id` | Header an upstream auth policy populated. |
| `cacheControlHeader` | string | `public, max-age=60` | `Cache-Control` on the Manual response. |
| `toolHeaderName` | string | `x-utcp-tool` | Header stamped on upstream after a match. |
| `toolNamePrefix` | string | `""` | Prepended to every tool name in the Manual. |
| `openapiUrl` | string | `""` | **Reserved.** Ignored in v0.1. |
| `refreshIntervalSeconds` | int | 300 | **Reserved.** Ignored in v0.1. |
| `allowInsecureOpenapiUrl` | bool | `false` | **Reserved.** Ignored in v0.1. |

---

## Error responses

All error responses are `application/json`.

| Status | Body | When |
|---|---|---|
| 400 | `{"error":"utcp.invalid_json"}` | Body validation enabled but body is not valid JSON. |
| 400 | `{"error":"utcp.input_invalid","tool":"...","violations":[{"path":"/body/...","message":"..."}]}` | Body parsed, but does not satisfy the tool's `inputs` schema. |
| 401 | `{"error":"utcp.unauthenticated"}` | `requirePrincipal=true` and `principalHeader` is missing. |
| 404 | `{"error":"utcp.tool_not_declared"}` | `enforcementMode=strict` and no tool matches `(method, path)`. |
| 413 | `{"error":"utcp.body_too_large","limit":<bytes>}` | Body exceeds `maxBodyBytes`. |

---

## JSON Schema validator — what's supported

v0.1 ships a deliberately narrow JSON Schema subset, sized for the
keywords that real-world Manuals (and OpenAPI-derived Manuals) actually
use. Anything outside this list is treated as "no constraint", per the
JSON Schema unknown-keyword rule.

- **Types:** `string`, `integer`, `number`, `boolean`, `object`,
  `array`, `null` (single or array of types)
- **Object:** `required`, `properties`, `additionalProperties`
  (bool or schema)
- **Array:** `items`, `minItems`, `maxItems`
- **String:** `minLength`, `maxLength`, `pattern` (regex)
- **Numeric:** `minimum`, `maximum`, `exclusiveMinimum`,
  `exclusiveMaximum`
- **Any:** `enum`

A full JSON Schema 2020-12 validator is on the roadmap (see below); the
subset is the right tradeoff for v0.1 given WASM binary-size and regex
backend constraints.

---

## Audit log shape

Every request that the policy makes a decision on emits one line via
PDK's logger. Fields:

```
timestamp, request_id, principal, tool, method, path,
status, latency_ms, validation_status, bytes_in, bytes_out
```

Validation failures additionally include `violations[]` (capped at the
configured limit). Lines flow through whatever sink your gateway is
already configured for (stdout, Anypoint Monitoring, sidecar
collector — the policy doesn't pick).

---

## What's deferred from v0.1

The following items are explicitly out of scope for the initial
release. Each has a code seam in place so it can be added without
restructuring the policy.

1. **Live OpenAPI fetch (`openapi`/`hybrid` modes).** Requires PDK
   outbound HTTP service binding — own design pass.
2. **Periodic refresh + admin refresh endpoint.** The
   `refreshIntervalSeconds` field is parsed but not yet wired up.
3. **Full JSON Schema 2020-12 validator.** Subset ships today; the
   full validator gets gated behind a `validatorBackend` config knob
   once a WASM-clean crate is selected.
4. **Non-HTTP transports.** `cli`, `sse`, `mcp`, `grpc`, `websocket`
   tools are preserved in the Manual but not routed/validated.
5. **Discovery `ETag` / `If-None-Match`.** Bytes are deterministic;
   conditional GET is a small follow-up.
6. **Override-secret leakage validator (hybrid mode).** Reject literal
   `api_key` / `client_secret` values that aren't `${ENV_VAR}`
   placeholders.
7. **Manual size and tool-count guards.** `maxToolCount` /
   `maxManualBytes` knobs to fail load on hostile inputs.
8. **Per-tool quotas / RPS hooks.** Recipe doc layering rate-limit on
   top of `x-utcp-tool`.
9. **PDK unit-test harness coverage of the request filter.** Today's
   unit tests are pure-Rust; the filter flow is exercised end-to-end
   via the playground.
10. **Connected-mode (Anypoint Monitoring) audit emission.** Today
    `pdk::logger::*` lines only; structured audit emission is the
    follow-up.
11. **Config schema round-trip for `requirePrincipal` /
    `principalHeader`.** Cosmetic — both fields work end-to-end; the
    next `cargo anypoint gcl-gen` regeneration just needs a sanity
    check.
12. **Documentation: "convert at deploy time" recipe** for operators
    who want OpenAPI-driven Manuals before live fetch lands.

---

## Compatibility notes

- **PDK 1.8 / Flex Gateway** only. The policy compiles to
  `wasm32-wasip1`.
- Runs in **Connected Mode** (configured via API Manager) and
  **Local Mode** (configured via Local Mode YAML).
- Discovery body is byte-deterministic, so the same Manual hashes
  identically across reloads — useful for agents that cache by hash.
- The policy does **not** authenticate the agent and does **not**
  rate-limit. Compose it with the existing JWT / OAuth2 / mTLS
  policies and a rate-limit / quota policy keyed off `x-utcp-tool`.

---

## Support and feedback

This is an initial release; please open issues against your
organization's policy repo or reach out to the team owning the policy.
The Roadmap section above is the source of truth for what's coming.
