# External co-processing

Demonstrates router request/response externalization via YAML configuration.

The example config enables:

- **router_http** (raw HTTP layer, before the Router pipeline): request path, method, and headers; response headers and status code.
- **router** (RouterService stage): request/response headers, context, body, and SDL (schema).

## Usage

```bash
cargo run -- -s ../graphql/supergraph.graphql -c ./router.yaml
```
