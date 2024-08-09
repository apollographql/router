### Entity cache preview ([PR #5574](https://github.com/apollographql/router/pull/5574))

#### Private information caching

When a subgraph returns a `Cache-Control: private` header, the response data should not be cached and shared among users. But since the router supports request authentication, it can use it to allocate separate cache entries per users. This is done via the `private_id` option, which configures the name of a key in the request context, that contains the data used to differentiate users. This must be paired with a coprocessor or rhai script to set the value in context.

Example configuration:

```yaml title="router.yaml"
# Enable entity caching globally
preview_entity_cache:
  enabled: true
  subgraph:
    all:
      enabled: true
      accounts:
        private_id: "user_id"
```


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5574