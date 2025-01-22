### Limit the compute job thread pool size ([PR #6624](https://github.com/apollographql/router/pull/6624))

The router has always observed the APOLLO_ROUTER_NUM_CORES environment variable to restrict the size of the main tokio async job scheduler.

We are now enhancing the compute job thread pool to both respect this environment variable and restrict the number of threads in the pool.

If the environment variable is not set, then the size of the pool is computed as a fraction of the total number of cores that the router has determined are available.

If it is set, then the environment variable is taken as the number of available cores.

From this number, let's call it available, the router then uses the following table to size the compute job thread pool:

/// available: 1     pool size: 1
/// available: 2     pool size: 1
/// available: 3     pool size: 2
/// available: 4     pool size: 3
/// available: 5     pool size: 4
/// ...
/// available: 8     pool size: 7
/// available: 9     pool size: 7
/// ...
/// available: 16    pool size: 14
/// available: 17    pool size: 14
/// ...
/// available: 32    pool size: 28
/// etc...

This table should not be relied upon as an explicit interface, since it may change in the future, but is provided here for informational purposes.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/6624