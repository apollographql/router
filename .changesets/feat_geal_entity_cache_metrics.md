### Support new span and metrics formats for entity caching ([PR #5625](https://github.com/apollographql/router/pull/5625))

<!-- [ROUTER-387] -->
Metrics of the router's entity cache have been converted to the latest format with support for custom telemetry.

The following example configuration shows the the `cache` instrument, the `cache` selector in the subgraph service, and the `cache` attribute of a subgraph span: 

```yaml
telemetry:
  instrumentation:
    instruments:
      default_requirement_level: none
      cache:
        apollo.router.operations.entity.cache:
          attributes:
            entity.type: true
            subgraph.name:
              subgraph_name: true
            supergraph.operation.name:
              supergraph_operation_name: string
      subgraph:
        only_cache_hit_on_subgraph_products:
          type: counter
          value:
            cache: hit
          unit: hit
          description: counter of subgraph request cache hit on subgraph products
          condition:
            all:
            - eq:
              - subgraph_name: true
              - products
            - gt:
              - cache: hit
              - 0
          attributes:
            subgraph.name: true
            supergraph.operation.name:
              supergraph_operation_name: string

```

By [@Geal](https://github.com/Geal) and [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5625