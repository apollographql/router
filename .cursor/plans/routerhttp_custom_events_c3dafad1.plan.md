---
name: RouterHttp custom events
overview: Add `router_http` as a first-class instrumentation stage with its own span and events (like router, supergraph, subgraph, connector)—dedicated router_http span, standard and custom events, and span attributes at router_http_service.
todos: []
isProject: false
---

# Add router_http instrumentation (span + events)

Add `router_http` as a **first-class instrumentation stage** on the same footing as `router`, `supergraph`, `subgraph`, and `connector`: a **dedicated router_http span** and **router_http events** (standard request/response/error plus custom events, conditions, attributes, selectors). Router_http is **not** a special type of router_service—it has its own config keys under `telemetry.instrumentation.spans` and `telemetry.instrumentation.events`, its own span name (`ROUTER_HTTP_SPAN_NAME`), and wiring in the telemetry plugin at the `router_http_service` hook so that both standard events and custom events (and span attributes) are associated with the router_http span.

**RouterHttp is HTTP-focused.** The RouterHttp stage runs at the raw HTTP layer (before the GraphQL request is parsed). Instrumentation and tests for router_http should emphasize **HTTP request/response** (e.g. request/response headers, HTTP method, status code, body size, URI) rather than GraphQL-specific concepts. GraphQL-based conditions or error patterns may still be used where the request/response types expose them, but the primary focus is HTTP.

---

## Clarification: what “router” refers to

- **Event names like `"router.response"`** refer to the **request lifecycle stage** (the `router_service` hook), not the entire Apollo Router product or a generic “apollo_router” span. Same for `"supergraph.request"`, `"subgraph.response"`, `"connector.http.error"` — each is the **stage** that emitted the event.
- **Spans:** We add a **dedicated router_http span** (`ROUTER_HTTP_SPAN_NAME`) so the RouterHttp segment has its own span in the trace. The hierarchy at this layer is **router_http span (outer) → router span (inner)**. Router_http events and span attributes are attached to the router_http span; router events and span attributes stay on the router span. **ROUTER_SPAN_NAME** remains the span for the router (router_service) segment.

---

## Minimal change: no refactor of other stages

**Do not** refactor supergraph, subgraph, or connector. Leave their hardcoded event/span names as-is. Only add router_http with its own span and events; use hardcoded `"router_http"` where needed (same pattern as other plugins).

- **Router_http span:** Use constant `ROUTER_HTTP_SPAN_NAME` when creating the span.
- **Router_http events:** Minimal code change: only in router/events.rs add an **event prefix** parameter to `on_response` and `on_error`. Router call sites pass `"router"`; router_http call site passes `"router_http"`. **No changes to supergraph, subgraph, or connector.**
- **Context keys:** Add distinct context keys for router_http and a stage parameter only in router `on_request`. Do not change other stages.

---

## 0. Dedicated router_http span (constants, config, creation, wiring)

**Goal:** A dedicated span for the RouterHttp stage so traces show both a **router_http** span and a **router** span. Standard and custom events for router_http are attached to the router_http span.

