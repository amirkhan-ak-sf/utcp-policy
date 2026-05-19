# UTCP Manual Validator — Roadmap

Living list of items deferred from v0.1. Each entry has a short rationale, a
sketch of the chosen approach, and pointers into the code where the seam
already exists. Items are grouped roughly by theme; priority and ordering
will fall out of operator feedback.

Last updated: 2026-05-19

---

## 1. Live OpenAPI fetch (`manualSource=openapi` and `hybrid`)

**Status:** v0.1 returns a clear configuration error for `openapi`/`hybrid`
sources at policy load (`src/manual_state.rs`, `ManualState::for_source`).
Operators must convert at deploy time and supply `staticManual` for now.

**Why deferred:** PDK 1.8 outbound HTTP requires a registered `Service`
binding (the same plumbing `oauth-2-jwt-bearer` uses for IDP calls). Wiring
that in correctly means:
  * Adding a service stanza to the gcl definition (or auto-deriving one
    from `openapiUrl`).
  * Doing the fetch from inside `configure` *without* blocking the
    launcher indefinitely if the IDP is slow / down.
  * Caching parse failures as a load error vs. retrying.

That is enough surface area to merit its own design pass rather than
trickling in alongside the v0.1 routing/validation work.

**Plan sketch:**
1. Add a `services:` entry in `definition/gcl.yaml` keyed off a
   policy-level `openapiServiceRef` config field (or a synthesized name).
2. Inject the `HttpClient` already available in `on_request` into
   `ManualState::for_source` via a one-shot fetch path:
   `cfg.manual_source == OpenApi { ... }` -> issue
   `HttpClient::request(&service, GET openapiUrl)` synchronously inside a
   bootstrap step that runs before the first request is processed.
3. Cache the rendered Manual bytes + router on success. On bootstrap
   failure, fail closed (refuse all requests with `503 utcp.manual_unavailable`)
   and re-attempt on a backoff ladder.
4. Add a configuration-level `openapiAuth:` block (header / bearer / mTLS)
   so private OpenAPI endpoints work — many platforms gate `/openapi.json`
   behind the same auth as the rest of the API.

**Code seams that already exist:**
  * `from_inline_openapi` and `from_hybrid` in `src/manual_state.rs:36,64`
    are the constructors a fetch path can call once it has the spec.
  * `openapi::convert` is pure (no I/O) so it slots in once the bytes are
    in hand.

---

## 2. Periodic refresh + admin refresh endpoint

**Why:** OpenAPI specs change. `refreshIntervalSeconds` is parsed and
validated today (`src/config.rs:117`) but unused.

**Plan sketch:**
  * Background tick using PDK timers (the pattern `data-masking-policy`
    uses for cache TTL). On tick, re-fetch (1) and atomically swap an
    `Rc<ManualState>` behind the request filter.
  * Admin endpoint at `<discoveryPath>/refresh` (POST, gated by
    `requirePrincipal` + an allowlist) that triggers an immediate refresh
    and returns the new tool count + ETag.
  * Emit an audit line on every refresh result (success / failure / no-op).

**Open questions:**
  * Do we want the swap to be all-or-nothing, or partial (keep old router
    for in-flight requests, route new ones against the new router)? The
    `Rc` swap covers the simple case but can race; `arc-swap` or a
    generation counter would be cleaner.

---

## 3. Full JSON Schema 2020-12 validator

**Status:** v0.1 ships `src/validate/schema.rs`, a deliberately narrow
subset (type / required / properties / additionalProperties / items /
min-max length / min-max / pattern / enum / minItems / maxItems). The
header docstring lists exactly what is supported.

**Why deferred:** the operator-facing keyword set the OpenAPI converter
emits is small. Pulling in `jsonschema` (or `valico`, `boon`) adds:
  * Unverified `wasm32-wasip1` regex backend story.
  * Format validators that need to be deterministic across builds.
  * Significant binary size.

**Plan sketch:**
1. Build the policy with `jsonschema = { default-features = false }`
   targeting wasm32-wasip1, measure binary size delta and verify
   regex-light works without `std::time`.
2. If acceptable, gate it behind `validatorBackend: subset|full` config
   so operators can opt in.
3. Keep the subset implementation as the default for low-overhead paths
   (it covers the vast majority of OpenAPI-derived schemas).

**Code seams:**
  * `validate::validate_inputs` in `src/validate/schema.rs:37` is the only
    entry point — swapping the implementation behind the public function
    is a one-file change.

---

## 4. Non-HTTP transports (cli / sse / mcp / grpc / websocket)

**Status:** v0.1 only routes/validates `tool_call_template.call_template_type
= http` (`src/manual/model.rs`). The serialized Manual already preserves
non-HTTP entries faithfully, but the router will skip them and the policy
will not match incoming traffic to them.

**Why deferred:** Flex Gateway is HTTP-shaped; non-HTTP transports require
either out-of-band invocation (CLI/MCP) or a different listener (gRPC,
WebSocket). Properly enforcing on those surfaces means picking a story for
each.

