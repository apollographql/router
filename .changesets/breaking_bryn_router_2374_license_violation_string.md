### `ApolloRouterError::LicenseViolation` holds a `String` ([PR #10856](https://github.com/apollographql/router/pull/10856))

`ApolloRouterError::LicenseViolation` now holds a `String` instead of a `Vec<String>`. Code that embeds the router and matches on this variant to read the list no longer compiles; use the `String`, or the error's `Display` output, instead.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10856
