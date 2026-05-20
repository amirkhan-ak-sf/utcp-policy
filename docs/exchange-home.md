# UTCP Bridge

A MuleSoft Flex Gateway policy that turns one API instance into a
UTCP-compliant front door for one or more upstream services. The
policy hosts a UTCP **Manual** at a configurable discovery path,
synthesises that Manual from the configured `upstreams[]`, and on
every inbound request matches `(method, path)` against a declared
tool, validates inputs, then issues the outbound HTTP call to the
matched upstream itself.

> **What is UTCP?** The Universal Tool Calling Protocol is a JSON
> discovery-and-call contract for AI agents. A "Manual" is a JSON
> document listing tools and how to invoke them on their *native*
> transport (HTTP today). Agents fetch the Manual once, then call
> tools directly. Because Flex Gateway already terminates the
> underlying traffic, it is the natural place to publish the contract
> and enforce conformance.

---

## At a glance

| | |
|---|---|
| **Policy ID** | `utcp-manual-validator-policy` (definition) / `utcp-manual-validator-policy-impl` (implementation) |
| **Display name** | UTCP Bridge |
| **Runtime** | Flex Gateway via PDK 1.8 (wasm32-wasip1) |
| **Injection point** | inbound |
| **Interface scope** | api, resource |
| **Category** | Security |
| **Version** | 0.2.1 |

---

## What the policy does

1. **Hosts the Manual.** `GET <apiInstanceProxyPath><discoveryPath>`
   (default `/utcp` after stripping the API instance prefix) returns
   a UTCP Manual JSON document. The Manual is built once at policy
   load from the configured `upstreams[]`.
2. **Routes each tool call.** Every non-discovery request is matched
   against `(method, path-template)` from the declared tools. Path
   placeholders like `/users/{id}` match a single non-slash segment
   and the captured value is forwarded to the upstream verbatim.
3. **Validates inputs.** When a tool matches, the request body /
   path / query is validated against the tool's `inputs` JSON Schema
   before the outbound call is made (subject to `validateInputs`).
4. **Calls the upstream itself.** The policy registers each
   `upstreams[].host` as a PDK `Service` and issues the outbound
   HTTP request via `HttpClient`. Caller headers (including
   `Authorization`, custom auth headers, tracing) are forwarded
   unchanged. The upstream response is returned to the caller as-is —
   UTCP is a *calling* protocol, not an envelope.
5. **Tags successful matches.** A successful match stamps
   `x-utcp-tool: <tool-name>` (configurable via `toolHeaderName`)
   on the outbound request *and* the response so downstream
   rate-limit / quota / audit policies can scope per tool.

The policy never holds upstream credentials. Authentication of the
agent itself is delegated to other policies (JWT, OAuth2, mTLS)
configured upstream of this one in the policy chain; authorization
to call the upstream is whatever the agent passes through
(`Authorization`, `x-api-key`, ...).

---

## Quick start

### 1. Apply the policy to an API instance

In API Manager, open your Flex-deployed API instance and apply the
**UTCP Bridge** policy from Exchange. A minimal config that fronts a
single upstream with one tool:

```json
{
  "discoveryPath": "/utcp",
  "apiInstanceProxyPath": "/erp",
  "manualTitle": "UTCP — Sales & Distribution",
  "manualInfoVersion": "1.0.0",
  "manualDescription": "Check inventory, create orders in SAP ECC.",
  "upstreams": [
    {
      "host": "https://sap-orders-sys-api.example.com",
      "tools": [
        {
          "name": "createSalesOrder",
          "description": "Create a sales order in SAP ECC.",
          "method": "POST",
          "path": "/api/order",
          "inputs": "{\"type\":\"object\",\"required\":[\"body\"],\"properties\":{\"body\":{\"type\":\"object\",\"required\":[\"SALES_ORG\",\"DISTR_CHAN\",\"DIVISION\",\"PURCH_NO\",\"MATERIAL\",\"PLANT\",\"TARGET_QTY\",\"CUSTOMER\"],\"properties\":{\"SALES_ORG\":{\"type\":\"string\",\"minLength\":1},\"DISTR_CHAN\":{\"type\":\"string\",\"minLength\":1},\"DIVISION\":{\"type\":\"string\",\"minLength\":1},\"PURCH_NO\":{\"type\":\"string\",\"minLength\":1},\"MATERIAL\":{\"type\":\"string\",\"minLength\":1},\"PLANT\":{\"type\":\"string\",\"minLength\":1},\"TARGET_QTY\":{\"type\":\"string\",\"minLength\":1},\"CUSTOMER\":{\"type\":\"string\",\"minLength\":1}}}}}"
        }
      ]
    }
  ]
}
```

