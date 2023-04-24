### Time-based forced hot-reload for chaos testing

The Router can now artificially be made to hot-reload (as if the configuration or schema had changed) at a configured time interval. This can help reproduce issues like reload-related memory leaks.

The new configuration section for chaos testing is marked as experimental:

```yaml
experimental_chaos:
    force_hot_reload: 1m
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2988
