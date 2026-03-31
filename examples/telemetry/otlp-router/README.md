# Local telemetry with Jaeger

Run the Apollo Router with OTLP telemetry against a local Jaeger instance, send traffic, and view traces in the Jaeger UI.

## Prerequisites

- **Docker** (for Jaeger)
- **Router**: build from repo root with `cargo build --release`, or use `cargo run` as below

## 1. Start Jaeger

From this directory:

```bash
docker compose up -d
```

Jaeger listens for OTLP on **HTTP** `http://127.0.0.1:4318` and **gRPC** `127.0.0.1:4317`.  
UI: **http://localhost:16686**

## 2. Run the router

From the **repository root** (so the supergraph path resolves):

```bash
cargo run -p apollo-router -- -s examples/graphql/supergraph.graphql -c examples/telemetry/otlp-router/router.yaml
```

Or with the release binary:

```bash
./target/release/router -s examples/graphql/supergraph.graphql -c examples/telemetry/otlp-router/router.yaml
```

This config sends traces to `http://127.0.0.1:4318` (OTLP HTTP) with spec-compliant spans.

## 3. Send a request

```bash
curl -X POST http://127.0.0.1:4000/ \
  -H "Content-Type: application/json" \
  -d '{"query":"query { __typename }"}'
```

## 4. View traces in Jaeger

1. Open **http://localhost:16686**
2. Choose service **router**
3. Click **Find Traces**

You can inspect the span hierarchy (router → supergraph → execution → subgraph, etc.) and attributes for each trace.

## Configuration

- **router.yaml** (this dir): OTLP HTTP to `http://127.0.0.1:4318`, spec_compliant spans. Same content as [../jaeger.router.yaml](../jaeger.router.yaml); this copy keeps the example self-contained.
- **docker-compose.yaml**: Jaeger all-in-one with `COLLECTOR_OTLP_ENABLED=true`; ports 4317 (gRPC), 4318 (HTTP), 16686 (UI).

## Troubleshooting

**Jaeger UI not loading**

- Ensure Docker is running, then from this directory: `docker compose up -d`
- Check the container: `docker compose ps` (expect `jaeger` with port 16686)
- Logs: `docker compose logs jaeger`

**Port 16686 in use** — Change the host port in `docker-compose.yaml` (e.g. `"16687:16686"`) and use http://localhost:16687
