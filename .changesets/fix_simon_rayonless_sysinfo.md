### Fix increased memory usage in `sysinfo` since Router 1.59.0 ([PR #6634](https://github.com/apollographql/router/pull/6634))

In version 1.59.0, Apollo Router started using the `sysinfo` crate to gather metrics about available CPUs and RAM. By default, that crate uses `rayon` internally to parallelize its handling of system processes. In turn, rayon creates a pool of long-lived threads.

In a particular benchmark on a 32-core Linux server, this caused resident memory use to increase by about 150 MB. This is likely a combination of stack space (which only gets freed when the thread terminates) and per-thread space reserved by the heap allocator to reduce cross-thread synchronization cost.

This regression is now fixed by:

* Disabling `sysinfo`’s use of `rayon`, so the thread pool is not created and system processes information is gathered in a sequential loop.
* Making `sysinfo` not gather that information in the first place since Router does not use it.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6634
