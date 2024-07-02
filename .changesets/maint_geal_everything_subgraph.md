### Introduce fuzz testing comparison between the router and monolithic subgraph ([PR #5302](https://github.com/apollographql/router/pull/5302))

Implements a fuzzer that can run on any router configuration to enhance router robustness and battle test new features.

Adds a router fuzzing target, to compare the result of a query sent to to router vs a monolithic subgraph, with a supergraph schema that points all subgraphs to that same monolith.

The monolithic subgraph consolidates code from typical subgraphs like accounts, products, reviews, and inventory (taken from the [starstuff repository](https://github.com/apollographql/starstuff/)).
This setup allows the subgraph to directly handle queries traditionally handled by individual subgraphs.
The invariant we check is that we should get the same result by sending the query to the subgraph directly or through a router that will artificially cut up the query into multiple subgraph requests, according to the supergraph schema.

To execute it:
- Start a router using the schema `fuzz/subgraph/supergraph.graphql`
- Start the subgraph with `cargo run --release` in `fuzz/subgraph`. It will start a subgraph on port 4005.
- Start the fuzzer from the repo root with `cargo +nightly fuzz run router`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5302