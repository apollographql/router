### `PluginInit` builders need a valid supergraph ([PR #10902](https://github.com/apollographql/router/pull/10902))

`PluginInit::builder()`, `PluginInit::try_builder()` and `PluginInit::fake_builder()` no longer have a `supergraph_schema` setter. They now read `supergraph_sdl` as a federation supergraph, the same way the router reads the schema it loads. Before, they accepted any GraphQL schema. If the SDL isn't a valid supergraph, `builder().build()` and `fake_builder().build()` panic, and `try_builder().build()` returns an error.

This affects only code that builds a `PluginInit` itself, usually a plugin's unit tests. To migrate, remove any `.supergraph_schema(...)` call and pass a composed supergraph as `supergraph_sdl`. If the test doesn't need a schema, pass an empty string to `builder()` or `try_builder()`, or leave `supergraph_sdl` unset on `fake_builder()`, and the plugin gets a minimal supergraph.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10902
