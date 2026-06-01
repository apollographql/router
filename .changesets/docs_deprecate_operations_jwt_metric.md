### Deprecate `apollo.router.operations.jwt` in favor of `apollo.router.operations.authentication.jwt`

Updated the JWT authentication observability documentation to mark `apollo.router.operations.jwt` as deprecated. It is a misnamed metric that has been emitted in parallel with its replacement for over a year; operators should use `apollo.router.operations.authentication.jwt` with the `authentication.jwt.failed` attribute instead. The metric will continue to be emitted, but will be removed in a future release.

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/XXXX
