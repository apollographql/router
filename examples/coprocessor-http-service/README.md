# Coprocessor http_service example

This example shows the **http_service** coprocessor stage: the hook that runs at the raw HTTP layer in front of the router (before GraphQL parsing). You can read and mutate request/response headers and body as bytes.

The Node.js coprocessor adds the response header `x-http-service-stage: response` so you can verify the hook ran.

## Prerequisites

- Node.js (for the coprocessor)
- Apollo Router binary (from this repo or release)
- A supergraph schema (e.g. from this repo’s `graphql/` examples)

## Steps

1. **Install and start the coprocessor**

   From this directory:

   ```bash
   cd nodejs
   npm install
   npm start
   ```

   Leave it running (it listens on http://127.0.0.1:3000).

2. **Run the router**

   From the repo root, or from a directory that can see `examples/graphql/` and this example’s `router.yaml`:

   ```bash
   cargo run -- -s examples/graphql/supergraph.graphql -c examples/coprocessor-http-service/nodejs/router.yaml
   ```

   Or with a released binary:

   ```bash
   ./router -s /path/to/supergraph.graphql -c examples/coprocessor-http-service/nodejs/router.yaml
   ```

3. **Send a request and check the header**

   ```bash
   curl -i -X POST http://127.0.0.1:4000/ \
     -H "Content-Type: application/json" \
     -d '{"query":"{ topProducts { name } }"}'
   ```

   The response should include:

   ```
   x-http-service-stage: response
   ```

   That confirms the coprocessor’s **http_service** response stage ran and its header was applied to the client response.

## Config reference

- `coprocessor.url`: address of this coprocessor.
- `coprocessor.http_service.request`: what to send on the HTTP request stage (e.g. `body`, `headers`).
- `coprocessor.http_service.response`: what to send on the HTTP response stage (e.g. `body`, `headers`, `status_code`).

Payload shape for these stages matches the router request/response externalization format (version, stage, id, headers, body, etc.).
