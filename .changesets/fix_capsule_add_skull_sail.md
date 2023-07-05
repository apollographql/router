### Bring root span name in line with otel semantic conventions. ([Issue #3229](https://github.com/apollographql/router/issues/3229))

Root span name has changed from `request` to `<graphql.operation.kind> <graphql.operation.name>`

[Open Telemetry graphql semantic conventions](https://opentelemetry.io/docs/specs/otel/trace/semantic_conventions/instrumentation/graphql/) specify that the root span name must match the operation kind and name. 

Many tracing providers don't have good support for filtering traces via attribute, so this change will bring significant usability enhancements to the tracing experience.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3364
