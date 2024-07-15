### Return request timeout and rate limited error responses as structured errors ([PR #5578](https://github.com/apollographql/router/pull/5578))

The router now returns request timeout errors (`408 Request Timeout`) and request rate limited errors (`429 Too Many Requests`) as structured GraphQL errors (for example, `{"errors": [...]}`). Previously, the router returned these as plaintext errors to clients.

Both types of errors are properly tracked in telemetry, including the `apollo_router_graphql_error_total` metric. 

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5578