# Telemetry

Examples for configuring router telemetry (tracing and metrics).

## Local telemetry with Jaeger

Run the router with OTLP tracing against a local Jaeger instance and view traces in the Jaeger UI. See [otlp-router/README.md](otlp-router/README.md) for step-by-step instructions.

**Configs:**

| Config | Description |
|--------|-------------|
| [jaeger.router.yaml](./jaeger.router.yaml) | Full OTLP: tracing to `http://127.0.0.1:4318`, Jaeger propagation, spec_compliant spans, optional Prometheus on :9090. |
| [otlp.router.yaml](./otlp.router.yaml) | Minimal OTLP (endpoint: default, gRPC 4317). |
| [custom-events-and-spans.router.yaml](./custom-events-and-spans.router.yaml) | Custom **spans** (attributes) and **events** for all stages (router, supergraph, subgraph, connector). The router span is the top-level span. Custom events attach to the current span at each stage and do not create new spans. |

**Quick start:** From [otlp-router/](./otlp-router/), run `docker compose up -d`. From the repo root, run the router with e.g. `cargo run -p apollo-router -- -s examples/graphql/supergraph.graphql -c examples/telemetry/otlp-router/router.yaml`. View traces at **http://localhost:16686**.

## Other examples

* **Custom events and spans** — [custom-events-and-spans.router.yaml](./custom-events-and-spans.router.yaml): Documents custom span attributes and custom events for every instrumented stage (router, supergraph, subgraph, connector). Run with the same steps as above; in Jaeger you will see the **router** span as the root with custom events on each span.
* **Metric renaming** — [metric-rename.router.yaml](./metric-rename.router.yaml): OpenTelemetry Views for OTLP and Prometheus.
* **Other exporters** — [datadog.router.yaml](./datadog.router.yaml), [zipkin-collector.router.yaml](./zipkin-collector.router.yaml), [zipkin-agent.router.yaml](./zipkin-agent.router.yaml).
