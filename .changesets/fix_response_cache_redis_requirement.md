### Require Redis configuration only when response caching is enabled ([PR #8684](https://github.com/apollographql/router/pull/8684))

Previously, the router attempted to connect to Redis for response caching regardless of whether response caching was enabled or disabled. This caused unnecessary connection attempts and configuration errors even when the feature was explicitly disabled.

The router now ignores Redis configuration if response caching is disabled. If response caching is configured to be _enabled_, Redis configuration is required, and missing Redis configuration raises an error on startup:

> Error: you must have a redis configured either for all subgraphs or for subgraph "products"

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8684
