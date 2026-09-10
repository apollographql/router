### Don't require `APOLLO_KEY` when `--graph-artifact-reference` points at a non-Apollo OCI registry ([Issue #1983](https://apollographql.atlassian.net/browse/ROUTER-1983))

Starting the router with `--graph-artifact-reference` / `APOLLO_GRAPH_ARTIFACT_REFERENCE` previously
required `APOLLO_KEY` to be set, even when the graph artifact reference pointed at a non-Apollo OCI
registry that doesn't need Apollo authentication at all. `APOLLO_KEY` is now only required when the
reference resolves to an Apollo-hosted registry (`*.apollographql.com`).

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/TODO
