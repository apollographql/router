### pq: include operation name in `PERSISTED_QUERY_NOT_IN_LIST` error ([PR #7768](https://github.com/apollographql/router/pull/7768))

When persisted query safelisting is enabled and a request has an unknown PQ ID, the GraphQL error now has the extension field `operation_name` containing the GraphQL operation name (if provided explicitly in the request).

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/7768