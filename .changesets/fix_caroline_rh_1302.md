### Set `Cache-Control: no-store` when the response cache returns GraphQL errors ([PR #8933](https://github.com/apollographql/router/pull/8933))

When using the response cache plugin, if a query spans multiple subgraphs and one returns an error or times out, the final HTTP response was still carrying the successful subgraph's `Cache-Control` header (e.g. `max-age=1800, public`). This allowed intermediate caches (CDNs, reverse proxies) to cache and serve incomplete or stale partial responses to other clients.

If the response cache plugin is enabled and was going to set a `Cache-Control` header, but the response contains any GraphQL errors, it now sets `Cache-Control: no-store` instead of the merged subgraph cache control value.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8933
