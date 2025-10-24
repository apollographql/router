### Avoid queueing and logging repeated config/schema reloads ([PR #8336](https://github.com/apollographql/router/pull/8336))

A file watch event during an existing hot reload will no longer spam the logs. It will hot reload as usual after the
existing reload has finished.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/8336
