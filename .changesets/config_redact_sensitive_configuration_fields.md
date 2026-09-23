### Redact sensitive configuration fields in debug output

Debug output for typed configuration fields now prints `[REDACTED]` for Redis usernames and passwords, AWS SigV4 access keys and secrets, TLS private keys (`tls.supergraph.key`, subgraph and connector `client_authentication.key`, and `telemetry.exporters.*.otlp.grpc.key`), and response-cache invalidation shared keys. The generated configuration schema marks these fields with `x-apollo-secret: true`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/TBD
