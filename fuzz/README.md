# Fuzzy testing

## Targets

### Router

This target is sending the same query to a backend exposing all the types in a schema, and to the router started with a supergraph schema that points to that same backend for each subgraph, and checks that both answers are the same.
The invariant tested here is that however the schema is cut up, and however many subgraph requests it will take, the router will return the expected answer, as if it was just one API.
Before launching it, you have to spawn the subgraph in `fuzz/suvbgraph` with `cargo run --release` and start a Router with the schema in `fuzz/subgrgraph/supergraph.graphql`. You can use whatever router configuration you need, allowing you to fuzz the router with specific features activated.

Run the fuzzer with this command:

```
# Only works on Linux
cargo +nightly fuzz run router
```

### Federation

This target is useful to spot differences between `gateway@1.x` and `gateway@2.x`. Before launching it you have to spawn the docker-compose located in the `fuzz` directory: `docker-compose -f fuzz/docker-compose.yml up`.
And then run it with:

```
# Only works on Linux
cargo +nightly fuzz run federation
```