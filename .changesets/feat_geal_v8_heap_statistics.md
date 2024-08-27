### Add V8 heap usage metrics ([PR #5781](https://github.com/apollographql/router/pull/5781))

The router supports new gauge metrics for tracking heap memory usage of the V8 Javascript engine:
- `apollo.router.v8.heap.used`: heap memory used by V8, in bytes
- `apollo.router.v8.heap.total`: total heap allocated by V8, in bytes

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5781