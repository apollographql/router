### Fix Linux flake in connectors::tests::test_operation_counter

`plugins::connectors::tests::test_operation_counter` was intermittently failing on Linux CI with:

```
[Request 1]: Expected path /users/1, got /users/2
```

The test issues `query { users { id name username } }` against a mocked subgraph. Connectors resolves this as a root `/users` fetch followed by two entity fetches — `/users/1` and `/users/2` — that run in parallel. Wiremock records requests in the order they actually arrive at the mock server, which is non-deterministic for concurrent in-flight requests. The test was using `req_asserts::matches`, which compares the recorded sequence positionally to the matcher list, so any time `/users/2` won the race it failed the assertion.

The fix swaps the positional matcher list for the existing `Plan::Sequence(Plan::Fetch, Plan::Parallel(...))` helper — the same pattern already used by `test_root_field_plus_entity_plus_requires` and `test_entity_references` for exactly this scenario. The parallel branch matches by set membership rather than position, so request ordering between the two entity fetches no longer affects the result. The counter assertion is unchanged.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
