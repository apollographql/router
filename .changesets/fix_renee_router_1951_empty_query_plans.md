### More consistent responses to no-op queries

Certain GraphQL queries are not exactly empty, but never produce any output:
```graphql
{
  username @skip(if: true)
}
```

The router previously short-circuited such requests early on, returning an empty GraphQL response. This caused minor inconsistencies because some features did not run on these responses and some telemetry was not emitted for them.

In particular, the `expose_query_plans` plugin did not emit the query plan in this case. Now, it correctly reports that there was an empty query plan:

```json
{
  "extensions": {
    "apolloQueryPlan": {
      "object": {
        "kind": "QueryPlan",
        "plan": null
      },
      "text": "QueryPlan {}"
    }
  }
}
```

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/9738