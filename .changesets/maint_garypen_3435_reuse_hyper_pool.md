### Add a pool idle timeout for subgraph HTTP connectors ([Issue #3435](https://github.com/apollographql/router/issues/3435))

Having a high idle pool timeout duration can sometimes trigger situations in which an HTTP request cannot complete (see [this comment](https://github.com/hyperium/hyper/issues/2136#issuecomment-589488526) for more information).

This changeset sets a default timeout duration of 5 seconds, which we may make configurable eventually.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3470