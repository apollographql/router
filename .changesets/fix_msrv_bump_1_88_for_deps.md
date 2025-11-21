### Bump MSRV to Rust 1.88.0 ([Issue #XXXX](https://github.com/apollographql/router/issues/XXXX))

Projects using `apollo-router` as a library must now use Rust 1.88.0 or later, but there are no breaking changes to the distributed Router binary.

Bump minimum supported Rust version (MSRV) from 1.85.0 to 1.88.0. Moving to Rust 1.88.0 secures our ability to bring in dependencies that might need security and bug fix updates requiring Rust 1.88+.

While there's no immediate security concern, the ecosystem is already shifting toward newer Rust versions — it's important that we're _not_ stuck if we need to update dependencies like `aws-sdk-sso`, `aws-sdk-ssooidc`, and `aws-sdk-sts`.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/XXXX
