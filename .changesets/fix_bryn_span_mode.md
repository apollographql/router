### `span_mode: spec_complient` not applied correctly ([Issue #4335](https://github.com/apollographql/router/issues/4335))

`telemetry.instrumentation.spans.span_mode` was not being correctly applied, resulting in extra request spans that should not have been present in spec compliant mode.

In spec compliant mode the spans should have the hierarchy:

`router.supergraph.subgraph`

but were instead output as:

`request.router.supergraph.subgraph`

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/4341
