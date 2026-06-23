### Add the `connect-migrate` CLI for migrating Connectors schemas to `connect/v0.4` ([PR #9574](https://github.com/apollographql/router/pull/9574))

Adds `connect-migrate`, a command-line tool for migrating Apollo Connectors schemas to `connect/v0.4`, built behind the non-default `connect-migrate` cargo feature of `apollo-federation`. It is not part of the router runtime — the binary is built and distributed separately (via [`apollographql/connect-migrate`](https://github.com/apollographql/connect-migrate)).

`connect-migrate analyze` dual-parses every `@connect(selection: …)` at a schema's currently-linked `connect/v0.n` against the `v0.4` target and emits an agent-facing manifest sorting each divergent site into deterministic `$.` rewrites, output-identical no-ops, and genuine questions for the developer. It is the supported upgrade path for the `connect/v0.4` selection behavior change (where a primitive in value position is now read as a literal rather than a property access).

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9574
