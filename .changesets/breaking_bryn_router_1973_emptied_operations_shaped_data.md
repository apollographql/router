### Fully unauthorized operations return the same response shape as partially unauthorized ones ([PR #9911](https://github.com/apollographql/router/pull/9911))

When authorization directives remove every field from an operation, the response now carries each requested root field as `null` alongside the authorization errors, matching what clients already receive when only some fields are removed:

```json
{"data": {"orga": null}, "errors": [{"message": "Unauthorized field or type", "path": ["orga", "id"], "extensions": {"code": "UNAUTHORIZED_FIELD_OR_TYPE"}}]}
```

Such responses previously carried `"data": null`. Clients that detect a fully refused operation by checking `data` for `null` should check for errors with the `UNAUTHORIZED_FIELD_OR_TYPE` code instead, which covers partial refusals as well.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9911
