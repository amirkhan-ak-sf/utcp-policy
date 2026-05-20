# UTCP Bridge — Roadmap

Living list of items deferred from v0.2. Each entry has a short rationale, a
sketch of the chosen approach, and pointers into the code where the seam
already exists. Items are grouped roughly by theme; priority and ordering
will fall out of operator feedback.

Last updated: 2026-05-20

---

## What landed in v0.2

For context, the items below are *deferred from* the following v0.2 baseline:

- **Nested `upstreams[]` config.** One entry per upstream host; each carries
  its own `tools[]`. Each upstream is registered as a PDK `Service` at
  policy load (`src/lib.rs` `configure`).
- **Outbound HTTP from the policy itself.** `request_filter` issues the
  upstream call via `HttpClient::request(&service)…send(method)` and
  returns the upstream's response to the caller via `Flow::Break`
  (`src/lib.rs:request_filter`).
- **`apiInstanceProxyPath`.** The API instance's listener prefix is
  stripped from the inbound path before tool matching, so the configured
  tool paths describe the *upstream* not the gateway.
- **`outboundTimeoutSeconds`.** Bounded outbound timeout (1–300 s).
- **Header pass-through.** Caller headers (incl. `Authorization`) flow to
  the upstream unchanged; only RFC 7230 hop-by-hop headers are stripped.
  `Content-Type` is overridden to the matched tool's `contentType`.
- **Synthesised Manual.** The Manual is built once at policy load from
  `upstreams[].tools[]` (`src/manual_state.rs`).

---

## 1. Live OpenAPI fetch (auto-derive `inputs` from upstream OpenAPI)

**Status:** v0.2 takes `inputs` as a JSON-Schema string per tool. To
auto-populate that from the upstream's own `/openapi.json` is the next
ergonomics win.

**Why deferred:** PDK 1.8 outbound HTTP works (we use it on the request
path now), but doing it from `configure` cleanly — without blocking the
launcher if the upstream is slow / down at boot — is its own design pass:
  * Add an opt-in field per upstream (e.g. `openapiUrl` and optional
    auth).
  * Fetch on bootstrap with a bounded timeout.
  * Cache parse failures as a load error vs. retry on a backoff ladder.
  * Decide failure-mode policy: refuse to load vs. degraded mode that
    serves a Manual without `inputs` for the affected upstream.

**Code seams:**
  * `ManualState::build` (`src/manual_state.rs`) is the natural choke
    point — today it uses the operator-supplied `inputs` directly; an
    OpenAPI fetch would feed the same structure.

---

## 2. Periodic refresh + admin refresh endpoint

**Why:** OpenAPI specs and tool inventories change. Today the Manual is
built once at policy load.

**Plan sketch:**
  * Background tick using PDK timers. On tick, re-fetch (1) and
    atomically swap an `Rc<ManualState>` behind the request filter.
  * Admin endpoint at `<discoveryPath>/refresh` (POST, gated by
    `requirePrincipal` + an allowlist) that triggers an immediate
    refresh and returns the new tool count + ETag.
  * Emit an audit line on every refresh result (success / failure /
    no-op).

**Open questions:**
  * Do we want the swap to be all-or-nothing, or partial (keep old
    router for in-flight requests, route new ones against the new
    router)? `Rc` swap covers the simple case but can race;
    `arc-swap` or a generation counter would be cleaner.

---

## 3. Full JSON Schema 2020-12 validator

**Status:** v0.2 ships `src/validate/schema.rs`, a deliberately narrow
subset (type / required / properties / additionalProperties / items /
min-max length / min-max / pattern / enum / minItems / maxItems). The
header docstring lists exactly what is supported.

**Why deferred:** the operator-facing keyword set most Manuals emit is
small. Pulling in `jsonschema` (or `valico`, `boon`) adds:
  * Unverified `wasm32-wasip1` regex backend story.
  * Format validators that need to be deterministic across builds.
  * Significant binary size.

**Plan sketch:**
1. Build the policy with `jsonschema = { default-features = false }`
   targeting `wasm32-wasip1`, measure binary-size delta and verify
   regex-light works without `std::time`.
2. If acceptable, gate behind `validatorBackend: subset|full` config
   so operators can opt in.
3. Keep the subset implementation as the default for low-overhead
   paths.