- **Constants** — [apollo-router/src/plugins/telemetry/consts.rs](apollo-router/src/plugins/telemetry/consts.rs): Add `ROUTER_HTTP_SPAN_NAME: &str = "router_http"`. Add it to `BUILT_IN_SPAN_NAMES`.
- **Spans config** — [apollo-router/src/plugins/telemetry/config_new/spans.rs](apollo-router/src/plugins/telemetry/config_new/spans.rs): Add `router_http: RouterSpans` as a peer of `router`, etc. Call `router_http.defaults_for_levels(...)` in `update_defaults()` and validate `router_http.attributes` in `validate()`. So `telemetry.instrumentation.spans.router_http` is a first-class key with the same shape as `router`.
- **Span creation** — [apollo-router/src/plugins/telemetry/span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs): Add `create_router_http<B>(&self, request: &http::Request<B>) -> Span` using `ROUTER_HTTP_SPAN_NAME`, mirroring `create_router` with appropriate attributes for the RouterHttp segment.
- **Wiring in router_http_service** — [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs): Create **router_http span as outer**, **router span as inner**. Add an outer `InstrumentLayer` that creates the router_http span (e.g. `span_mode.create_router_http(&request.router_request)`), then the existing `InstrumentLayer` for the router span. Run router_http span attributes and router_http events (on_request) in the router_http span context; run router_http on_response/on_error when back in the router_http span (e.g. in an outer response handler after the inner future completes). This may require two-level wrapping: outer layer creates router_http span and runs router_http on_request, then calls inner (router span + router events); inner future completion runs router on_response, then control returns to outer to run router_http on_response/on_error. Ensure router_http events and span attributes are recorded on the router_http span.
- **Downstream span names:** If [apollo_telemetry.rs](apollo-router/src/plugins/telemetry/tracing/apollo_telemetry.rs), [datadog/mod.rs](apollo-router/src/plugins/telemetry/tracing/datadog/mod.rs), or [apollo_otlp_exporter.rs](apollo-router/src/plugins/telemetry/apollo_otlp_exporter.rs) switch on span name or list known spans, add `ROUTER_HTTP_SPAN_NAME` / `"router_http"` where appropriate (e.g. REPORTS_INCLUDE_SPANS, resource mappings, export).

---

## 1. Config: add `router_http` as its own events stage

**File: [apollo-router/src/plugins/telemetry/config_new/events.rs](apollo-router/src/plugins/telemetry/config_new/events.rs)**

- Add `router_http` as a **peer** of `router`, `supergraph`, `subgraph`, and `connector` in the `Events` struct:
  - `router_http: Extendable<RouterEventsConfig, Event<RouterAttributes, RouterSelector>>`
  (Same config shape as `router` because both use `router::Request`/`router::Response`; it is still its own stage, not a sub-key of `router`.)
- Add `new_router_http_events(&self) -> RouterEvents` that builds from `self.router_http` only (same logic as `new_router_events`, but reading from `self.router_http`: standard request/response/error from `router_http.attributes` + custom events from `router_http.custom`).
- In `validate()`: validate `router_http` the same way as `router` (request/response stages, custom event validation) and include it in error messages as its own stage.

No new event config or selector types are required: `router_http` reuses `RouterEventsConfig` and `RouterSelector`/`RouterAttributes` by convention (same request/response types at that layer), but in YAML and in code it is a separate stage key, e.g. `telemetry.instrumentation.events.router_http`.

---

## 2. Event prefix and context keys (router and router_http only)

**Scope:** Only [config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs) and its call sites in `mod.rs` (and fmt_layer if it calls router events). **Do not change** supergraph, subgraph, or connector event code.

- **Router (and router_http):** [apollo-router/src/plugins/telemetry/config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs)  
  - Add an **event prefix** parameter (e.g. `event_prefix: &str`) to the methods that call `log_event`: `on_response` and `on_error`. Use `format!("{}.response", event_prefix)` and `format!("{}.error", event_prefix)`. Router call site passes `"router"`; router_http call site passes `"router_http"`.
  - Add a stage parameter to `on_request` so that when storing display request/response in context we use the right key: router and router_http run in the same layer and share context. Pass a stage (e.g. enum `Router` | `RouterHttp`) and insert `DisplayRouterRequest`/`DisplayRouterResponse` for `Router` and `DisplayRouterHttpRequest`/`DisplayRouterHttpResponse` for `RouterHttp`.

**Context keys:** Any code that reads these (e.g. [router/selectors.rs](apollo-router/src/plugins/telemetry/config_new/router/selectors.rs), [fmt_layer.rs](apollo-router/src/plugins/telemetry/fmt_layer.rs)) must handle both router and router_http display types where relevant.

---

## 3. Wire router_http span and events in the telemetry plugin

**File: [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs)**

