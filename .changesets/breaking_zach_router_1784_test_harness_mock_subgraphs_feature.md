### `TestHarness` requires the `mock_subgraphs_testing` feature for subgraph mocking outside this crate

`TestHarness`'s built-in subgraph mocking (used whenever [`with_subgraph_network_requests`](https://docs.rs/apollo-router/latest/apollo_router/struct.TestHarness.html#method.with_subgraph_network_requests) is not called) is gated behind the `mock_subgraphs_testing` Cargo feature for consumers outside the `apollo-router` crate. If you build a `TestHarness` from a downstream crate without that feature enabled and without calling `with_subgraph_network_requests()`, building the harness now returns an error instead of silently making real network requests to subgraphs.

To keep the existing mocking behavior, add the feature to your `apollo-router` dependency:

```toml
apollo-router = { version = "...", features = ["mock_subgraphs_testing"] }
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9957
