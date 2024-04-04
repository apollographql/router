### Experimental: Introduce a pool of query planners ([PR #4897](https://github.com/apollographql/router/pull/4897))

This changeset introduces an experimental pool of query planners to parallelize query planning.

This feature is experimental, you can discuss it by following this link: https://github.com/apollographql/router/discussions/4917


Configuration:

```yaml
supergraph:
  query_planner:
    experimental_available_parallelism: auto # number of available cpus
```

Note you can also set `experimental_available_parallelism` to a number representing how many planners you want to use in a pool.
The default is `1`.

By [@xuorig](https://github.com/xuorig) and [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/4897
