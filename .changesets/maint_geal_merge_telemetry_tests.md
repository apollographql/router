### Merge tests to reduce linking time ([PR #3272](https://github.com/apollographql/router/pull/3272))

We build multiple test executables to perform short tests and each of them needs to link an entire router. By merging them in larger files, we can reduce the time spent in CI

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3272