### Entity caching: fix inconsistency in cache-control header handling ([PR #7987](https://github.com/apollographql/router/pull/7987))

When the [Subgraph Entity Caching] feature is in use, it determines the `Cache-Control` HTTP response header sent to supergraph clients based on those received from subgraph servers.
In this process, Apollo Router only emits the `max-age` [directive] and not `s-maxage`.
This PR fixes a bug where, for a query that involved a single subgraph fetch that was not already cached, the subgraph response’s `Cache-Control` header would be forwarded as-is.
Instead, it now goes through the same algorithm as other cases.

[Subgraph Entity Caching]: https://www.apollographql.com/docs/graphos/routing/performance/caching/entity
[directive]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control#response_directives