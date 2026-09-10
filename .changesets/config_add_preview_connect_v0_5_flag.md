### Add the `preview_connect_v0_5` opt-in for Connect v0.5

Using Connect spec v0.5 in a subgraph (via `@link(url: "https://specs.apollo.dev/connect/v0.5")`) now requires setting `connectors.preview_connect_v0_5: true` in `router.yaml`. As with earlier preview versions of the Connect spec, the router rejects schemas that link an ungated preview spec at startup until the corresponding opt-in is set.

By [@briannafugate408](https://github.com/briannafugate408) in https://github.com/apollographql/router/pull/9714
