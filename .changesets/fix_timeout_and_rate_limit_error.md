### Request timeout and rate limited error responses are structured errors ([PR #5578](https://github.com/apollographql/router/pull/5578))

Request timeout (`408 Request Timeout`) and request rate limited (`429 Too Many Requests`)errors will now result in a structured GraphQL error (i.e., `{"errors": [...]}`) being returned to the client rather than a plain-text error as was the case previously.

Also both errors are now properly tracked in telemetry, including `apollo_router_graphql_error_total` metric. 

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5578