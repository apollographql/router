### Pipeline construction runs in explicit acquire, activate, and assemble phases ([PR #10045](https://github.com/apollographql/router/pull/10045))

The router now builds its request pipeline in three phases: acquire every resource whose creation can fail (plugins, TLS material, Redis connections, the persisted-query manifest), activate the plugins, then assemble the service stacks through infallible steps. Traces of startup and hot reload gain a `pipeline_initialization` span containing `acquire`, `activate`, `assemble`, and `warmup` spans, so a slow reload shows which step is responsible.

Exported trace names are unchanged, including the per-plugin construction spans. In text and JSON *logs*, those construction spans now render as `plugin` with an `otel.name` field carrying the old `plugin: apollo.<name>` value, instead of the concatenated name.

By [@BrynCooke](https://github.com/BrynCooke)