- In `router_http_service` (see also §0 for span hierarchy):
  - **Span order:** Outer layer creates the **router_http** span; inner layer creates the **router** span. Router_http events and span attributes run in the router_http span context; router events and span attributes run in the router span context.
  - **Event order: router_http before router.** In the closure that runs on request, call `router_http_events.on_request(request, stage_for_router_http)` first (in router_http span context), then `custom_events.on_request(request, stage_for_router)` (in router span context). On response/error, call `router_http_events.on_response` / `on_error` first (in router_http span context), then `custom_events.on_response` / `on_error`.
  - Extend the tuple carried into the future to include `router_http_events` (and any router_http span attribute state). Ensure router_http on_response/on_error run in the outer layer so they are attached to the router_http span.

Document and test that the **router_http** span exists and that **router_http** events (standard + custom) run before **router** events and are attached to the router_http span.

---

## 4. Tests

### New tests to add

- **Config and validation:** Test that `telemetry.instrumentation.events.router_http` is deserialized and validated like `router` (e.g. invalid conditions or unknown fields produce the expected errors). Reuse or mirror tests in [apollo-router/src/plugins/telemetry/config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs) and [apollo-router/src/plugins/telemetry/config_new/events.rs](apollo-router/src/plugins/telemetry/config_new/events.rs).
- **Integration:** Add a test that enables both `router` and `router_http` (events and, if applicable, span attributes), sends a request through the RouterHttp pipeline, and asserts that: (1) a **router_http span** is present in the trace, (2) both `router.response` and `router_http.response` (or equivalent) appear and **router_http events run before router events**, and (3) router_http events are attached to the router_http span.
- **Unit:** If `fmt_layer` or selectors are updated to handle router_http display types, add unit tests for the new behavior and update any snapshots that include router event names.

### Tests to mirror from router and supergraph

Add router_http equivalents of the following so router_http has the same test coverage as router and supergraph.

**Events (mirror [config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs), but HTTP-focused):** Router_http runs at the raw HTTP layer, so its tests should focus on **HTTP request/response** (headers, method, status code, body size, URI), not GraphQL-specific behavior. Add router_http event tests that:

- `**test_router_http_events`** — Config with `events.router_http` (e.g. standard request/response events, or custom events gated by **request_header** / **response_header**). Call through `router_http_service` with an HTTP request (e.g. specific method, headers). Assert snapshot includes `kind: router_http.response` (or request/error) and that conditions are driven by **HTTP** (e.g. a header like `x-log-request` / `x-log-response`), not GraphQL.
- `**test_router_http_events_response`** — Successful HTTP response (e.g. 200, response headers) and assert router_http response event is emitted with expected HTTP attributes (e.g. status, response headers).
- `**test_router_http_events_error`** — Error path (e.g. failed inner service or HTTP error response). Assert router_http error event is emitted. We can use the same error-context patterns as router for consistency, but the scenario should be understandable as an HTTP-level success vs failure (e.g. status code, or error from the pipeline).

Avoid centering router_http tests on GraphQL error context or operation names; prefer **request_header**, **response_header**, HTTP status, method, and body size where possible. Optionally add `test_router_http_events_with_exists_condition` using an HTTP-oriented condition (e.g. exists on a request header).

**Spans (mirror [config_new/router/spans.rs](apollo-router/src/plugins/telemetry/config_new/router/spans.rs) and [config_new/supergraph/spans.rs](apollo-router/src/plugins/telemetry/config_new/supergraph/spans.rs)):** Router and supergraph have unit tests on the **spans config type** (e.g. `RouterSpans`, `SupergraphSpans`) for default attribute levels and custom attributes. Router_http uses the same **RouterSpans** type for `config.instrumentation.spans.router_http`, so the existing router span tests already cover the attribute logic. Add one or both of:

