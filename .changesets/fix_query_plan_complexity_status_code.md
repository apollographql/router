### Return a 4xx with `QUERY_PLAN_COMPLEXITY_EXCEEDED` when query planning complexity limits are exceeded

When a query plan was rejected for exceeding a planner complexity limit, the router returned an HTTP 500 with a generic `INTERNAL_SERVER_ERROR` code, making it indistinguishable from a genuine internal failure. These rejections are now returned as a 4xx response with the `QUERY_PLAN_COMPLEXITY_EXCEEDED` extension code, so clients and monitoring can correctly attribute them to the operation rather than to the router.

By [@apollo-mateuswgoettems](https://github.com/apollo-mateuswgoettems) in https://github.com/apollographql/router/pull/10197
