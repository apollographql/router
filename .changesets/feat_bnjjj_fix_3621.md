### feat(redis): add configuration to set the timeout ([Issue #3621](https://github.com/apollographql/router/issues/3621))

It adds a configuration to set another timeout than the default one (2ms) for redis requests. It also change the default timeout to 2ms (previously set to 1ms)

Example for APQ:
```yaml
apq:
  router:
    cache:
      redis:
        urls: ["redis://..."] 
        timeout: 5ms
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3817