Multi-upstream is the same shape with more entries — every tool,
across all upstreams, must have a unique `name` since that's how
calls are routed and tagged.

### 2. Discover the Manual

```bash
curl -i https://<gateway-host>/erp/utcp
```

Returns the synthesised Manual with `Content-Type: application/json`
and the configured `Cache-Control`. Each tool's `tool_call_template.url`
is composed from `upstreams[].host` + `tools[].path`.

### 3. Call a tool

```bash
curl -i -X POST https://<gateway-host>/erp/api/order \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <agent-token>' \
  -d '{ "SALES_ORG": "3000", "DISTR_CHAN": "10", "DIVISION": "00",
        "PURCH_NO": "Cake", "MATERIAL": "MULETEST0", "PLANT": "3000",
        "TARGET_QTY": "1", "CUSTOMER": "0000000007" }'
```

Flow on the gateway:

1. `/erp/api/order` → strip `apiInstanceProxyPath=/erp` → local
   path `/api/order`.
2. Match `(POST, /api/order)` → tool `createSalesOrder` on upstream
   `https://sap-orders-sys-api.example.com`.
3. Validate body against the tool's `inputs` schema.
4. Issue outbound `POST https://sap-orders-sys-api.example.com/api/order`
   with caller headers preserved (minus hop-by-hop headers).
5. Return the upstream's status, headers, and body to the caller,
   plus `x-utcp-tool: createSalesOrder`.

> **Heads-up on the proxy prefix.** Flex Gateway forwards the API
> instance's listener path as part of the inbound URL. Set
> `apiInstanceProxyPath` to that prefix so the policy strips it before
> matching. The Manual is then reachable at
> `<gateway-host><apiInstanceProxyPath><discoveryPath>` and tool URLs
> in the served Manual reflect the *upstream* host, not the gateway.

---

## Configuration reference

### Top-level

| Field | Type | Default | Notes |
|---|---|---|---|
| `upstreams` | array (object) | — (required) | One entry per upstream host. See below. |
| `discoveryPath` | string | `/utcp` | Path the Manual is served on (after `apiInstanceProxyPath` stripping). |
| `apiInstanceProxyPath` | string | `""` | API instance listener path prefix. Stripped from inbound paths before tool matching. |
| `utcpVersion` | string | `1.0.1` | Value emitted as `utcp_version` in the Manual. |
| `manualTitle` | string | `""` | `info.title` of the synthesised Manual. |
| `manualInfoVersion` | string | `""` | `info.version` of the synthesised Manual. |
| `manualDescription` | string | `""` | `info.description` of the synthesised Manual. |
| `enforcementMode` | enum | `strict` | `strict`: 404 on unmatched. `permissive`: still rejects (the policy owns routing) but tags the audit log with `utcp.unmatched=true`. |
| `validateInputs` | bool | `true` | Validate request against the matched tool's `inputs` schema. |
| `maxBodyBytes` | int | 1048576 | Body cap when validation is enabled. Over → 413. Min 1 KiB, max 50 MiB. |
| `outboundTimeoutSeconds` | int | 30 | Timeout on the outbound call. On timeout → 504. Range 1–300. |
| `requirePrincipal` | bool | `false` | When `true`, missing `principalHeader` → 401. |
| `principalHeader` | string | `x-anypoint-client-id` | Header carrying the authenticated principal. |
| `cacheControlHeader` | string | `public, max-age=60` | `Cache-Control` on the Manual response. |
| `toolHeaderName` | string | `x-utcp-tool` | Header stamped on outbound (and the response) after a successful match. |
| `toolNamePrefix` | string | `""` | Prepended to every tool name in the served Manual (lets multiple instances share an agent without collisions). |

### `upstreams[]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `host` | string | — (required) | Upstream URL — scheme + host, optionally + base path. Registered as a PDK `Service` at policy load. |
| `tools` | array (object) | `[]` | Tools served on this upstream. At least one is required per upstream. |

