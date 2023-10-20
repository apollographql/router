### Propagate headers for source stream events with subscription ([Issue #3731](https://github.com/apollographql/router/issues/3731))

Before the headers coming from the request were not propagated to the subgraph request when configured with headers plugin on subscription events. You had to use a Rhai script as a workaround, it's not required anymore.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4057