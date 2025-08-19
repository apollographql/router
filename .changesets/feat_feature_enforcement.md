### Enforcing allowed features ([PR #7917](https://github.com/apollographql/router/pull/7917))

<!-- start metadata -->

<!-- [ROUTER-1412] -->
---
The router now enforces feature access based on the `allowed_features` claim in the license. The `allowed_features` claim may be found in all Apollo licenses. Features enforced include APQ caching and distributed query planning and plugins such as subscriptions and demand control. If a feature isn't included in the license, the router will fail to start and will emit an error message describing which features must be removed from the configuration file or which directives must be removed from the schema. For a full list of features see the `AllowedFeature` enum.


[ROUTER-1412]: https://apollographql.atlassian.net/browse/ROUTER-1412?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@DMallare](https://github.com/DMallare) in https://github.com/apollographql/router/pull/7917