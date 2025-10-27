### Reduce config and schema reload log noise ([PR #8336](https://github.com/apollographql/router/pull/8336))

File watch events during an existing hot reload no longer spam the logs. Hot reload continues as usual after the existing reload finishes.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/8336
