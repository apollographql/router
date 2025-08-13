### Enable jemalloc on MacOS ([PR #8046](https://github.com/apollographql/router/pull/8046))

This PR enables the jemalloc allocator on MacOS by default. Previously, this was only done for Linux. We're making this change because it will make memory profiling easier than it currently is.
