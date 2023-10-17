### Add path to SupergraphRequest ([Issue #4016](https://github.com/apollographql/router/issues/4016))

Coprocessors multi-part response support has been enhanced to include `hasNext`, allowing you to determine when a request has completed.

When `stage` is `SupergraphResponse`, `hasNext` if present and `true` indicates that there will be subsequent `SupergraphResponse` calls to the co-processor for each multi-part (`@defer`/subscriptions) response.

See the [coprocessor documentation](https://www.apollographql.com/docs/router/customizations/coprocessor/) for more details.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4017
