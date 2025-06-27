### Coprocessor: improve handling of invalid GraphQL responses with conditional validation ([PR #7731](https://github.com/apollographql/router/pull/7731))

The router was creating invalid GraphQL responses internally, especially when subscriptions terminate. When a coprocessor is configured, it validates all responses for correctness, causing errors to be logged when the router generates invalid internal responses. This affects the reliability of subscription workflows with coprocessors.

Fix handling of invalid GraphQL responses returned from coprocessors, particularly when used with subscriptions. Added conditional response validation and improved testing to ensure correctness. Added the `response_validation` configuration option at the coprocessor level to enable the response validation (by default it's enabled).

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7731