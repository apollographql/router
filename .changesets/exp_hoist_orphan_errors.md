### Add `experimental_hoist_orphan_errors` configuration for controlling orphan error path assignment

Adds a new `experimental_hoist_orphan_errors` configuration that controls how entity-less ("orphan") errors from subgraphs are assigned paths in the response. When enabled for a subgraph, orphan errors are assigned to the nearest non-array ancestor in the response path, preventing them from being duplicated across every element in an array. This can be enabled globally via `all` or per-subgraph via the `subgraphs` map. Per-subgraph settings override `all`.

Here's an example when targeting a specific subgraph, `my_subgraph`:

```yaml
experimental_hoist_orphan_errors:
  subgraphs:
    my_subgraph:
      enabled: true
```

An example when targeting all subgraphs:

```yaml
experimental_hoist_orphan_errors:
  all:
    enabled: true
```

And an example enabling for all subgraphs except one:

```yaml
experimental_hoist_orphan_errors:
  all:
    enabled: true
  subgraphs:
    noisy_one:
      enabled: false
```

Use this feature only if you know you have subgraphs that don't respond with the correct paths when making entity calls. If you're unsure, you probably don't need this.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8998
