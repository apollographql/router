### Coalesce queued schema, configuration, and license reloads ([PR #9890](https://github.com/apollographql/router/pull/9890))

When several schema, configuration, or license updates are already queued together, the router now collapses them into a single reload of the newest state rather than building each intermediate state in turn. Because every reload includes query-planner warm-up, a rapid burst of updates previously spent time warming up states that were immediately superseded; those intermediate builds are now skipped.

The router also distinguishes transient reload failures from permanent ones. A schema that fails to parse, or a configuration or license that violates enforcement, cannot succeed on retry, so it no longer retries on a timer and instead keeps serving the previous good state until the inputs change. Transient failures are retried as before.

A new metric, `apollo.router.state.reload.coalesced`, counts the reloads skipped by coalescing.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9890
