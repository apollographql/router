### Entity cache preview: reorganize subgraph configuration override ([PR #5474](https://github.com/apollographql/router/pull/5474))

We align the entity cache configuration with the same all/subgraphs override pattern found in other parts of the Router configuration. We provide an automated configuration migration for this in the Router, so this should not affect existing uses.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5474