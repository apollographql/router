### Remove the deprecated `connectors.subgraphs` configuration field

The deprecated `connectors.subgraphs` configuration field has been removed. The replacement `connectors.sources` field has been available since 2.x with a deprecation warning.

Existing configurations are migrated automatically at startup: each `subgraphs.<subgraph_name>.sources.<source_name>` entry is collapsed into a single `sources` entry keyed by `<subgraph_name>.<source_name>`, and any `$config` block at the subgraph level is copied onto each source. The router logs a notice describing the rewrite.

The migration preserves the deprecated runtime's precedence rules: a subgraph-level `$config` overwrites any source-level `$config` defined alongside it (matching the order in which the old `apply_config` assigned values). A subgraph entry that only declares `$config` (no `sources`) cannot be expressed in the new shape and its `$config` is dropped — under the new model, `$config` is per-source.

Before:

```yaml
connectors:
  subgraphs:
    my_subgraph:
      $config:
        api_key: "secret"
      sources:
        my_source:
          override_url: https://example.com
          max_requests_per_operation: 50
```

After (equivalent shape, produced automatically):

```yaml
connectors:
  sources:
    my_subgraph.my_source:
      $config:
        api_key: "secret"
      override_url: https://example.com
      max_requests_per_operation: 50
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9520
