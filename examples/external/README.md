# External co-processing

Demonstrates router request/response externalization can be performed via yaml configuration.

Possible operations include externalizing:
- Headers
- Body
- Context
- SDL (schema)

## Usage

```bash
cargo run -- -s ../graphql/supergraph.graphql -c ./router.yaml
```
