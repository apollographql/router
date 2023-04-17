### use jemalloc on linux

Detailed memory investigations of the router in use have revealed that there is a significant amount of memory fragmentation when using the default allocator, glibc, on linux. Performance testing and flamegraph analysis suggests that jemalloc on linux can yield significant performance improvements. In our tests, this figure shows performance to be about 35% faster than the default allocator. The improvement in performance being due to less time spent managing memory fragmentation.

Not everyone will see a 35% performance improvement in this release of the router. Depending on your usage pattern, you may see more or less than this. If you see a regression, please file an issue with details.

We have no reason to believe that there are allocation problems on other platforms, so this change is confined to linux.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2882
