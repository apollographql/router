### Normalize "operation_name" value inside supergraph request ([Issue #5014](https://github.com/apollographql/router/issues/5014))

At present, the behavior of `request.body.operation_name` in Rhai scripts is such that it is a direct copy of `operationName` from the client request.
So `request.body.operation_name` can be empty even if `query` contains a named query. For example:
```json
{
  "query": "query OperationName { me { id } }"
}
```
In this case, `request.body.operation_name` was empty because the client didn't specify `operationName.`

This behavior is very confusing to users who expect it to have an operation name if `query` contains a named query.
After this change, `request.body.operation_name` will always default to the operation name extracted from parsed `query`.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5008
