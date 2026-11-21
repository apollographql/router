### `AllowedFeature::from_plugin_name` is removed and `TestHarness` builds unlicensed plugins ([PR #10856](https://github.com/apollographql/router/pull/10856))

`apollo_router::AllowedFeature::from_plugin_name` mapped a built-in plugin's configuration name, such as `"authorization"`, to the licence feature that plugin needed. The router no longer uses that mapping: it now checks each licensed setting against your licence instead of each plugin name. Code that calls `from_plugin_name` no longer compiles. Use the `AllowedFeature` variant you need directly, such as `AllowedFeature::Authorization`, or keep the mapping in your own code.

`TestHarness` now builds a configured plugin even when the licence passed to `license_from_allowed_features` lacks that plugin's feature; before, it skipped the plugin. The harness doesn't run the router's startup licence check, so nothing refuses the configuration either. For example, a test that configures `authorization:` with `license_from_allowed_features(vec![])` used to run without authorization and now runs with it.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10856
