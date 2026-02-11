# Rhai http_service and service_http example

This example uses two Rhai hooks:

1. **http_service** – runs at the raw HTTP layer in front of the router (before GraphQL parsing). Adds request/response headers.
2. **service_http** – runs at the raw HTTP layer for outbound requests to subgraphs and connectors. Adds headers to subgraph/connector requests and responses.

## Prerequisites

- Apollo Router binary (from this repo or release)
- A supergraph schema (e.g. from this repo’s `graphql/` examples)

## Run

From the repository root:

```bash
cargo run -- -s examples/graphql/supergraph.graphql -c examples/rhai-http-service/router.yaml
```

Or with a released binary:

```bash
./router -s /path/to/supergraph.graphql -c examples/rhai-http-service/router.yaml
```

## Verify

```bash
curl -i -X POST http://127.0.0.1:4000/ \
  -H "Content-Type: application/json" \
  -d '{"query":"{ topProducts { name } }"}'
```

The response should include:

- `x-rhai-http-layer: ok` – confirms the **http_service** response callback ran
- `x-rhai-subgraph-http-service: ok` – confirms the **service_http** response callback ran (note: subgraph response headers may not always propagate to the client; check router logs for `service_http: outbound request` to verify the hook runs)

The script also sets `x-rhai-http-request` and `x-rhai-subgraph-http-request` on requests (visible to the router pipeline and subgraphs if you log or forward them).
