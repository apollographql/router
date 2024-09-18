### Enable both introspection implementation by default ([PR #6014](https://github.com/apollographql/router/pull/6014))

As part of the process to replace JavaScript schema introspection with a more performant Rust implementation in the router, we are enabling the router to run both implementations as a default. This allows us to definitively assess reliability and stability of Rust implementation before completely removing JavaScript one. As before, it's possible to toggle between implementations using the `experimental_introspection_mode` config key. Possible values are: `new` (runs only Rust-based validation), `legacy` (runs only JS-based validation), `both` (runs both in comparison, logging errors if a difference arises).

The `both` mode is now the default, which will result in **no client-facing impact** but will record the metrics for the outcome of comparison as a `apollo.router.operations.introspection.both` counter. If this counter in your metrics has `rust_error = true` or `is_matched = false`, please open an issue.

Schema introspection itself is disabled by default, so the above has no effect unless it is enabled in configuration:

```yaml
supergraph:
  introspection: true
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6014
