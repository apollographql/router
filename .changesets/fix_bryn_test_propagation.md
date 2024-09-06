### Fix Datadog sampling ([PR #5788](https://github.com/apollographql/router/pull/5788))

The router's Datadog exporter has been fixed so that traces are sampled as intended.

Previously, the Datadog exporter's context may not have been set correctly, causing traces to be undersampled.

By [@BrynCooke](https://github.com/BrynCooke) & [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5788