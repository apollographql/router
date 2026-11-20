### Licensing is removed from the plugin and test APIs ([PR #10856](https://github.com/apollographql/router/pull/10856))

The router checks your license against your configuration and schema when it starts or reloads. Plugins never read the license, so the APIs that passed one to them are removed, and code that uses them no longer compiles:

- `PluginInit::builder()`, `PluginInit::try_builder()` and `PluginInit::fake_builder()` no longer take a license. Delete the `.license(...)` call.
- `TestHarness::license_from_allowed_features` is removed. Delete the call. The harness builds every plugin you configure and doesn't check your configuration against a license.
- `apollo_router::AllowedFeature` is no longer exported, and its `from_plugin_name` method is gone with it. It was only needed for the calls above. If you used `from_plugin_name` to map a plugin's configuration name to a license feature, keep that mapping in your own code.

Deleting these calls doesn't change what your plugins or tests do.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10856
