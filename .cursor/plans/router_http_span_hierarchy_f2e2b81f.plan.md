---
name: Router HTTP span and events with docs and validation
overview: Router HTTP is a full Tower layer (coprocessor, Rhai, Rust plugins) and is an OTel span with custom attributes and custom events, created inside the router_http service and appearing under the top-level router span. Document this and why the router span does not align with the router pipeline hook. Validate all behavior with examples, integration tests, and unit tests.
todos: []
isProject: false
---

# Router HTTP as OTel span with custom attributes and events — plan from scratch

This plan defines the desired behavior, reviews existing code against it, and requires documentation plus validation (examples, integration tests, unit tests) for any changes.

---

## 1. Desired behavior

### Span hierarchy

- **Router span:** The top-level span for each request. Created at the **Axum HTTP boundary** when the request is received (`TraceLayer` + `PropagatingMakeSpan` → `create_router(request)` in [apollo-router/src/axum_factory/utils.rs](apollo-router/src/axum_factory/utils.rs)). It represents the **entire router handler** (from HTTP in to response out) and is the **parent** of all other router spans (router_http, supergraph, subgraph, etc.). It is **not** the same as the "router service" or "router" pipeline stage that plugins/coproc hook into.
- **Router HTTP span:** A span in OpenTelemetry that:
  - Is created and **initiated inside the router_http service** (the first layer in the Router pipeline, in the telemetry plugin’s `router_http_service`).
  - Appears **under** the top-level router span (i.e. **router** is parent, **router_http** is child).
  - Supports **custom attributes** (via `instrumentation.spans.router_http.attributes`) and **custom events** (via `instrumentation.events.router_http`).
  - Covers the Router HTTP layer where coprocessor, Rhai scripts, and Rust plugins can run before the rest of the Router pipeline.

So the trace hierarchy must be: **router** (root) → **router_http** (child) → … (router pipeline, then supergraph, etc.).

### Why the router span doesn’t “line up”

The **router span** (OTel) = entire request handler (Axum → full pipeline). The **router service / router hook** (plugin API) = one pipeline **stage** (after RouterHttp, before supergraph). So they do not line up; the router span wraps everything, including the router_http span and the router pipeline stage. This must be clearly documented.

---

## 2. Review existing code and align to desired behavior

Do **not** assume current code is correct. Verify and, if needed, fix:

1. **Creation order and parent/child**
  - Confirm the **router** span is created at Axum (TraceLayer) before the request is dispatched to the Router pipeline.
  - Confirm the **router_http** span is created inside the Router pipeline (telemetry plugin `router_http_service`) as the first layer, so that when it is created the current span is **router**. That yields **router** → **router_http**.
  - If the order is wrong (e.g. router_http were created before router), fix it so router is the parent.
2. **Router HTTP span implementation**
  - [apollo-router/src/plugins/telemetry/mod.rs](apollo-router/src/plugins/telemetry/mod.rs): The `router_http_service` should add an `InstrumentLayer` that creates the router_http span, then map_request/response logic that runs router_http events and applies router_http span attributes in that span’s context. State (e.g. `RouterHttpTelemetryState`) should carry the span and events/attrs so response/error handling runs in the router_http span context.
  - [apollo-router/src/plugins/telemetry/span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs): `create_router_http` should exist and create a span with the appropriate name and attributes for the HTTP request.
  - [apollo-router/src/plugins/telemetry/config_new/spans.rs](apollo-router/src/plugins/telemetry/config_new/spans.rs): `spans.router_http` should exist and be used to configure attributes on the router_http span.
  - [apollo-router/src/plugins/telemetry/config_new/events.rs](apollo-router/src/plugins/telemetry/config_new/events.rs): `events.router_http` should exist and be used to build router_http events (request/response/error and custom events).
3. **OTel and logging**
  - Ensure the router_http span name is included where root/span-name lists are used for sampling or export (e.g. [apollo-router/src/plugins/telemetry/otel/layer.rs](apollo-router/src/plugins/telemetry/otel/layer.rs), [apollo-router/src/plugins/telemetry/consts.rs](apollo-router/src/plugins/telemetry/consts.rs), [apollo-router/src/plugins/telemetry/tracing/apollo_telemetry.rs](apollo-router/src/plugins/telemetry/tracing/apollo_telemetry.rs)) so the router_http span is correctly exported and not treated as a root when it is a child of router.
4. **Config schema**
  - Keep `instrumentation.spans.router_http` and `instrumentation.events.router_http` in the schema. Update doc comments if needed so they state that the router_http span is created inside the router_http service and appears under the router span.

---

## 3. Documentation

### 3.1 Router HTTP span

Document in the telemetry/observability docs (e.g. [docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/spans.mdx](docs/source/routing/observability/router-telemetry-otel/enabling-telemetry/spans.mdx) and any events doc):

