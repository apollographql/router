### Handle both `deprecated` enum values when merging coprocessor context ([PR #8913](https://github.com/apollographql/router/pull/8913))

A change to coprocessor context merges in Router v2.10 caused keys to be deleted when `context: true` is used as the coprocessor context selector in the router configuration file.

The workaround was to pass `context: deprecated` instead. This change brings parity when `context: true` is provided.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8913
