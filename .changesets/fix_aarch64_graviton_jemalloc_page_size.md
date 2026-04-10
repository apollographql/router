### Prevent startup crash on ARM64 Linux hosts with kernel page sizes larger than 4 KiB ([Issue #3382](https://github.com/apollographql/router/issues/3382))

The published `aarch64-unknown-linux-gnu` binary crashed on startup on AWS Graviton and other ARM64 Linux systems whose kernels use page sizes larger than 4 KiB, with the error `jemalloc: Unsupported system page size`. jemalloc was compiled with a hardcoded 4 KiB assumption and validates at startup that the compiled value doesn't exceed the system's actual page size — on Graviton (16 KiB pages), this check fails immediately.

The `aarch64-unknown-linux-gnu` binary now compiles jemalloc with 64 KiB page granularity, making it compatible with all ARM64 Linux kernel page sizes (4 KiB, 16 KiB, and 64 KiB). ARM64 Linux deployments on systems with 4 KiB pages may see a modest increase in baseline memory use. All other targets are unaffected.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TBD
