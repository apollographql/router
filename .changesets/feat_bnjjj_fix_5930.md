### Add ability to alias standard attributes for telemetry ([Issue #5930](https://github.com/apollographql/router/issues/5930))

There is an issue when using standard attributes (on cache for example) because on new relic `entity.type` is a reserved attribute name and so it won’t work properly. cf [Learn about New Relic entities](https://docs.newrelic.com/docs/new-relic-solutions/new-relic-one/core-concepts/what-entity-new-relic/#reserved-attributes)  Moreover `entity.type` is not consistent with our other graphql attributes (prefixed by `graphql.`). So we rename `entity.type` attribute to `graphql.type.name`.

In order to make it work and that could also answer other use cases that would be great if we can alias the name of a standard attribute like this:

```yaml
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant # Docs state this significantly improves performance: https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/spans#spec_compliant
    instruments:
      cache: # Cache instruments configuration
        apollo.router.operations.entity.cache: # A counter which counts the number of cache hit and miss for subgraph requests
          attributes:
            graphql.type.name:
              alias: entity_type # ENABLED and aliased to entity_type
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5957