- **Config path:** A test that the global `Spans` struct deserializes with `spans.router_http` and that `update_defaults()` / `validate()` run for `router_http` (e.g. in [config_new/spans.rs](apollo-router/src/plugins/telemetry/config_new/spans.rs): test that a YAML with `telemetry.instrumentation.spans.router_http` and e.g. `default_attribute_requirement_level` applies to router_http, or that invalid `router_http` attributes fail validation).
- **Levels (optional):** If we want parity with router span tests, add `test_router_http_spans_level_none`, `test_router_http_spans_level_required`, `test_router_http_spans_level_recommended` that build or deserialize `Spans` with `router_http` populated and assert the same default-attribute behavior as the existing `test_router_spans_level`_* (same assertions, but using `config.spans.router_http`).

**Span factory (mirror [span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs)):** Router has `test_specific_span` and `test_http_route_on_array_of_router_spans` that assert a span named `ROUTER_SPAN_NAME` is created with expected fields. Add:

- A test that `create_router_http(&request)` returns a span with name `ROUTER_HTTP_SPAN_NAME` and **HTTP-oriented** fields (e.g. `http.route`, `http.request.method`), consistent with the HTTP layer. Optionally extend the parameterized “array of spans” style test to include router_http (e.g. test both `create_router` and `create_router_http` for the same request).

**Integration (span names):** If [tests/integration/telemetry/datadog.rs](apollo-router/tests/integration/telemetry/datadog.rs) or OTLP tests assert on the list of default span names (e.g. `test_default_span_names`), add `router_http` to the expected set after adding `ROUTER_HTTP_SPAN_NAME` to `BUILT_IN_SPAN_NAMES` (and to any exporter/include list).

### Existing tests that would fail or need updates after the changes

- **Compilation (signature changes):** Adding an `event_prefix` (and stage for `on_request`) parameter only in [config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs) affects only router and router_http call sites. Update: [mod.rs](apollo-router/src/plugins/telemetry/mod.rs) router and router_http call sites to pass `"router"` and `"router_http"`; [fmt_layer.rs](apollo-router/src/plugins/telemetry/fmt_layer.rs) router event calls to pass the prefix/stage if it exercises router events.
- **Config structs:** Adding `router_http` to `Events` and `Spans` requires that both have `Default` (or the field is optional). Existing tests that deserialize YAML without `router_http` should still pass as long as `router_http` defaults (e.g. `Extendable` and `RouterSpans` default). Tests that construct `Events` or `Spans` in Rust may need to supply `router_http` or rely on `Default`.
- **Snapshot tests (event “kind”):** If we change how event names are produced (e.g. from a parameter), existing snapshots that contain `kind: router.response` should still match as long as we pass `"router"` for the router stage. Snapshots that capture full trace/log output and assume a single “router” span may need updating once a **router_http** span is added (e.g. one more span in the trace, or different span order). Specifically:
  - [config_new/router/snapshots/...router_events_graphql_response@logs.snap](apollo-router/src/plugins/telemetry/config_new/router/snapshots/) and `...router_events_graphql_error@logs.snap` — contain `kind: router.response`; should still pass if router events keep that kind.
  - [config_new/snapshots/...router_events_graphql_*.snap](apollo-router/src/plugins/telemetry/config_new/snapshots/) — same.
  - [apollo-router/src/plugins/telemetry/snapshots/...fmt_layer__tests__*_logging_with_custom_events*.snap](apollo-router/src/plugins/telemetry/snapshots/) — contain `kind=router.response`; update if we add router_http events to the test config and need to assert on both kinds or new span.
- **Span / trace structure:** Tests that assert on the exact number of spans, span names, or span hierarchy may fail once the **router_http** span is introduced (e.g. one extra span, or router_http as parent of router). Review:
  - [apollo-router/tests/integration/telemetry/otlp/tracing.rs](apollo-router/tests/integration/telemetry/otlp/tracing.rs) — `test_router_http_observable_in_telemetry` and any test that asserts on “router” span or trace shape; update to allow or assert on the router_http span as well.
  - [apollo-router/src/plugins/telemetry/span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs) — tests only `create_router` and `create_request`; adding `create_router_http` does not break them unless we add tests that assert on the total set of span names.
