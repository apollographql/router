### Entity cache preview: support queries with private scope ([PR #4855](https://github.com/apollographql/router/pull/4855))

**This feature is part of the work on [subgraph entity caching](https://www.apollographql.com/docs/router/configuration/entity-caching/), currently in preview.**

The router now supports caching responses marked with `private` scope. This caching currently works only on subgraph responses without any schema-level information.

For details about the caching behavior, see [PR #4855](https://github.com/apollographql/router/pull/4855) 


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4855