**Plan sketch (per transport):**
  * **gRPC** — once the policy can run on a gRPC listener, route by
    `<service>/<method>` from `:path` and validate proto messages against
    the inputs schema.
  * **WebSocket** — match on the upgrade request, then leave the open
    connection alone (no per-message validation in v1).
  * **CLI / MCP** — out of scope for a network policy. Documented
    explicitly in REQUIREMENTS so operators don't expect coverage.

---

## 5. Discovery ETag and conditional GET

**Status:** v0.1 returns the Manual with `cache-control` from config but
no `ETag` / `If-None-Match` support.

**Plan sketch:** hash `state.manual_bytes` once at policy load, set
`ETag: "<hex>"` on the discovery response, short-circuit to `304` when
the request carries a matching `If-None-Match`. The bytes are already
deterministic (`render::to_json_bytes`) so the ETag is stable across
reloads of the same Manual.

**Code seams:**
  * `src/lib.rs:88` (the discovery short-circuit) is where to read
    `If-None-Match` and synthesize the 304.

---

## 6. Override-secret leakage validator (`hybrid` mode)

**Why:** in hybrid mode operators can patch `tool_call_template.auth` via
overrides. If they paste a literal API key by accident, it ends up in the
served Manual JSON.

**Plan sketch:** in `manual::overrides::apply_overrides`, scan
`auth.api_key` / `auth.client_secret` / `auth.password` / `auth.token`
fields and reject (load-time error) any value that is *not* of the form
`${ENV_VAR}` unless the operator sets `allowLiteralSecretsInManual=true`.
Audit-log the override key paths regardless.

**Code seams:**
  * `src/manual/overrides.rs` — the patch merger is the natural choke
    point.

---

## 7. Manual size and tool-count guards

**Why:** a hostile or runaway OpenAPI spec could blow up startup memory.
Today the policy will happily serialize a 50k-tool Manual.

**Plan sketch:** add config knobs `maxToolCount` (default 2000) and
`maxManualBytes` (default 4 MiB), enforce in `ManualState::finalize`. Fail
load with an actionable error.

---

## 8. Per-tool quotas / RPS hooks

**Why:** the policy already tags requests with `x-utcp-tool` so a
downstream rate-limit / quota policy can scope per-tool. We do not yet
ship a sample showing that composition.

**Plan sketch:**
  * Add a playground recipe layering `rate-limit` after this policy and
    a `quota-enforcement` policy keyed off the `x-utcp-tool` header.
  * Document the integration in REQUIREMENTS §10 (Observability) once
    we've validated it on the local Flex container.

---

## 9. PDK unit-test harness coverage of the request filter

**Status:** today's tests are pure-Rust (router, schema, converter, audit
shapes). The actual `request_filter` flow is exercised end-to-end via the
playground but not in CI.

**Plan sketch:** use `pdk-unit` (already a dev-dependency) to drive
`request_filter` with fabricated `RequestHeadersState` and assert on the
returned `Flow`. Cover:
  * Discovery short-circuit (`GET /utcp` -> 200 + manual bytes).
  * Strict 404 on unmatched.
  * Permissive pass-through on unmatched.
  * Validation 400 on schema violation.
  * 413 on body overflow.
  * `requirePrincipal=true` 401.

---

## 10. Connected-mode (Anypoint Monitoring) audit emission

**Status:** v0.1 emits `pdk::logger::*` lines only. That is enough for
local + connected stdout pipelines but doesn't surface as a structured
audit event in Anypoint Monitoring.

**Plan sketch:** mirror the access-log format declared in
`playground/config/logging.yaml` so that the JSON line carries
`tool`, `principal`, and `validation_status` fields the gateway picks up
without per-policy plumbing. Audit struct already exists
(`src/audit.rs:14`); we just need to actually emit it (today we emit
loose `logger::info!` strings).

---

## 11. Config schema — `requirePrincipal` and `principalHeader` round-trip

**Status:** both fields are accepted by `PolicyConfig::from_raw` and used
by the request filter. Confirm they round-trip through the codegen
`Config` once `make build` regenerates `src/generated/config.rs` against
`definition/gcl.yaml`.

**Why this is on the roadmap rather than done:** v0.1 ships a
hand-written placeholder `src/generated/config.rs` that mirrors the
fields data-masking-policy uses. The first `cargo anypoint gcl-gen` run
will overwrite it; we should diff that run and verify the policy still
compiles + passes tests.

---

## 12. Documentation: "convert at deploy time" recipe

While (1) is deferred, operators who want OpenAPI-driven manuals need a
documented recipe:

```
swagger-cli bundle openapi.yaml | utcp-cli openapi-to-manual > manual.json
# then paste into staticManual or set up CI to do it
```

Worth shipping as `docs/recipes/openapi-at-deploy-time.md` once the CLI
shape settles.
