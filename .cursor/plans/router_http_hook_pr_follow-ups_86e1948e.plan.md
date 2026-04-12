---
name: Router HTTP hook PR follow-ups
overview: "Move telemetry initialization to RouterHttp so the hook is observable; add code and doc warnings about bypassing router protections; document layer vs plugin ordering. Optional: align with Renee's router-service refactor if timing allows."
todos: []
isProject: false
---

# Router HTTP hook (PR #8925) follow-up changes

Implement the following changes. The top-level RouterHttp service and license enforcement at that stage are already in place on the branch.

---

## 1. Move telemetry initialization to RouterHttp (required)

**Problem:** The telemetry plugin’s per-request setup (spans, context, router overhead tracker, custom attributes) runs in **router_service**. RouterHttp plugins run *before* that, so their execution is not visible in router telemetry (e.g. Datadog, OTLP).

**Approach:** Have the telemetry plugin also participate at RouterHttp so that by the time any RouterHttp plugin runs, telemetry is already initialized for the request (same pattern as license enforcement moving to the earlier service).

**Implementation:**

- In [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs), add **router_http_service** that wraps the inner service with the same per-request behavior that **router_service** currently has:
  - Create/use the router span (or equivalent for raw HTTP) so RouterHttp work is under a span.
  - Run the same request-time setup that lives in **router_service**’s `map_future_with_request_data` (e.g. supergraph_schema_id, client name/version, custom attributes, **router_overhead::RouterOverheadTracker**, RouterInstruments/Events on_request). RouterHttp plugins then run *after* telemetry init and are observable.
- Keep **router_service** behavior as-is for the rest of the Router pipeline (or refactor shared logic into a helper used by both **router_http_service** and **router_service** to avoid duplication).
- In [apollo-router/src/router_factory.rs](apollo-router/src/router_factory.rs), ensure telemetry is ordered early in the RouterHttp pipeline (e.g. before license_enforcement). Plugin list order already defines RouterHttp fold order.

---

## 2. Warnings when RouterHttp customizations are used

**Code warning:** When RouterHttp customizations are configured, log a one-time warning that requests passing through RouterHttp plugins run before router self-protection (traffic shaping, limits, etc.) and that users should understand the implications.

**When to warn (config-based):**

- Coprocessor has **router_http** configured: in [apollo-router/src/plugins/coprocessor/mod.rs](apollo-router/src/plugins/coprocessor/mod.rs), when `router_http != RouterHttpStage::default()` at init or when building router_http_service.
- Rhai: only if detectable without fragile parsing (optional for v1).

**Message (concept):** e.g. “RouterHttp customizations are in use. Requests that pass through RouterHttp plugins run before traffic shaping, limits, and other router protections. Ensure you understand the performance and security risks.” Optionally mention CSRF.

**Doc warnings:** Add a short caveat or warning block in:

- [docs/source/routing/request-lifecycle.mdx](docs/source/routing/request-lifecycle.mdx) (near the RouterHttp bullet and/or hook-point list).
- [docs/source/routing/customization/native-plugins.mdx](docs/source/routing/customization/native-plugins.mdx) (router_http_service).
- [docs/source/routing/customization/rhai/reference.mdx](docs/source/routing/customization/rhai/reference.mdx) and [index.mdx](docs/source/routing/customization/rhai/index.mdx) (router_http).
- [docs/source/routing/customization/coprocessor/reference.mdx](docs/source/routing/customization/coprocessor/reference.mdx) and [index.mdx](docs/source/routing/customization/coprocessor/index.mdx) (router_http stage).

Content: using RouterHttp means your code runs *before* router protections (traffic shaping, limits, CSRF, etc.); only use it if you understand and accept that risk (and can scale coprocessors if using them). Do not hide the feature from public docs; document it and warn.

---

## 3. Layers vs plugins ordering (clarify / document)

**Background:** Some HTTP server layers run before the RouterHttp plugin stack. Document or comment so implementors know the full order.

**Actions:**

- **Code:** In [apollo-router/src/services/router/service.rs](apollo-router/src/services/router/service.rs) (or where the HTTP stack leading to RouterHttpGate is built), add a short comment listing what runs before the RouterHttp pipeline (e.g. Axum routing, TraceLayer, license_handler, decompression from [apollo-router/src/axum_factory/axum_http_server_factory.rs](apollo-router/src/axum_factory/axum_http_server_factory.rs)).
- **Docs:** In [docs/source/routing/request-lifecycle.mdx](docs/source/routing/request-lifecycle.mdx), optionally add a sentence that HTTP server layers may run before the RouterHttp pipeline.

---

## 4. Tests (beyond existing branch tests)

**Existing on branch:** `router_http_ordering.rs` (RouterHttp before Router; default no-op does not break pipeline); `lifecycle.rs` (full ordering with Rhai/coprocessor, request/response visibility, Rhai-only router_http, coprocessor router_http, static landing skips RouterHttp); coprocessor unit tests for RouterHttp stages and config validation.

**Add:**

1. **Telemetry at RouterHttp:** Add a test that, with telemetry enabled, a request that passes through the RouterHttp pipeline (e.g. with a router_http hook) still produces the expected router span/trace so RouterHttp execution is observable. Follow patterns in [apollo-router/tests/integration/telemetry/otlp/tracing.rs](apollo-router/tests/integration/telemetry/otlp/tracing.rs) (e.g. OTLP + TraceSpec) or use the test harness with telemetry and a router_http hook and assert span/trace presence (e.g. via tracing subscriber or integration test that checks exported traces).
2. **Warning when router_http configured (optional):** If a one-time log warning is implemented for coprocessor router_http, add a test that the warning is emitted when coprocessor has router_http configured (e.g. assert log output or use a tracing subscriber in the test).

No new tests are required for doc-only or comment-only changes.

---

---

## Summary of deliverables


| Item                                                                                                   | Type                 | Where                                                                                                                                                                                                    |
| ------------------------------------------------------------------------------------------------------ | -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Telemetry plugin implements router_http_service and does per-request init so RouterHttp is observable  | Code                 | [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs); plugin order in [apollo-router/src/router_factory.rs](apollo-router/src/router_factory.rs)                     |
| One-time code warning when RouterHttp customizations are configured (at least coprocessor router_http) | Code                 | [apollo-router/src/plugins/coprocessor/mod.rs](apollo-router/src/plugins/coprocessor/mod.rs) or router_factory                                                                                           |
| Doc caveats: RouterHttp bypasses router protections                                                    | Docs                 | request-lifecycle, native-plugins, rhai reference/index, coprocessor reference/index                                                                                                                     |
| Comment (and optional doc) on what runs before RouterHttp (layers)                                     | Code + optional docs | [apollo-router/src/services/router/service.rs](apollo-router/src/services/router/service.rs) and/or axum factory; [docs/source/routing/request-lifecycle.mdx](docs/source/routing/request-lifecycle.mdx) |
| Test: RouterHttp execution observable in telemetry                                                     | Test                 | integration/telemetry or test harness                                                                                                                                                                    |
| Test (optional): warning logged when coprocessor router_http configured                                | Test                 | unit or integration                                                                                                                                                                                      |


