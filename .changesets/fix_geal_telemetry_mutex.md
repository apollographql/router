### use a parking-lot Mutex and RwLock in telemetry ([Issue #2751](https://github.com/apollographql/router/issues/2751))

Parts of telemetry require synchronized access to some elements, and precedently we used futures aware mutexes and read-write locks, but those are susceptible to contention. This replaces that mutex with an atomic, and the read-write lock with a parking-lot implementation that is much faster.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2884
