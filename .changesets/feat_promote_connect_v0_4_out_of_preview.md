### `connect/v0.4` is no longer a preview spec version

`connect/v0.4` (and the `->` method shape checking, `?` optional chaining, and unified object/selection literal syntax it enables) is no longer treated as a preview version of the Connect spec. Subgraphs can `@link` to `connect/v0.4` without any opt-in, and composition now uses `connect/v0.4` as the default version stamped into the supergraph when a subgraph doesn't explicitly declare a connect spec version.

`connect/v0.5` is added as a new preview spec version, gated behind the `connectors.preview_connect_v0_5` opt-in in `router.yaml` (see the accompanying changeset).

By [@briannafugate408](https://github.com/briannafugate408) in https://github.com/apollographql/router/pull/9839
