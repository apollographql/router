### Pipeline construction runs in explicit acquire, activate, and assemble phases ([PR #10045](https://github.com/apollographql/router/pull/10045))

The router now builds its request pipeline in two phases around the point of no return: `prepare_pipeline` does everything fallible and everything slow — resource acquisition, plugin construction, and query-plan warm-up — while the previous pipeline is still fully serving, then plugin activation swaps the global telemetry providers, and a fast, infallible `apply_pipeline` assembles the service stacks. Query-plan warm-up no longer runs after activation, so a hot reload's slow work happens with the old pipeline's telemetry intact. Traces of startup and hot reload gain `prepare_pipeline` (containing `acquire` and `warmup`), `activate`, and `apply_pipeline` spans, so a slow reload shows which step is responsible.

Exported trace names are unchanged, including the per-plugin construction spans. In text and JSON *logs*, those construction spans now render as `plugin` with an `otel.name` field carrying the old `plugin: apollo.<name>` value, instead of the concatenated name.

By [@BrynCooke](https://github.com/BrynCooke)
