### Add `supergraph.strict_deprecation_rules` to accept supergraphs with invalid `@deprecated` usages ([PR #TBD](https://github.com/apollographql/router/pull/TBD))

Router 3 rejects supergraphs that use `@deprecated` in ways the GraphQL September 2025 specification no longer allows. Supergraphs composed by the latest LTS composition already have these usages stripped, but older supergraphs may still contain them.

A new configuration option `supergraph.strict_deprecation_rules` (default `true`) can be set to `false` to apply the same fixes as composition before the router validates the supergraph, so these supergraphs load without being recomposed:

- `reason: null` is removed from `@deprecated`, leaving a bare `@deprecated`.
- `@deprecated` is removed from implementing fields whose interface field is not deprecated.

A warning is logged for each change. `@deprecated` on required arguments and input fields is still rejected, as it has been since router v2.16.1.

```yaml
supergraph:
  strict_deprecation_rules: false
```

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/TBD
