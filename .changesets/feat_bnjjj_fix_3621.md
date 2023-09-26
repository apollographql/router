### Added configuration to set redis request timeout. ([Issue #3621](https://github.com/apollographql/router/issues/3621))

We added configuration to modify default timeout for redis requests. Default timeout was also changed from 1ms to **2ms**.

Here is an exmample to change the timeout for APQ:
```yaml
apq:
  router:
    cache:
      redis:
        urls: ["redis://..."]
        timeout: 5ms
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3817