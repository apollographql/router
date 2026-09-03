### Remove retries in query plan warmup ([PR #9772](https://github.com/apollographql/router/pull/9772))

During router reloads, recently used GraphQL queries are pre-planned using the new configuration and schema. This is called warmup. In Router 2.x, if the router's compute job worker pool was overloaded, warmup would wait a bit and retry each query plan until space was available on the pool. In Router 3.0, warmup of a query is skipped if there is no space for it, so we do not queue up extra work when the router is already overloaded.

In effect, the router may switch over to the new configuration and schema on a cold cache. This can cause a further spike in 503 responses post-reload, but the router will also stabilise faster: the old and new pipelines are not competing for resources for as long.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/9772