### `upstreams[].tools[]`

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | — (required) | Tool name. Globally unique across all upstreams. |
| `path` | string | — (required) | Path on the upstream (must start with `/`). May contain `{param}` placeholders forwarded verbatim. |
| `description` | string | `""` | Free-form. Surfaces in the Manual. |
| `method` | string | `POST` | Common values: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`. |
| `contentType` | string | `application/json` | `Content-Type` set on the outbound request. |
| `bodyField` | string | `body` | Inputs-schema field that carries the outbound body. Empty disables body. |
| `inputs` | string (JSON) | `""` | JSON Schema for inputs as a JSON string. Empty disables validation for this tool. |

---

## Error responses

All error responses are `application/json`.

| Status | Body | When |
|---|---|---|
| 400 | `{"error":"utcp.input_invalid","tool":"...","violations":[{"path":"/body/...","message":"..."}]}` | Body parsed, but does not satisfy the tool's `inputs` schema. |
| 401 | `{"error":"utcp.unauthenticated"}` | `requirePrincipal=true` and `principalHeader` is missing. |
| 404 | `{"error":"utcp.tool_not_declared"}` | No tool matches `(method, path)`. |
| 413 | `{"error":"utcp.body_too_large","limit":<bytes>}` | Body exceeds `maxBodyBytes`. |
| 500 | `{"error":"utcp.upstream_misconfigured"}` | Internal: matched tool refers to an upstream with no registered Service (configuration drift). |
| 504 | `{"error":"utcp.upstream_timeout"}` | The outbound call to the upstream failed or timed out. |

---

## JSON Schema validator — what's supported

v0.2 ships a deliberately narrow JSON Schema subset, sized for the
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

A full JSON Schema 2020-12 validator is on the roadmap; the subset
is the right tradeoff given WASM binary-size and regex backend
constraints.

---

## Header forwarding

Inbound caller headers are forwarded to the upstream unchanged
**except** for hop-by-hop / proxy-internal headers, which are
stripped per RFC 7230:

```
host, connection, keep-alive, transfer-encoding, content-length,
upgrade, proxy-authenticate, proxy-authorization, te, trailer
```

`Content-Type` is overridden to the matched tool's `contentType`.
The matched tool name is tagged on both the outbound request and
the eventual response under `<toolHeaderName>`.

---

## Audit log shape

Every request that the policy makes a decision on emits one line
via PDK's logger. The lines flow through whatever sink your gateway
is already configured for (stdout, Anypoint Monitoring, sidecar
collector — the policy doesn't pick).

Common fields: `tool`, `method`, `path`, `principal`, `status`,
`upstream_url`. Validation failures additionally include
`violations[]`.

---

## What's deferred from v0.2

The following items are explicitly out of scope. Each has a code
seam in place so it can be added without restructuring the policy.

1. **Live OpenAPI fetch** — Manuals are synthesised from
   `upstreams[]` today. Auto-deriving the per-tool `inputs` schema
   from a live `/openapi.json` endpoint is a follow-up.
2. **Periodic refresh + admin refresh endpoint** — Manual is built
   once at policy load.
3. **Full JSON Schema 2020-12 validator** — subset ships today; the
   full validator gets gated behind a `validatorBackend` config knob
   once a WASM-clean crate is selected.
4. **Non-HTTP transports** — `cli`, `sse`, `mcp`, `grpc`,
   `websocket` — Flex Gateway is HTTP-shaped; out of scope.
5. **Discovery `ETag` / `If-None-Match`** — bytes are deterministic;
   conditional GET is a small follow-up.
6. **Manual size and tool-count guards** — `maxToolCount` /
   `maxManualBytes` knobs to fail load on hostile inputs.
7. **Per-tool quotas / RPS hooks** — recipe doc layering rate-limit
   on top of `x-utcp-tool`.
8. **PDK unit-test harness coverage of the request filter** — today's
   unit tests are pure-Rust; the filter flow is exercised via the
   playground and a live API instance.
9. **Connected-mode (Anypoint Monitoring) audit emission** — today
   `pdk::logger::*` lines only; structured audit emission is a
   follow-up.

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

Open issues against your organization's policy repo or reach out to
the team owning the policy. The "What's deferred" section above is
the source of truth for what's coming.
