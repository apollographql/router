### Entity cache preview ([PR #5574](https://github.com/apollographql/router/pull/5574))

#### Support private information caching

The router supports a new `private_id` option that enables separate, private cache entries to be allocated per user for authenticated requests.

When a subgraph returns a `Cache-Control: private` header, the response data shouldn't be cached and shared among users. However, since the router supports request authentication, it can use it to allocate separate cache entries per users. 

To enable this, configure the `private_id` to be the name of a key in the request context that contains the data that's used to differentiate users. This option must be paired with a coprocessor or Rhai script to set the value in context.

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


To learn more about configuring and customizing private information caching, go to [Private information caching](https://www.apollographql.com/docs/router/configuration/entity-caching/#private-information-caching) docs.
 
By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5574