### Update JWKS handling ([PR #6930](https://github.com/apollographql/router/pull/6930))

This PR updates JWT-handling in the `AuthenticationPlugin`;

- JWT-related code has been moved around a bit for organizational purposes
- Users may now set a new a new config option `config.authentication.router.jwt.on_error`. When set to the default `Error`, JWT-related errors will be returned to users (the current behavior.) When set to `Continue`, JWT errors will instead be ignored. When a JWT error is encountered, JWT claims will not be set in the request context.
- When JWTs are processed, whether processing fails or succeeds, request context will contain a new variable `apollo::authentication::jwt_status` which notes the result of processing.
- I applied a few lints that RustRover highlighted.

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/6930