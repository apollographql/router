### Operations with only authorization errors return spec-compliant data ([PR #9911](https://github.com/apollographql/router/pull/9911))

When every field in an operation fails authorization, the response now carries each requested root field as `null` alongside an error for each field, the same shape clients receive when some fields fail:

```json
{"data": {"orga": null}, "errors": [{"message": "Unauthorized field or type", "path": ["orga", "id"], "extensions": {"code": "UNAUTHORIZED_FIELD_OR_TYPE"}}]}
```

The router previously returned `"data": null` here, incorrectly reporting that execution never produced a result. Clients that detect this case by checking `data` for `null` should check for errors with the `UNAUTHORIZED_FIELD_OR_TYPE` code instead, which covers partial failures as well.

`authorization.directives.reject_unauthorized` keeps returning `"data": null`; spec compliance for refused operations is tracked separately.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9911
