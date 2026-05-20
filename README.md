# UTCP Bridge — MuleSoft Flex Gateway Policy

A custom Flex Gateway policy that turns any MuleSoft API instance into a
[Universal Tool Calling Protocol (UTCP)](https://github.com/universal-tool-calling-protocol/utcp-specification)
endpoint for AI agents — without changing the upstream APIs.

The policy hosts a UTCP **Manual** at `/utcp`, validates inbound agent
requests against each tool's JSON Schema, tags the matched call, and
forwards it to the configured upstream over the gateway's normal egress
path.

---

## Purpose

UTCP's premise is that AI agents call native HTTP endpoints directly,
discovering them via a self-describing Manual. That eliminates the
MCP-style wrapper layer — but it also eliminates the natural choke
point for **auth, quota, schema enforcement, and audit**.

A gateway *is* that choke point. The UTCP Bridge places those controls
where they belong without forcing every backend team to implement UTCP
themselves.

## Goals

| Goal | What it means in practice |
|---|---|
| **G1.** Discoverability | Serve a Manual at a stable path so any UTCP-aware agent can self-configure. |
| **G2.** Declarative tool surface | Operators list tools (name, method, path, JSON Schema) in policy config — no upstream changes. |
| **G3.** Inbound validation | Reject malformed agent calls (`400 utcp.input_invalid`) before they reach the backend. |
| **G4.** Routing safety | Reject calls to undeclared endpoints (`404 utcp.tool_not_declared`) in strict mode. |
| **G5.** Per-tool observability | Tag every call with `x-utcp-tool: <name>` so rate-limit / quota / audit policies can scope by tool. |
| **G6.** No-touch upstreams | The bridge issues outbound HTTP itself; the upstream API never learns it's serving an agent. |

## Business benefits

- **Faster agent enablement** — expose existing APIs to AI agents in minutes by attaching a policy, not by writing a new microservice.
- **Centralised governance** — auth, rate-limit, quota, audit, and PII redaction stack against the same policy chain that already protects the API.
- **Schema-enforced safety** — the LLM cannot send malformed payloads through; bad requests die at the gateway with deterministic error codes.
- **Per-tool quotas and billing** — `x-utcp-tool` is the dimension downstream policies key off, enabling granular cost control per agent action.
- **No vendor lock-in for agents** — UTCP is an open spec; any UTCP client (Python, JS, LangChain, custom) can fetch the Manual and call tools.
- **Drop-in for existing OpenAPI APIs** — tool definitions follow the same JSON Schema operators already write for OAS.

---

## Quickstart — Expose HTTPBin as UTCP tools

This walk-through wraps four [httpbin.org](https://httpbin.org) endpoints
as UTCP tools so an agent can discover and call them.

### 1. Configure the policy

Apply the policy to any Flex Gateway API instance with this config:

```json
{
  "discoveryPath": "/utcp",
  "apiInstanceProxyPath": "/httpbin-utcp",
  "publicBaseUrl": "https://gw.example.com/httpbin-utcp",
  "egressBaseUrl": "https://httpbin.org",
  "outboundTimeoutMs": 30000,
  "utcpVersion": "1.0.1",
  "manualTitle": "HTTPBin Tools",
  "manualInfoVersion": "1.0.0",
  "manualDescription": "Echo, status, delay, and UUID tools backed by httpbin.org.",
  "enforcementMode": "strict",
  "validateInputs": true,
  "tools": [
    {
      "name": "echoJson",
      "description": "Echo a JSON body back, including received headers and query.",
      "method": "POST",
      "path": "/anything",
      "contentType": "application/json",
      "bodyField": "body",
      "inputs": "{\"type\":\"object\",\"required\":[\"body\"],\"properties\":{\"body\":{\"type\":\"object\"}}}"
    },
    {
      "name": "generateUuid",
      "description": "Return a freshly generated v4 UUID.",
      "method": "GET",
      "path": "/uuid",
      "contentType": "application/json",
      "bodyField": "",
      "inputs": ""
    },
    {
      "name": "checkStatus",
      "description": "Probe a target HTTP status code (path param: code).",
      "method": "GET",
      "path": "/status/{code}",
      "contentType": "application/json",
      "bodyField": "",
      "inputs": "{\"type\":\"object\",\"required\":[\"code\"],\"properties\":{\"code\":{\"type\":\"string\",\"pattern\":\"^[1-5][0-9]{2}$\"}}}"
    },
    {
      "name": "delayResponse",
      "description": "Delay a response by N seconds (1-10).",
      "method": "GET",
      "path": "/delay/{seconds}",
      "contentType": "application/json",
      "bodyField": "",
      "inputs": "{\"type\":\"object\",\"required\":[\"seconds\"],\"properties\":{\"seconds\":{\"type\":\"string\",\"pattern\":\"^([1-9]|10)$\"}}}"
    }
  ]
}
```

### 2. Discover the Manual

```bash
curl -s https://gw.example.com/httpbin-utcp/utcp | jq
```

Response (abridged):

```json
{
  "manual_version": "1.0.0",
  "utcp_version": "1.0.1",
  "info": {
    "title": "HTTPBin Tools",
    "version": "1.0.0",
    "description": "Echo, status, delay, and UUID tools backed by httpbin.org."
  },
  "tools": [
    {
      "name": "echoJson",
      "description": "Echo a JSON body back, including received headers and query.",
      "inputs": { "type": "object", "required": ["body"], "properties": { "body": { "type": "object" } } },
      "tool_call_template": {
        "call_template_type": "http",
        "url": "https://gw.example.com/httpbin-utcp/anything",
        "http_method": "POST",
        "content_type": "application/json",
        "body_field": "body"
      }
    }
    // ...generateUuid, checkStatus, delayResponse
  ]
}
```

### 3. Call a tool — happy path

```bash
curl -s -X POST https://gw.example.com/httpbin-utcp/anything \
  -H 'content-type: application/json' \
  -d '{"hello":"world","n":42}' | jq
```

The bridge:
1. Strips the `/httpbin-utcp` proxy prefix.
2. Matches `(POST, /anything)` → tool `echoJson`.
3. Validates body against the schema.
4. Adds `x-utcp-tool: echoJson` and forwards to `https://httpbin.org/anything`.
5. Returns httpbin's echoed response to the caller.

### 4. Call with a path parameter

```bash
curl -s https://gw.example.com/httpbin-utcp/status/418
# → upstream returns 418 "I'm a teapot"
```

The router resolves `{code}` from the path; the schema's `pattern`
rejects anything that isn't a 3-digit code:

```bash
curl -s https://gw.example.com/httpbin-utcp/status/banana
# → 400 utcp.input_invalid
```

### 5. Call an undeclared route — strict rejection

```bash
curl -s https://gw.example.com/httpbin-utcp/headers
# → 404 utcp.tool_not_declared
```

In `permissive` mode the same call would pass through and be tagged
`utcp.unmatched=true` in audit logs.

### 6. Verify the audit tag

If you call the upstream directly with curl, httpbin echoes the
forwarded headers — you'll see `x-utcp-tool: echoJson` injected by the
bridge. Downstream rate-limit / quota policies can use this to bill or
throttle per tool.

---

## What's supported today

### UTCP spec coverage

| Spec area | Status |
|---|---|
| Manual JSON serving (`/utcp`) | ✅ |
| `manual_version`, `utcp_version`, `info`, `tools[]` | ✅ |
| Tool: `name`, `description`, `inputs` | ✅ |
| HTTP `tool_call_template` (`url`, `http_method`, `content_type`, `body_field`) | ✅ |
| Path params via `{name}` templates | ✅ |
| Query parameters in inputs schema | ✅ |
| JSON Schema subset: `type`, `required`, `properties`, `items`, `enum`, `pattern`, `minimum`/`maximum`, `minLength`/`maxLength`, `minItems`/`maxItems`, `additionalProperties` (boolean) | ✅ |
| Tool: `outputs` schema | ❌ (see roadmap) |
| Tool: `tags`, `average_response_size` | ❌ |
| HTTP CallTemplate: `headers`, `header_fields` | ❌ (see roadmap) |
| HTTP CallTemplate: `auth` block + `${ENV_VAR}` substitution | ❌ (see roadmap) |
| Non-HTTP transports (`cli`, `sse`, `streamable_http`, `text`, `mcp`) | ❌ |
| Variable substitution (`variables` block) | ❌ |
| Full JSON Schema 2020-12 (`$ref`, `oneOf`/`anyOf`/`allOf`, `format`, `if`/`then`/`else`) | ❌ |

### Bridge-only features (beyond the spec)

- Configurable discovery path + API-instance proxy prefix stripping.
- Path-template router with `literal-beats-param` precedence.
- Single-egress proxy mode — outbound HTTP issued by the policy itself
  via PDK `HttpClient` to `egressBaseUrl`.
- `enforcementMode: strict | permissive`.
- `requirePrincipal` enforcement against a configurable principal header.
- `x-utcp-tool` audit tag injection.
- Body-size cap with `413 utcp.body_too_large`.
- Configurable `Cache-Control` on the served Manual.
- Tool-name prefixing for federated multi-instance setups.

### Error response codes

| Status | Code | Trigger |
|---|---|---|
| 400 | `utcp.input_invalid` | Body / path / query fails the tool's `inputs` schema. |
| 401 | `utcp.unauthenticated` | `requirePrincipal=true` and principal header missing. |
| 404 | `utcp.tool_not_declared` | Strict mode + `(method, path)` doesn't match any tool. |
| 413 | `utcp.body_too_large` | Body exceeds `maxBodyBytes`. |
| 502 | `utcp.upstream_unavailable` | Outbound call to `egressBaseUrl` failed or timed out. |

---

## What's not supported

See [ROADMAP.md](./ROADMAP.md) for the full list with rationale and
implementation sketches. Highlights:

- **`outputs` schema in served Manual** — agents currently have no
  declared response shape.
- **`headers`, `header_fields` in `tool_call_template`** — operators
  cannot declare static or dynamic forwarded headers in the Manual.
- **`auth` block + `${ENV_VAR}` substitution** — Manual cannot declare
  upstream auth; the bridge passes through whatever Authorization the
  agent sends.
- **Non-HTTP transports** — gRPC, WebSocket, SSE, MCP, CLI, Text are
  all out of scope today.
- **Live OpenAPI fetch and periodic refresh** — Manual is built once at
  policy load.
- **Discovery `ETag` / `If-None-Match`** — no conditional GET support
  yet.

---

## Configuration reference

See [`definition/gcl.yaml`](./definition/gcl.yaml) for the full schema
surfaced in API Manager. The most important fields:

| Field | Required | Default | Notes |
|---|---|---|---|
| `tools[]` | ✅ | — | Per-tool name, method, path, contentType, bodyField, inputs (JSON Schema string). |
| `egressBaseUrl` | ✅ | — | Host-only base URL for outbound calls. |
| `discoveryPath` | | `/utcp` | Where to serve the Manual. |
| `apiInstanceProxyPath` | | `""` | Listener prefix to strip before tool matching. |
| `publicBaseUrl` | | `""` | Composes `tool_call_template.url` in the Manual. |
| `enforcementMode` | | `strict` | `strict` or `permissive`. |
| `validateInputs` | | `true` | Toggle JSON Schema enforcement. |
| `outboundTimeoutMs` | | `30000` | Per-call upstream timeout. |
| `maxBodyBytes` | | `1048576` | 1 MiB default; max 50 MiB. |
| `requirePrincipal` | | `false` | Reject if `principalHeader` missing. |
| `principalHeader` | | `x-anypoint-client-id` | Audit identity source. |
| `toolHeaderName` | | `x-utcp-tool` | Audit-tag header on forwarded request. |
| `toolNamePrefix` | | `""` | Prepend to every tool name (federation). |

---

## Architecture

```
┌─────────┐   GET /utcp           ┌───────────────────────┐
│  Agent  ├──────────────────────▶│  UTCP Bridge policy   │
│         │   ◀── Manual JSON ────│  (Flex Gateway WASM)  │
│  (LLM)  │                       │                       │
│         │   POST /tool/path     │   1. strip proxy prefix
│         ├──────────────────────▶│   2. router lookup     │
│         │                       │   3. JSON Schema check │
│         │                       │   4. tag x-utcp-tool   │──┐
│         │                       │                       │  │  HttpClient
│         │   ◀── upstream resp ──│                       │◀─┘  (egressBaseUrl)
└─────────┘                       └───────────────────────┘
                                                              ┌────────────┐
                                                              │  Upstream  │
                                                              │  (HTTPBin) │
                                                              └────────────┘
```

The bridge does **not** rely on Flex Gateway's normal upstream cluster
forwarding. Instead it issues an outbound call to `egressBaseUrl`
(typically the gateway's own external hostname) so the bridge can stack
against sibling API instances on the same gateway.

---

## Building and publishing

```bash
make build       # cargo build, generate config bindings
make test        # cargo test
make release     # publish .wasm and .yaml to Anypoint Exchange
```

The policy targets `wasm32-wasip1` via PDK 1.8. See
[REQUIREMENTS.md](./REQUIREMENTS.md) for the design contract and
[ROADMAP.md](./ROADMAP.md) for deferred work.

---

## License

Internal MuleSoft policy. Not currently distributed externally.
