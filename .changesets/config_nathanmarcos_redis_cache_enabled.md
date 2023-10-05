### Configuration param to disable Redis cache for APQ and Query Planning ([Issue #3506](https://github.com/apollographql/router/issues/3506))

The distributed [Query Plan](https://www.apollographql.com/docs/router/configuration/distributed-caching#distributed-query-plan-caching) and [APQ](https://www.apollographql.com/docs/router/configuration/distributed-caching#distributed-apq-caching) caching are [enterprise features](https://www.apollographql.com/docs/router/enterprise-features/) and require a [graph API key](https://www.apollographql.com/docs/graphos/api-keys/#graph-api-keys). In development environment, you might not have such a key and would need to manually change Router's configuration file to prevent a license violation error at startup. Now you can conveniently disable it by using an environment variable:

```yaml
supergraph:
  query_planning:
    experimental_cache:
      in_memory:
        limit: 1024
      redis:
----->  enabled: ${env.ENABLE_REDIS_CACHE:-false}
        urls:
          - "redis://${env.REDIS_HOST:-127.0.0.1}"

apq:
  router:
    cache:
      in_memory:
        limit: 1024
      redis:
----->  enabled: ${env.ENABLE_REDIS_CACHE:-false}
        urls:
          - "redis://${env.REDIS_HOST:-127.0.0.1}"
```

By [@nathanmarcos](https://github.com/nathanmarcos) in https://github.com/apollographql/router/pull/3805
