# Fuzzy testing

## Targets

### Router

This target is sending the same query to both a backend exposing all the types in a schema and to a router started with a supergraph schema pointing to that same backend for each subgraph. The target then checks that both answers are the same.
The invariant tested here is that router will return the expected response regardless of how the schema is cut up, or the number of subgraph requests it takes to get a response.
Before launching the fuzz target, you have to spawn a subgraph server and start a router.

Start the subgraph server with this command:
```bash
cd fuzz/subgraph && cargo run --release
```

Start the router with a supergraph in `fuzz` directory and any router configuration you need to test. Below, the command uses `router.yaml` configuration in the `fuzz` directory:
```bash
 cargo run -- --config fuzz/router.yaml --supergraph fuzz/subgraph/supergraph.graphql --dev --log=trace
```

Run the fuzzer with this command:

```
# Only works on Linux and MacOS
cargo +nightly fuzz run router
```

### Connectors

This target fuzzes the Connector's Mapping Language and ensures that it continues to compose and that it
successfully handles requests to a running router instance with the fuzzed schema.

```
cargo +nightly fuzz run connectors
```

### Query planner

This target fuzzes the `apollo-federation` query planner in-process. It generates an arbitrary
valid operation against the API schema, then asks a `QueryPlanner` built from
`fuzz/subgraph/supergraph.graphql` to plan it. The invariant is that the planner must not panic on
any operation that successfully parses and validates against the API schema — planning errors are
expected for adversarial inputs and are ignored. No running router or subgraph is required.

```
cargo +nightly fuzz run query_planner -- -max_len=4096
```

`-max_len=4096` is recommended: apollo-smith consumes a lot of bytes per operation, and at libFuzzer's
default starting size the generated operations rarely contain `@defer` or other interesting directives.
With 4096-byte inputs roughly half the generated operations carry `@defer`.
