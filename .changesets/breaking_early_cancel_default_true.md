### Default `supergraph.early_cancel` to `true` ([PR #9594](https://github.com/apollographql/router/pull/9594))

The `supergraph.early_cancel` configuration option now defaults to `true`. When a client disconnects, the router will immediately cancel the in-flight request, consistent with the behavior of most other web services.

Previously the default was `false`, which caused requests to continue running in a background task even after the client disconnected.

To restore the previous behavior (e.g. to preserve telemetry for canceled requests), set `early_cancel: false` in your router config.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9594
