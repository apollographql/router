### Redact sensitive configuration fields in debug output ([PR #10215](https://github.com/apollographql/router/pull/10215))

Debug output for typed configuration fields now prints `[REDACTED]` for Redis usernames and passwords, AWS SigV4 access keys and secrets, TLS private keys (`tls.supergraph.key`, subgraph and connector `client_authentication.key`, and `telemetry.exporters.*.otlp.grpc.key`), and response-cache invalidation shared keys. The generated configuration schema marks these fields with `x-apollo-secret: true`.

Configuration sections that contain these secrets can no longer be serialized, so the generated schema no longer advertises a `default` for them: `authentication.subgraph.all`/`subgraphs`, `authentication.connector.sources`, `telemetry.exporters.*.otlp.grpc` and its `key`, and the response-cache subgraph `all`, `redis`, `invalidation` and `shared_key`. Editors using the schema show fewer default hints for these sections. Runtime defaults and validation are unchanged, and non-secret fields inside these sections keep their schema defaults.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10215
