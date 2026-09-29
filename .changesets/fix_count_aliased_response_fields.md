### Count aliased fields in response-based metrics and demand control ([PR #10331](https://github.com/apollographql/router/pull/10331))

Demand control's actual cost, local field metrics, and GraphQL field instruments looked up response values by field name instead of by response key. Aliased fields were skipped, so their cost and metrics were missing. They are now matched by their alias.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10331