**Code seams:**
  * `validate::validate_inputs` in `src/validate/schema.rs` is the
    only entry point — swapping the implementation behind the public
    function is a one-file change.

---

## 4. Non-HTTP transports (cli / sse / mcp / grpc / websocket)

**Status:** v0.2 only routes/validates HTTP tools. The Manual model in
`src/manual/model.rs` is HTTP-only by construction now.

**Why deferred:** Flex Gateway is HTTP-shaped; non-HTTP transports
require either out-of-band invocation (CLI/MCP) or a different listener
(gRPC, WebSocket). Properly enforcing on those surfaces means picking
a story for each.

**Plan sketch (per transport):**
  * **gRPC** — once the policy can run on a gRPC listener, route by
    `<service>/<method>` from `:path` and validate proto messages
    against the inputs schema.
  * **WebSocket** — match on the upgrade request, then leave the open
    connection alone (no per-message validation in v1).
  * **CLI / MCP** — out of scope for a network policy. Documented
    explicitly in REQUIREMENTS so operators don't expect coverage.

---

## 5. Discovery ETag and conditional GET

**Status:** v0.2 returns the Manual with `Cache-Control` from config
but no `ETag` / `If-None-Match` support.

**Plan sketch:** hash `state.manual_bytes` once at policy load, set
`ETag: "<hex>"` on the discovery response, short-circuit to `304` when
the request carries a matching `If-None-Match`. The bytes are already
deterministic (`render::to_json_bytes`) so the ETag is stable across
reloads of the same Manual.

**Code seams:**
  * The discovery short-circuit in `src/lib.rs:request_filter` is
    where to read `If-None-Match` and synthesize the 304.

---

## 6. Manual size and tool-count guards

**Why:** a hostile or runaway upstream tool list could blow up startup
memory. Today the policy will happily compose a 50k-tool Manual.

**Plan sketch:** add config knobs `maxToolCount` (default 2000) and
`maxManualBytes` (default 4 MiB), enforce in `ManualState::finalize`.
Fail load with an actionable error.

---

## 7. Per-tool quotas / RPS hooks

**Why:** the policy already tags requests with `x-utcp-tool` so a
downstream rate-limit / quota policy can scope per-tool. We do not
yet ship a sample showing that composition.

**Plan sketch:**
  * Add a playground recipe layering `rate-limit` after this policy
    and a `quota-enforcement` policy keyed off the `x-utcp-tool`
    header.
  * Document the integration in REQUIREMENTS once we've validated it
    on the local Flex container.

---

## 8. PDK unit-test harness coverage of the request filter

**Status:** today's tests are pure-Rust (router, schema, audit shapes,
URL composition). The actual `request_filter` flow is exercised via
the playground and a live API instance, not in CI.

**Plan sketch:** use `pdk-unit` (already a dev-dependency) to drive
`request_filter` with fabricated `RequestHeadersState` and a mock
`HttpClient`. Cover:
  * Discovery short-circuit (`GET <proxy>/<discoveryPath>` -> 200
    + Manual bytes).
  * Strict 404 on unmatched.
  * Validation 400 on schema violation.
  * 413 on body overflow.
  * `requirePrincipal=true` 401.
  * 504 on upstream timeout / failure.
  * Header pass-through (Authorization preserved, hop-by-hop
    stripped).

---

## 9. Connected-mode (Anypoint Monitoring) audit emission

**Status:** v0.2 emits `pdk::logger::*` lines only. That is enough
for local + connected stdout pipelines but doesn't surface as a
structured audit event in Anypoint Monitoring.

**Plan sketch:** mirror the access-log format declared in
`playground/config/logging.yaml` so that the JSON line carries
`tool`, `principal`, `status`, `upstream_url`, and
`validation_status` fields the gateway picks up without per-policy
plumbing.

---

## 10. Multi-instance Manual federation

**Why:** when an organisation has multiple UTCP Bridge instances
fronting different upstreams, agents currently fetch each Manual
separately. A federation endpoint that aggregates several Manuals
(or a discovery index) would simplify agent configuration.

**Plan sketch:** out of scope for this policy — solved at a higher
layer (an aggregator service or a UTCP-aware discovery proxy). Noted
here so operators know it's not on the policy roadmap.
