### Respond with `application/graphql-response+json` when the client accepts it ([PR #9977](https://github.com/apollographql/router/pull/9977))

The router accepted requests whose `Accept` header listed `application/graphql-response+json`, but its responses always had `Content-Type: application/json`. Clients that follow the [GraphQL over HTTP specification](https://graphql.github.io/graphql-over-http/draft/) expect a response of the media type they asked for.

When the `Accept` header includes `application/graphql-response+json`, non-streamed responses now have `Content-Type: application/graphql-response+json`, including when `application/json` is also listed. Requests that accept only `application/json` or `*/*` still get `application/json`. The router doesn't read `q=` weights in the `Accept` header yet, so `application/graphql-response+json` wins whenever it's present. Multipart responses (`@defer` and subscriptions) don't change.

By [@martinbonnin](https://github.com/martinbonnin) in https://github.com/apollographql/router/pull/9977
