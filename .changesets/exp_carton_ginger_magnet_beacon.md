### Experimental: Introduce a pool of query planners ([PR #4897](https://github.com/apollographql/router/pull/4897))

The router supports a new experimental feature: a pool of query planners to parallelize query planning.

You can configure query planner pools with the `supergraph.query_planning.experimental_parallelism` option:

```yaml
supergraph:
  query_planning:
    experimental_parallelism: auto # number of available cpus
```

Its value is the number of query planners that run in parallel, and its default value is `1`. You can set it to the
special value `auto` to automatically set it equal to the number of available CPUs.

You can discuss and comment about query planner pools in
this [GitHub discussion](https://github.com/apollographql/router/discussions/4917).

By [@xuorig](https://github.com/xuorig) and [@o0Ignition0o](https://github.com/o0Ignition0o)
in https://github.com/apollographql/router/pull/4897
