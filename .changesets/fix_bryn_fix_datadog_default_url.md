### Fix the Datadog default tracing exporter URL ([Issue #4415](https://github.com/apollographql/router/issues/4416))

The default URL for the Datadog exporter was incorrectly set to `http://localhost:8126/v0.4/traces` which caused issues for users that were running different agent versions.
This is now fixed and matches the exporter URL of `http://127.0.0.1:8126`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4444
