### add metrics tracking the V8 heap usage ([PR #5781](https://github.com/apollographql/router/pull/5781))

We add new gauge metrics tracking V8 memory usage:
- `apollo.router.v8.heap.used`: heap memory used by V8, in bytes
- `apollo.router.v8.heap.total`: total heap allocated by V8, in bytes

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5781