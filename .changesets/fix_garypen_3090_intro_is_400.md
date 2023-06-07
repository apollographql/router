### GraphQL introspection errors are now 400 errors ([Issue #3090](https://github.com/apollographql/router/issues/3090))

If we get an Introspection error during SupergraphService::plan_query(), then it is reported to the client as an HTTP 500 error. This change modifies the handling of errors to generate a valid GraphQL error for Introspection errors whilst also modifying the HTTP status to be 400.

The result of this change is that the client response

StatusCode:500
```json
{"errors":[{"message":"value retrieval failed: introspection error: introspection error : Field "__schema" of type "__Schema!" must have a selection of subfields. Did you mean "__schema { ... }"?","extensions":{"code":"INTERNAL_SERVER_ERROR"}}]}
```

becomes:

StatusCode:400
```json
{"errors":[{"message":"introspection error : Field "__schema" of type "__Schema!" must have a selection of subfields. Did you mean "__schema { ... }"?","extensions":{"code":"INTROSPECTION_ERROR"}}]}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3122