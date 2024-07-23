### Enable running both query planners by default ([PR #5709](https://github.com/apollographql/router/pull/5709))

<!-- [ROUTER-458] -->

This change is enabling running the Native query planner in a background thread
in order to compare output and performance differences to the existing JS query
planner. Execution continues to be based on query plans produced by the JS query
planner, as Native query planner results are discarded after the comparison.

This option has been run in various different environments without an impact to
memory or CPU utilisation.

You can go back to the previous behaviour of running only the JS query planner with the following config in your router yaml:

```yml
experimental_query_planner_mode: legacy
```

By [lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/5709