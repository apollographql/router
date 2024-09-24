### Support aliasing standard attributes for telemetry ([Issue #5930](https://github.com/apollographql/router/issues/5930))

The router now supports creating aliases for standard attributes for telemetry.

This fixes issues where standard attribute names collide with reserved attribute names. For example, the standard attribute name `entity.type` is a [reserved attribute](New Relic entities](https://docs.newrelic.com/docs/new-relic-solutions/new-relic-one/core-concepts/what-entity-new-relic/#reserved-attributes) name for New Relic, so it won't work properly. Moreover `entity.type` is inconsistent with our other GraphQL attributes prefixed with `graphql.` 

The example configuration below renames `entity.type` to `graphql.type.name`:

```yaml
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant # Docs state this significantly improves performance: https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/spans#spec_compliant
    instruments:
      cache: # Cache instruments configuration
        apollo.router.operations.entity.cache: # A counter which counts the number of cache hit and miss for subgraph requests
          attributes:
            graphql.type.name: # renames entity.type
              alias: entity_type # ENABLED and aliased to entity_type
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5957
