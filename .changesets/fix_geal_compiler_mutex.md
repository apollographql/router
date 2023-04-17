### use a parking-lot mutex for the query compiler access ([Issue #2751](https://github.com/apollographql/router/issues/2751))

Access to the compiler is protected by a mutex to make it shareable between threads. Precedently we used a futures aware mutex, but those are susceptible to contention. This replaces that mutex with a parking-lot synchronous mutex that is much faster.

This also simplifies the logic around compiler initialization, removing the need for once-cell.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2883
