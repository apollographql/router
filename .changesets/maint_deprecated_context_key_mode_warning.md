### Warn at startup when coprocessor uses deprecated 1.x context key mode ([PR #9632](https://github.com/apollographql/router/pull/9632))

The coprocessor plugin now emits a startup deprecation warning when `context: deprecated` is configured, in addition to the existing warning for the legacy boolean form `context: true`. Both forms opt into 1.x context key names and should be migrated to use `context: all` or selective context keys with current 2.x key names.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9632
