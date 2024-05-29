### Submit new metrics to Apollo ([PR #5270](https://github.com/apollographql/router/pull/5270))

The router submits two new metrics to Apollo:
- `apollo.router.lifecycle.api_schema` provides us feedback on the experimental Rust-based API schema generation.
- `apollo.router.lifecycle.license` provides metrics on license expiration. We use this to improve the reliability of the license check mechanism.
