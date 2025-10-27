### Connection shutdown race condition during hot reload ([PR #8169](https://github.com/apollographql/router/pull/8169))

The router now reliably terminates all connections during hot reload, preventing out-of-memory errors from multiple active pipelines.

A race condition during hot reload occasionally left connections in an active state instead of terminating. Connections that are opening during shutdown now immediately terminate, maintaining stable memory usage through hot reloads.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8169
