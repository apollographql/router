### Enable new and old schema introspection implementations by default ([PR #6014](https://github.com/apollographql/router/pull/6014))

Starting with this release, if schema introspection is enabled, the router runs both the old Javascript implementation and a new Rust implementation of its introspection logic by default. 

The more performant Rust implementation will eventually replace the Javascript implementation. For now, both implementations are run by default so we can definitively assess the reliability and stability of the Rust implementation before removing the Javascript one. 

You can still toggle between implementations using the `experimental_introspection_mode` configuration key. Its valid values: 

- `new` runs only Rust-based validation
- `legacy` runs only Javascript-based validation
- `both` (default) runs both in comparison and logs errors if differences arise

Having `both` as the default causes no client-facing impact. It will record and output the metrics of its comparison as a `apollo.router.operations.introspection.both` counter. (Note: if this counter in your metrics has `rust_error = true` or `is_matched = false`, please open an issue with Apollo.)

Note: schema introspection itself is disabled by default, so its implementation(s) are run only if it's enabled in your configuration:

```yaml
supergraph:
  introspection: true
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6014
