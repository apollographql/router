### Rename `authentication.router.jwt.on_error` values to snake_case

The values of `authentication.router.jwt.on_error` are renamed from `Error` / `Continue` /
`RedactedError` to `error` / `continue` / `redacted_error`, for consistency with every other
string enum in the router configuration schema. Existing configurations are migrated
automatically.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
