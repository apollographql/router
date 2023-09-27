### Added configuration to set redis request timeout ([Issue #3621](https://github.com/apollographql/router/issues/3621))

We added configuration to override default timeout for Redis requests. Default timeout was also changed from 1ms to **2ms**.

Here is an example to change the timeout for [Distributed APQ](https://www.apollographql.com/docs/router/configuration/distributed-caching#distributed-apq-caching) (an Enterprise Feature):
```yaml
apq:
  router:
    cache:
      redis:
        urls: ["redis://..."]
        timeout: 5ms
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3817