- **BUILT_IN_SPAN_NAMES / REPORTS_INCLUDE_SPANS:** Adding `ROUTER_HTTP_SPAN_NAME` to `BUILT_IN_SPAN_NAMES` in [consts.rs](apollo-router/src/plugins/telemetry/consts.rs) changes the array length (e.g. from 11 to 12). Any code that indexes or asserts on that length will need updating. [tracing/datadog/mod.rs](apollo-router/src/plugins/telemetry/tracing/datadog/mod.rs) only iterates the array; no change needed there. If we add `ROUTER_HTTP_SPAN_NAME` to `REPORTS_INCLUDE_SPANS` in [tracing/apollo_telemetry.rs](apollo-router/src/plugins/telemetry/tracing/apollo_telemetry.rs), the array size (e.g. 16 → 17) and any tests that depend on that list may need updating.
- **Schema / config snapshots:** [apollo-router/src/configuration/snapshots/](apollo-router/src/configuration/snapshots/) may generate schema that includes `telemetry.instrumentation.events` and `telemetry.instrumentation.spans`. Adding new keys `router_http` can change the generated schema snapshot; run the schema generation test and update the snapshot if the schema is intended to include the new keys.

---

## 5. Documentation

### Already in plan (required)

- **Events:** [docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/events.mdx](docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/events.mdx) — Add `router_http` to the list of lifecycle services that support events. Describe that `router_http` has the same structure as `router` (request/response/error + custom events) but runs at the RouterHttp stage (earliest point, before router pipeline). Emphasize that **router_http is HTTP-focused**: conditions and attributes should use HTTP request/response (headers, method, status, body size) rather than GraphQL-specific concepts. Include a short YAML example with `telemetry.instrumentation.events.router_http` and point to the request lifecycle doc.
- **Spans:** [docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/spans.mdx](docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/spans.mdx) — Add `router_http` to the list of services that support span configuration (`telemetry.instrumentation.spans.router_http`) and to the YAML example (router, supergraph, subgraph, connector, http_client → add router_http). Note that router_http span attributes are oriented around the HTTP request/response at that stage.
- **Router lifecycle services:** [docs/shared/router-lifecycle-services.mdx](docs/shared/router-lifecycle-services.mdx) — Add a bullet for **RouterHttp service** that supports instrumentation (span + events) at the **raw HTTP layer** (before the Router pipeline), with its own span in the trace; instrumentation is HTTP-focused (headers, method, status, etc.).

### Other docs to update for the new span

