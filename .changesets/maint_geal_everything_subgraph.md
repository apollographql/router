### fuzz test the router VS a monolithic subgraph ([PR #5302](https://github.com/apollographql/router/pull/5302))

To get better assurance in how the Router is running and battle test new features, we are improving our testing process by including a fuzzer that can run with any Router configuration.

This adds a Router fuzzing target, to compare the result of a query sent to a router VS a monolithic subgraph, with a supergraph schema that points all subgraphs to that same monolith.

The subgraph here merges the code from the usual accounts, products, reviews and inventory subgraphs (taken from the [starstuff repository](https://github.com/apollographql/starstuff/)).
This means that it can answer any query that would be handled by those subgraphs, but since it also has all the types and data available, it can be queried directly too.
The invariant we check here is that we should get the same result by sending the query to the subgraph directly, or through a router that will artificially cut up the query in multiple subgraph requests, according to the supergraph schema.

To execute it:
- start a router using the schema `fuzz/subgraph/supergraph.graphql`
- start the subgraph with `cargo run --release` in `fuzz/subgraph`. It will start a subgraph on port 4005
- start the fuzzer from the repo root with `cargo +nightly fuzz run router`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5302