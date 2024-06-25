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

### Federation

This target is useful to spot differences between `gateway@1.x` and `gateway@2.x`. Before launching it you have to spawn the docker-compose located in the `fuzz` directory: `docker-compose -f fuzz/docker-compose.yml up`.
And then run it with:

```
# Only works on Linux
cargo +nightly fuzz run federation
```