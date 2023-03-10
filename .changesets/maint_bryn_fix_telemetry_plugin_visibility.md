### Fix visibility of telemetry plugin ([Issue #2739](https://github.com/apollographql/router/issues/2739))

The telemetry plugin is currently pub. However, since the refactor of the Telemetry plugin and associated tests this doesn't need to be.

It is not a breaking change to fix this as the plugin was exported under the _private module which was clearly marked as internal. However, this can be removed altogether.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2740
