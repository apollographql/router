### Default `supergraph.early_cancel` to `true` ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))

The `supergraph.early_cancel` configuration option now defaults to `true`. When a client disconnects, the router will immediately cancel the in-flight request and release its buffer permit, which is consistent with the behavior of most other web services.

Previously the default was `false`, which caused requests to run to completion in a background task even after the client disconnected, holding buffer permits unnecessarily.

To restore the previous behavior (e.g. to preserve telemetry for canceled requests), set `early_cancel: false` in your router config.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
