### Deprecate `Mock*Service` test helpers in favour of `tower_test::mock::pair`

The `MockRouterService`, `MockSupergraphService`, `MockExecutionService`, `MockSubgraphService`, `MockConnectorService`, and `MockHttpClientService` types exported from `apollo_router::plugin::test` are now deprecated and will be removed in Router 3.0.

In your Rust plugins, migrate tests to use [`tower_test::mock::pair`](https://docs.rs/tower-test/latest/tower_test/mock/fn.pair.html) directly:

```rust
// Before
let mut mock = MockSubgraphService::new();
mock.expect_call().returning(|req| {
    Ok(subgraph::Response::fake_builder().build())
});
let service = stage.as_service(mock.boxed(), ...);

// After
let (mock, mut handle) = tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
let driver = tokio::spawn(async move {
    let (req, responder) = handle.next_request().await.unwrap();
    responder.send_response(subgraph::Response::fake_builder().build());
});
let service = stage.as_service(mock.boxed(), ...);
// ... after the test action:
driver.await.unwrap();
```

Use a timeout-guarded `driver.await` to catch assertion failures inside the spawned task and prevent silent test hangs.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9716