- **Request lifecycle observability:** [docs/source/routing/request-lifecycle.mdx](docs/source/routing/request-lifecycle.mdx) — In "Observability of the request lifecycle", the text says "You can instrument the Router, Supergraph, and Subgraph services with events". Update to include **RouterHttp** (e.g. "RouterHttp, Router, Supergraph, and Subgraph services") so the new span/events stage is mentioned where we describe what can be instrumented.
- **Standard attributes:** [docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/standard-attributes.mdx](docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/standard-attributes.mdx) — Has "Standard attributes of the `router` service", "Standard attributes of the `supergraph` service", etc. Add **"Standard attributes of the `router_http` service"**: same HTTP-oriented attributes as the router service (request method, headers, status, etc.), and note that router_http runs at the raw HTTP layer.
- **Selectors:** [docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/selectors.mdx](docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/selectors.mdx) — States "Each service of the router pipeline (`router`, `supergraph`, `subgraph`, `connector`) has its own available selectors." Add `**router_http`** to that list and add a short subsection for the router_http service: same selectors as the router service (HTTP-centric: request_header, response_header, etc.), since router_http uses the same request/response types.
- **Trace exporters overview:** [docs/source/routing/observability/router-telemetry-otel/telemetry-pipelines/trace-exporters/overview.mdx](docs/source/routing/observability/router-telemetry-otel/telemetry-pipelines/trace-exporters/overview.mdx) — Optional: where it says "The router generates spans that include the various phases of serving a request", consider adding that traces include a **router_http** span (HTTP layer) and a **router** span (router pipeline), or leave as-is if the phrasing is intentionally high-level.
- **Datadog router instrumentation:** [docs/source/routing/observability/router-telemetry-otel/apm-guides/datadog/router-instrumentation.mdx](docs/source/routing/observability/router-telemetry-otel/apm-guides/datadog/router-instrumentation.mdx) — The example YAML has `spans.router`, `spans.supergraph`, `spans.subgraph`. Add a `**router_http`** section to the example so users can customize the router_http span for Datadog (e.g. `otel.name`, `resource.name` from request method/path) if desired. Keeps APM docs consistent with the new span.
- **Other APM / telemetry docs:** If [Jaeger](docs/source/routing/observability/router-telemetry-otel/apm-guides/jaeger/jaeger-traces.mdx), [Zipkin](docs/source/routing/observability/router-telemetry-otel/apm-guides/zipkin/zipkin-traces.mdx), [New Relic](docs/source/routing/observability/router-telemetry-otel/apm-guides/new-relic/new-relic-otlp-traces.mdx), or [Dynatrace](docs/source/routing/observability/router-telemetry-otel/apm-guides/dynatrace/dynatrace-traces.mdx) list span names or show YAML that enumerates spans (router, supergraph, subgraph), add **router_http** where relevant so the new span is documented and any examples stay accurate.

---

## 6. Optional follow-ups (out of scope for this plan)

- **Instruments:** Add `router_http` under `telemetry.instrumentation.instruments` if custom metrics at RouterHttp are desired.

---

## Summary of files to touch


| Area                                                  | File(s)                                                                                                                                                                                                                                                   |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Constants + BUILT_IN_SPAN_NAMES                       | [apollo-router/src/plugins/telemetry/consts.rs](apollo-router/src/plugins/telemetry/consts.rs) — add ROUTER_HTTP_SPAN_NAME                                                                                                                                |
| Spans config + validation                             | [apollo-router/src/plugins/telemetry/config_new/spans.rs](apollo-router/src/plugins/telemetry/config_new/spans.rs) — add router_http: RouterSpans                                                                                                         |
| Span creation                                         | [apollo-router/src/plugins/telemetry/span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs) — add create_router_http                                                                                                                       |
| Config + validation (events)                          | [apollo-router/src/plugins/telemetry/config_new/events.rs](apollo-router/src/plugins/telemetry/config_new/events.rs)                                                                                                                                      |
| Event prefix + context keys (router/router_http only) | [apollo-router/src/plugins/telemetry/config_new/router/events.rs](apollo-router/src/plugins/telemetry/config_new/router/events.rs) — no supergraph/subgraph/connector changes                                                                             |
| Selectors / fmt_layer (if they read display types)    | router/selectors.rs, fmt_layer.rs                                                                                                                                                                                                                         |
| Wiring (span + events)                                | [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs) — router_http span (outer) + router span (inner), router_http events and span attributes                                                                         |
| Downstream span names (if needed)                     | apollo_telemetry.rs, tracing/datadog/mod.rs, apollo_otlp_exporter.rs — add ROUTER_HTTP_SPAN_NAME where span names are listed or switched on                                                                                                               |
| Tests                                                 | config_new/events.rs, config_new/router/events.rs, integration telemetry (assert router_http span + event order); update snapshots if needed                                                                                                              |
| Docs                                                  | events.mdx, spans.mdx, router-lifecycle-services.mdx; request-lifecycle.mdx, standard-attributes.mdx, selectors.mdx; trace-exporters/overview.mdx (optional); apm-guides/datadog/router-instrumentation.mdx; other APM trace docs if they list span names |


