### Remove the `preview_connect_v0_4` opt-in for Connect v0.4 ([PR #9644](https://github.com/apollographql/router/pull/9644))

Using Connect spec v0.4 in a subgraph (via `@link(url: "https://specs.apollo.dev/connect/v0.4")`) no longer requires setting `connectors.preview_connect_v0_4: true` in `router.yaml`. Linking the v0.4 spec is itself a sufficient opt-in, so the router no longer rejects these schemas at startup. The `preview_connect_v0_4` configuration key is now a deprecated no-op; it continues to be accepted so existing configurations keep working, and can be safely removed.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9644
