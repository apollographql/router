### Remove the deprecated `connectors.subgraphs` configuration field

The deprecated `connectors.subgraphs` configuration field has been removed. The replacement `connectors.sources` field has been available since 2.x with a deprecation warning.

Existing configurations are migrated automatically at startup: each `subgraphs.<subgraph_name>.sources.<source_name>` entry is collapsed into a single `sources` entry keyed by `<subgraph_name>.<source_name>`, and any `$config` block at the subgraph level is copied onto each source. The router logs a notice describing the rewrite.

The migration preserves the deprecated runtime's precedence rules, including when a `connectors.sources` entry already exists for the same `<subgraph>.<source>` key: the deprecated `override_url` / `max_requests_per_operation` win over it when set, and a subgraph-level `$config` always overwrites any source-level `$config` — matching the order in which the old `apply_config` applied the two shapes.

Two cases can't be fully replicated automatically, and the router logs a separate notice for them:

- A subgraph entry that only declares `$config` (no `sources`) can't be expressed in the new shape, since `$config` is per-source there; its `$config` is dropped.
- The deprecated runtime applied a subgraph's `$config` to *every* connector under that subgraph, including ones on `@source`s the deprecated config's own `sources` map didn't list. The migration can only copy `$config` onto the composite keys it can see, so if your schema declares additional sources for a migrated subgraph, copy `$config` onto their `connectors.sources` entries by hand.

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
