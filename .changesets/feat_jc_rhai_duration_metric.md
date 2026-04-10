### Emit `apollo.router.operations.rhai.duration` histogram metric for Rhai script callbacks

A new `apollo.router.operations.rhai.duration` histogram metric (unit: `s`, value type: `f64`) is now emitted for every Rhai script callback execution across all pipeline stages. This mirrors the existing `apollo.router.operations.coprocessor.duration` metric.

Attributes on each datapoint:
- `rhai.stage` — the pipeline stage (e.g. `RouterRequest`, `SubgraphResponse`)
- `rhai.succeeded` — `true` if the callback returned without throwing
- `rhai.is_deferred` — present on response stages. `true` for `@defer` and subscription data chunks, `false` for the primary or initial response.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9072
