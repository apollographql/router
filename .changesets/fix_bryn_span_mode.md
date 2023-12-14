### `span_mode: spec_compliant` not applied correctly ([Issue #4335](https://github.com/apollographql/router/issues/4335))

Previously, `telemetry.instrumentation.spans.span_mode.spec_compliant` was not being correctly applied. This resulted in extra request spans that should not have been present in spec compliant mode, where `router.supergraph.subgraph` was incorrectly output as `request.router.supergraph.subgraph`. This has been fixed in this release.


By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/4341