- **Router HTTP** is an OpenTelemetry **span** with:
  - Custom attributes (via `instrumentation.spans.router_http.attributes`).
  - Custom events (via `instrumentation.events.router_http`).
- The span is **initiated inside the router_http service** (the first layer in the Router pipeline, where coprocessor, Rhai, and Rust plugins can run).
- It appears **under** the top-level **router** span (router = parent, router_http = child).

### 3.2 Router span vs router hook

Document (same spans doc and optionally [docs/source/routing/request-lifecycle.mdx](docs/source/routing/request-lifecycle.mdx)):

- The **router span** in OpenTelemetry represents the **entire router handler** (created at HTTP entry, parent of router_http, supergraph, subgraph, etc.). It does **not** line up with the **router service** or **router** pipeline stage.
- The **router service / router hook** is the pipeline **stage** you plug into with Rust plugins or the coprocessor (after RouterHttp, before supergraph). That stage is one part of the request; the router span wraps the full request. So when you customize “router” in the plugin sense, you are not customizing the OTel “router” span; you are customizing the Router pipeline stage.

---

## 4. Examples (examples/telemetry)

Add or update example config(s) in **[examples/telemetry/](examples/telemetry/)** so that:

1. **Router HTTP span and events** are shown: include `instrumentation.spans.router_http.attributes` and `instrumentation.events.router_http` with at least one custom request and one custom response event (and attributes), so users can see the router_http span and its events in a trace.
2. **All stages** are documented: router, router_http, supergraph, subgraph, connector — both custom span attributes and custom events where applicable, so the examples serve as a reference for every instrumented stage.
3. **README** in examples/telemetry (and optionally otlp-router) is updated to describe that the example shows the router_http span under the router span and documents custom spans and custom events for all stages.

Reference structure: [apollo-router/src/plugins/telemetry/testdata/custom_events.router.yaml](apollo-router/src/plugins/telemetry/testdata/custom_events.router.yaml). The example config must be valid and schema-compliant.

---

## 5. Validation: tests

Any new or changed code must be validated with **examples**, **integration tests**, and **unit tests** for the affected sections.

### 5.1 Integration tests

- **Span hierarchy:** Assert that a request produces a trace where the **router** span is the root (or child of a propagated root) and the **router_http** span is a **child** of the router span. Validate in the OTLP integration tests (e.g. [apollo-router/tests/integration/telemetry/otlp/tracing.rs](apollo-router/tests/integration/telemetry/otlp/tracing.rs)) that the expected span names and parent-child relationship hold (e.g. `test_router_http_observable_in_telemetry` or equivalent).
- **Router HTTP ordering:** Existing tests (e.g. [apollo-router/tests/integration/router_http_ordering.rs](apollo-router/tests/integration/router_http_ordering.rs)) that assert RouterHttp runs before the Router pipeline should remain and pass.
- **Verifier:** The telemetry trace verifier (e.g. [apollo-router/tests/integration/telemetry/verifier.rs](apollo-router/tests/integration/telemetry/verifier.rs)) should validate the router_http span (e.g. span kind, presence) when the spec includes it.

### 5.2 Unit tests

- **Span factory:** Unit tests that create the router_http span and assert name and key attributes (e.g. [apollo-router/src/plugins/telemetry/span_factory.rs](apollo-router/src/plugins/telemetry/span_factory.rs) tests if present).
- **Config and events:** Unit tests for router_http events and span attribute config (e.g. in config_new or events tests) so that custom events and attributes for router_http are built and applied as expected.
- **Telemetry plugin:** Any logic in the telemetry plugin that creates or uses the router_http span should be covered by unit tests where practical (e.g. that RouterHttpTelemetryState is populated and used in the response future).

### 5.3 Examples as validation

- Running the example config(s) in examples/telemetry (e.g. with otlp-router and Jaeger) should show the **router** and **router_http** spans in the trace with the expected parent-child relationship and custom events/attributes. Document in the example README how to run and what to expect in the trace.

---

## 6. Summary


| Item                  | Action                                                                                                                                           |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Router span**       | Top-level span, created at Axum; parent of router_http and all other router spans.                                                               |
| **Router HTTP span**  | OTel span with custom attributes and custom events; created inside the router_http service; **child** of the router span.                        |
| **Code review**       | Review existing code (creation order, router_http InstrumentLayer, spans/events config, OTel/layer lists) and fix so behavior matches this plan. |
| **Documentation**     | Document router_http span (where it’s created, that it’s under the router span); document why router span ≠ router hook.                         |
| **Examples**          | examples/telemetry config(s) showing router_http span + events + attributes and all other stages; update README.                                 |
| **Integration tests** | Validate span hierarchy (router → router_http), router_http ordering, and verifier checks for router_http span.                                  |
| **Unit tests**        | Cover span factory, config/events for router_http, and telemetry plugin logic for router_http where practical.                                   |


