### Fix caching documentation to use environment variables for Redis credentials

The `caching.mdx` documentation contained incorrect variable expansion links and hardcoded `username`/`password` values in the Redis configuration YAML examples. This update corrects the variable expansion to reference the right links and updates the YAML snippets to use environment variable references, consistent with best practices for managing sensitive credentials.

By [@srinivas-sampath-apollo](https://github.com/srinivas-sampath-apollo) in https://github.com/apollographql/router/pull/9268
