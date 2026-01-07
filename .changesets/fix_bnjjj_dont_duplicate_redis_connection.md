### Response cache: don't duplicate redis connection if we specify custom config for a subgraph ([PR #8764](https://github.com/apollographql/router/pull/8764))


Fixed an issue where the response cache would create duplicate Redis connections when a custom configuration was specified for individual subgraphs (without any specific Redis configuration). Previously, if a subgraph inherited the same Redis configuration from the global `all` setting, the router would unnecessarily establish a redundant connection. Now, the router correctly reuses the existing connection pool when the configuration is identical, improving resource efficiency and reducing connection overhead.

**Impact**: Users with response caching enabled who specify Redis configurations at both the global and subgraph levels will see reduced Redis connection usage, leading to better resource utilization.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8764