### Improve testing by avoiding cache effects and redacting tracing details ([PR #5638](https://github.com/apollographql/router/pull/5638))

We've had some problems with flaky tests and this PR addresses some of them.

The router executes in parallel and concurrently. Many of our tests use snapshots to try and make assertions that functionality is continuing to work correctly. Unfortunately, concurrent/parallel execution and static snapshots don't co-operate very well. Results may appear in pseudo-random order (compared to snapshot expectations) and so tests become flaky and fail without obvious cause.

The problem becomes particularly acute with features which are specifically designed for highly concurrent operation, such as batching.

This set of changes addresses some of the router testing problems by:

1. Making items in a batch test different enough that caching effects are avoided.
2. Redacting various details so that sequencing is not as much of an issue in the otel traces tests.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5638