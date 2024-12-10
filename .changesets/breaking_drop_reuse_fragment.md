### Drop experimental reuse fragment query optimization option ([PR #6353](https://github.com/apollographql/router/pull/6353))

Drop support for the experimental reuse fragment query optimization. This implementation was not only very slow but also very buggy due to its complexity.

Auto generation of fragments is a much simpler (and faster) algorithm that in most cases produces better results. Fragment auto generation is the default optimization since v1.58 release.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/6353
